//! Provider-neutral mapped channel and notification adapter.

use std::time::Duration;

use nwipc_atomic::{mapped_consumer, mapped_producer};
use nwipc_capabilities::TransportCapabilities;
use nwipc_channel_core::{
    ChannelConfig, ChannelEndpoint, ChannelEvent, ChannelSend, ControlRecord,
};
use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_layout::{REGION_HEADER_SIZE, RegionLayout};
use nwipc_memory_api::{MappedRegion, MappingAccess};
use nwipc_protocol::NegotiatedProtocol;
use nwipc_signal_api::{SignalListener, SignalSender};
use nwipc_signal_hybrid::HybridSignal;
use nwipc_signal_poll::{AdaptivePoller, PollConfig};
use nwipc_validation::{RegionExpectation, Validator};

/// Writes immutable layout metadata and zero cursors into a newly created mapping.
///
/// # Errors
///
/// Rejects read-only or length-mismatched mappings and propagates provider failures.
pub fn initialize_region(
    mapping: &mut impl MappedRegion,
    layout: RegionLayout,
) -> Result<(), ErrorReport> {
    let expected_len = usize::try_from(layout.total_length())
        .map_err(|_| transport_error(ErrorCode::InvalidRange, "initialize region length"))?;
    if mapping.access() != MappingAccess::ReadWrite || mapping.len() != expected_len {
        return Err(transport_error(
            ErrorCode::InvalidRange,
            "initialize mapped region",
        ));
    }
    let mut header = vec![0; REGION_HEADER_SIZE];
    layout.encode(&mut header)?;
    mapping.write(0, &header)
}

/// Negotiated channel limits derived from the common protocol selection.
///
/// # Errors
///
/// Rejects missing production capabilities or an empty negotiated message range.
pub fn negotiated_channel_config(
    capacity: u32,
    layout_inline_limit: u32,
    low_watermark: u32,
    high_watermark: u32,
    negotiated: NegotiatedProtocol,
) -> Result<ChannelConfig, ErrorReport> {
    let capabilities = negotiated.capabilities.capabilities();
    let required = TransportCapabilities::SHARED_MEMORY_DATA_PLANE
        .union(TransportCapabilities::BINARY_MESSAGES)
        .union(TransportCapabilities::BOUNDED_BACKPRESSURE);
    if !capabilities.contains(required) {
        return Err(transport_error(
            ErrorCode::RequiredCapabilityMissing,
            "production channel capabilities",
        ));
    }
    let maximum_inline_message = layout_inline_limit.min(negotiated.maximum_message);
    if maximum_inline_message == 0 {
        return Err(transport_error(
            ErrorCode::InvalidRange,
            "negotiated inline message limit",
        ));
    }
    let maximum_message = if capabilities.contains(TransportCapabilities::FRAGMENTATION) {
        negotiated.maximum_message
    } else {
        maximum_inline_message
    };
    Ok(ChannelConfig {
        capacity,
        maximum_inline_message,
        maximum_message,
        low_watermark,
        high_watermark,
    })
}

/// Attaches opposite-direction mappings to one endpoint after common validation.
///
/// # Errors
///
/// Rejects malformed layout metadata, stale identity, direction mismatch, or invalid negotiation.
#[allow(clippy::too_many_arguments)]
pub fn attach_mapped_endpoint<Outbound, Inbound>(
    outbound: Outbound,
    inbound: Inbound,
    outbound_expectation: RegionExpectation,
    inbound_expectation: RegionExpectation,
    negotiated: NegotiatedProtocol,
    low_watermark: u32,
    high_watermark: u32,
) -> Result<ChannelEndpoint, ErrorReport>
where
    Outbound: MappedRegion,
    Inbound: MappedRegion,
{
    if outbound_expectation.owner == inbound_expectation.owner
        || outbound_expectation.session_id != inbound_expectation.session_id
        || outbound_expectation.generation != inbound_expectation.generation
    {
        return Err(transport_error(
            ErrorCode::ProtocolViolation,
            "mapped channel direction pair",
        ));
    }
    let outbound_layout = validate_mapping(&outbound, outbound_expectation)?;
    let inbound_layout = validate_mapping(&inbound, inbound_expectation)?;
    if outbound_layout.capacity() != inbound_layout.capacity()
        || outbound_layout.maximum_inline_message() != inbound_layout.maximum_inline_message()
    {
        return Err(transport_error(
            ErrorCode::InvalidRange,
            "mapped channel layout pair",
        ));
    }
    let config = negotiated_channel_config(
        outbound_layout.capacity(),
        outbound_layout.maximum_inline_message(),
        low_watermark,
        high_watermark,
        negotiated,
    )?;
    ChannelEndpoint::from_memories(
        mapped_producer(outbound, config.capacity)?,
        mapped_consumer(inbound, config.capacity)?,
        config,
    )
}

fn validate_mapping(
    mapping: &impl MappedRegion,
    expectation: RegionExpectation,
) -> Result<RegionLayout, ErrorReport> {
    let mut header = vec![0; REGION_HEADER_SIZE];
    mapping.read(0, &mut header)?;
    Validator::new().region_layout(&header, mapping.len(), expectation)
}

/// Endpoint event including the local writable-return edge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportEvent {
    /// One complete application message.
    Message(Vec<u8>),
    /// Graceful remote close.
    Closed,
    /// Immediate remote reset.
    Reset,
    /// Local outbound capacity crossed below its low watermark.
    Writable,
    /// Preserved control record.
    Control(ControlRecord),
}

/// A mapped channel combined with primary notification hints and correctness polling.
pub struct ChannelTransport<Sender, Listener> {
    channel: ChannelEndpoint,
    outbound_sender: Sender,
    inbound_sender: Sender,
    outbound_wake: HybridSignal<Listener>,
    inbound_wake: HybridSignal<Listener>,
}

impl<Sender, Listener> ChannelTransport<Sender, Listener>
where
    Sender: SignalSender,
    Listener: SignalListener,
{
    /// Creates the common signal adapter for fake or platform providers.
    ///
    /// # Errors
    ///
    /// Rejects invalid polling bounds.
    pub fn new(
        channel: ChannelEndpoint,
        outbound_sender: Sender,
        outbound_listener: Listener,
        inbound_sender: Sender,
        inbound_listener: Listener,
        poll_config: PollConfig,
    ) -> Result<Self, ErrorReport> {
        Ok(Self {
            channel,
            outbound_sender,
            inbound_sender,
            outbound_wake: HybridSignal::new(outbound_listener, AdaptivePoller::new(poll_config)?),
            inbound_wake: HybridSignal::new(inbound_listener, AdaptivePoller::new(poll_config)?),
        })
    }

    /// Sends a complete message and posts an empty-to-non-empty hint when useful.
    ///
    /// # Errors
    ///
    /// Propagates channel and signal-provider failures.
    pub fn send(&mut self, payload: &[u8]) -> Result<ChannelSend, ErrorReport> {
        let sent = self.channel.send(payload)?;
        self.notify_outbound(sent)?;
        Ok(sent)
    }

    /// Sends a FIFO close marker and notifies the remote consumer.
    ///
    /// # Errors
    ///
    /// Propagates channel and signal-provider failures.
    pub fn close(&mut self) -> Result<(), ErrorReport> {
        let sent = self.channel.close()?;
        self.notify_outbound(sent)
    }

    /// Sends an immediate reset marker and notifies the remote consumer.
    ///
    /// # Errors
    ///
    /// Propagates channel and signal-provider failures.
    pub fn reset(&mut self) -> Result<(), ErrorReport> {
        let sent = self.channel.reset()?;
        self.notify_outbound(sent)
    }

    /// Inspects shared state without relying on a delivered notification.
    ///
    /// # Errors
    ///
    /// Propagates shared-state validation and signal-provider failures.
    pub fn poll(&mut self) -> Result<Option<TransportEvent>, ErrorReport> {
        if let Some(event) = self.channel.receive()? {
            self.inbound_sender.notify()?;
            return Ok(Some(channel_event(event)));
        }
        let flow = self.channel.refresh_flow()?;
        Ok(flow.became_writable.then_some(TransportEvent::Writable))
    }

    /// Returns committed bytes which have not yet been consumed by the remote endpoint.
    ///
    /// # Errors
    ///
    /// Propagates shared cursor validation failures.
    pub fn buffered_amount(&self) -> Result<u32, ErrorReport> {
        self.channel.buffered_amount()
    }

    /// Waits for a primary hint or bounded correctness poll, then uses the common drain path.
    ///
    /// # Errors
    ///
    /// Propagates channel and signal-provider failures.
    pub fn wait_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<TransportEvent>, ErrorReport> {
        if let Some(event) = self.poll()? {
            return Ok(Some(event));
        }
        let inbound_source = self.inbound_wake.wait_timeout(timeout)?;
        let outbound_source = self.outbound_wake.wait_timeout(Duration::ZERO)?;
        let event = self.poll()?;
        let progressed = event.is_some();
        self.inbound_wake.record_drain(inbound_source, progressed);
        self.outbound_wake.record_drain(outbound_source, progressed);
        Ok(event)
    }

    /// Cancels both direction-specific listeners. Cancellation is idempotent.
    pub fn cancel(&mut self) {
        self.outbound_wake.cancel();
        self.inbound_wake.cancel();
    }

    fn notify_outbound(&self, sent: ChannelSend) -> Result<(), ErrorReport> {
        if sent.notify {
            self.outbound_sender.notify()?;
        }
        Ok(())
    }
}

fn channel_event(event: ChannelEvent) -> TransportEvent {
    match event {
        ChannelEvent::Message(payload) => TransportEvent::Message(payload),
        ChannelEvent::Closed => TransportEvent::Closed,
        ChannelEvent::Reset => TransportEvent::Reset,
        ChannelEvent::Control(record) => TransportEvent::Control(record),
    }
}

fn transport_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        if code == ErrorCode::RequiredCapabilityMissing {
            ErrorCategory::Protocol
        } else {
            ErrorCategory::Configuration
        },
        code,
        Recoverability::ReplaceEndpoint,
        operation,
    )
}

#[cfg(test)]
mod tests {
    use nwipc_capabilities::NegotiatedCapabilities;
    use nwipc_error::ErrorCode;
    use nwipc_layout::OwnerRole;
    use nwipc_memory_api::{MappingAccess, SharedMemoryProvider};
    use nwipc_protocol::CURRENT_VERSION;
    use nwipc_signal_api::{SignalDirection, SignalListener, SignalSender};
    use nwipc_testkit::{
        FakeMemoryProvider, FakeSignalListener, FakeSignalMode, FakeSignalSender, fake_signal_pair,
    };
    use nwipc_types::{Generation, SessionId};

    use super::*;

    const CAPACITY: u32 = 1024;
    const INLINE: u32 = 32;
    const MAXIMUM_MESSAGE: u32 = 80;
    type FakeTransport = ChannelTransport<FakeSignalSender, FakeSignalListener>;

    fn session_id() -> SessionId {
        SessionId::from_bytes([7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]).unwrap()
    }

    fn generation() -> Generation {
        Generation::new(4).unwrap()
    }

    fn negotiated() -> NegotiatedProtocol {
        let capabilities = TransportCapabilities::SHARED_MEMORY_DATA_PLANE
            .union(TransportCapabilities::BINARY_MESSAGES)
            .union(TransportCapabilities::BOUNDED_BACKPRESSURE)
            .union(TransportCapabilities::DIRECT_SIGNAL)
            .union(TransportCapabilities::FRAGMENTATION);
        NegotiatedProtocol {
            version: CURRENT_VERSION,
            capabilities: NegotiatedCapabilities::new(capabilities),
            maximum_message: MAXIMUM_MESSAGE,
        }
    }

    fn mapped_endpoints<Provider>(
        provider: &Provider,
    ) -> Result<(ChannelEndpoint, ChannelEndpoint), ErrorReport>
    where
        Provider: SharedMemoryProvider,
    {
        let total_length = REGION_HEADER_SIZE + CAPACITY as usize;
        let renderer_layout = RegionLayout::new(
            session_id(),
            generation(),
            OwnerRole::Renderer,
            total_length as u64,
            INLINE,
        )?;
        let peer_layout = RegionLayout::new(
            session_id(),
            generation(),
            OwnerRole::Peer,
            total_length as u64,
            INLINE,
        )?;
        let (mut renderer_outbound, renderer_descriptor) =
            provider.create(total_length, generation())?;
        let (mut peer_outbound, peer_descriptor) = provider.create(total_length, generation())?;
        initialize_region(&mut renderer_outbound, renderer_layout)?;
        initialize_region(&mut peer_outbound, peer_layout)?;
        let renderer_inbound =
            provider.attach(&peer_descriptor, generation(), MappingAccess::ReadWrite)?;
        let peer_inbound =
            provider.attach(&renderer_descriptor, generation(), MappingAccess::ReadWrite)?;
        let renderer = attach_mapped_endpoint(
            renderer_outbound,
            renderer_inbound,
            expectation(OwnerRole::Renderer),
            expectation(OwnerRole::Peer),
            negotiated(),
            256,
            768,
        )?;
        let peer = attach_mapped_endpoint(
            peer_outbound,
            peer_inbound,
            expectation(OwnerRole::Peer),
            expectation(OwnerRole::Renderer),
            negotiated(),
            256,
            768,
        )?;
        Ok((renderer, peer))
    }

    fn expectation(owner: OwnerRole) -> RegionExpectation {
        RegionExpectation {
            session_id: session_id(),
            generation: generation(),
            owner,
        }
    }

    fn fake_transports(
        mode: FakeSignalMode,
    ) -> Result<(FakeTransport, FakeTransport), ErrorReport> {
        let (renderer, peer) = mapped_endpoints(&FakeMemoryProvider)?;
        let (renderer_to_peer_sender, renderer_to_peer_listener) = fake_signal_pair(mode);
        let (peer_to_renderer_sender, peer_to_renderer_listener) = fake_signal_pair(mode);
        let poll = PollConfig {
            active: Duration::from_micros(1),
            idle: Duration::from_micros(2),
            maximum: Duration::from_micros(4),
        };
        Ok((
            ChannelTransport::new(
                renderer,
                renderer_to_peer_sender.clone(),
                renderer_to_peer_listener.clone(),
                peer_to_renderer_sender.clone(),
                peer_to_renderer_listener.clone(),
                poll,
            )?,
            ChannelTransport::new(
                peer,
                peer_to_renderer_sender,
                peer_to_renderer_listener,
                renderer_to_peer_sender,
                renderer_to_peer_listener,
                poll,
            )?,
        ))
    }

    fn run_contract<Sender, Listener>(
        renderer: &mut ChannelTransport<Sender, Listener>,
        peer: &mut ChannelTransport<Sender, Listener>,
    ) where
        Sender: SignalSender,
        Listener: SignalListener,
    {
        let exact = vec![0xa5; INLINE as usize];
        let fragmented = (0..MAXIMUM_MESSAGE)
            .map(|value| u8::try_from(value).unwrap())
            .collect::<Vec<_>>();
        for payload in [&[][..], exact.as_slice(), fragmented.as_slice()] {
            renderer.send(payload).unwrap();
        }
        for expected in [Vec::new(), exact, fragmented] {
            assert_eq!(next_event(peer), TransportEvent::Message(expected));
        }

        let payload = [0x5a; INLINE as usize];
        loop {
            match renderer.send(&payload) {
                Ok(_) => {}
                Err(error) if error.code() == ErrorCode::Backpressured => break,
                Err(error) => panic!("unexpected send failure: {error}"),
            }
        }
        while matches!(peer.poll().unwrap(), Some(TransportEvent::Message(_))) {}
        assert_eq!(next_event(renderer), TransportEvent::Writable);

        renderer.close().unwrap();
        assert_eq!(next_event(peer), TransportEvent::Closed);
        peer.reset().unwrap();
        assert_eq!(next_event(renderer), TransportEvent::Reset);
    }

    fn next_event<Sender, Listener>(
        transport: &mut ChannelTransport<Sender, Listener>,
    ) -> TransportEvent
    where
        Sender: SignalSender,
        Listener: SignalListener,
    {
        for _ in 0..20 {
            if let Some(event) = transport.wait_timeout(Duration::from_millis(2)).unwrap() {
                return event;
            }
        }
        panic!("transport made no bounded progress")
    }

    #[test]
    fn fake_provider_passes_production_transport_contract_with_dropped_signals() {
        let (mut renderer, mut peer) = fake_transports(FakeSignalMode::Drop).unwrap();
        run_contract(&mut renderer, &mut peer);
    }

    #[test]
    fn fragmentation_requires_the_negotiated_capability() {
        let capabilities = TransportCapabilities::SHARED_MEMORY_DATA_PLANE
            .union(TransportCapabilities::BINARY_MESSAGES)
            .union(TransportCapabilities::BOUNDED_BACKPRESSURE);
        let config = negotiated_channel_config(
            CAPACITY,
            INLINE,
            256,
            768,
            NegotiatedProtocol {
                version: CURRENT_VERSION,
                capabilities: NegotiatedCapabilities::new(capabilities),
                maximum_message: MAXIMUM_MESSAGE,
            },
        )
        .unwrap();
        assert_eq!(config.maximum_message, INLINE);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn iosurface_and_darwin_pass_the_same_production_transport_contract() {
        use nwipc_memory_iosurface::IoSurfaceProvider;
        use nwipc_signal_darwin::{DarwinSignal, DarwinSignalDescriptor};

        let (renderer, peer) = mapped_endpoints(&IoSurfaceProvider::initialize().unwrap()).unwrap();
        let signal = DarwinSignal::initialize().unwrap();
        let renderer_to_peer = DarwinSignalDescriptor::new(
            session_id(),
            generation(),
            SignalDirection::RendererToPeer,
        );
        let peer_to_renderer = DarwinSignalDescriptor::new(
            session_id(),
            generation(),
            SignalDirection::PeerToRenderer,
        );
        let poll = PollConfig::default();
        let mut renderer = ChannelTransport::new(
            renderer,
            signal.sender(&renderer_to_peer, generation()).unwrap(),
            signal.listener(&renderer_to_peer, generation()).unwrap(),
            signal.sender(&peer_to_renderer, generation()).unwrap(),
            signal.listener(&peer_to_renderer, generation()).unwrap(),
            poll,
        )
        .unwrap();
        let mut peer = ChannelTransport::new(
            peer,
            signal.sender(&peer_to_renderer, generation()).unwrap(),
            signal.listener(&peer_to_renderer, generation()).unwrap(),
            signal.sender(&renderer_to_peer, generation()).unwrap(),
            signal.listener(&renderer_to_peer, generation()).unwrap(),
            poll,
        )
        .unwrap();
        run_contract(&mut renderer, &mut peer);
    }
}
