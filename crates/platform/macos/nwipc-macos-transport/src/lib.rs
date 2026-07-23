//! macOS provider factory for the common mapped production channel.

use std::collections::VecDeque;
use std::time::Duration;

use nwipc_bootstrap_schema::{BootstrapEnvelope, EndpointRole, OpaqueDescriptor, ProviderKind};
use nwipc_capabilities::{NegotiatedCapabilities, TransportCapabilities};
use nwipc_channel_core::ChannelSend;
use nwipc_channel_transport::{
    ChannelTransport, TransportEvent as ChannelEvent, attach_mapped_endpoint, initialize_region,
};
use nwipc_crypto::{EndpointProtection, EndpointRole as CryptoEndpointRole, FRAME_OVERHEAD};
use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_layout::{OwnerRole, REGION_HEADER_SIZE, RegionLayout};
use nwipc_memory_api::{MappingAccess, SharedMemoryProvider};
use nwipc_memory_iosurface::{IoSurfaceDescriptor, IoSurfaceMapping, IoSurfaceProvider};
use nwipc_peer_core::PortTransport;
use nwipc_protocol::{
    EndpointRole as ProtocolEndpointRole, HandshakeIdentity, InitiatorConfig, InitiatorHandshake,
    NegotiatedProtocol, ProtocolVersion, VersionRange,
};
use nwipc_renderer_api::{
    RendererTransport, SendDisposition, TransportDiagnostics as RendererTransportDiagnostics,
    TransportEvent as RendererEvent,
};
use nwipc_renderer_bootstrap::RendererTransportFactory;
use nwipc_signal_api::{SignalDirection, SignalSender};
use nwipc_signal_darwin::{DarwinListener, DarwinSender, DarwinSignal, DarwinSignalDescriptor};
use nwipc_signal_poll::PollConfig;
use nwipc_types::{Generation, SessionId};
use nwipc_validation::RegionExpectation;

const MEMORY_MAGIC: &[u8; 4] = b"NWM1";
const SIGNAL_MAGIC: &[u8; 4] = b"NWS1";
const IOSURFACE_DESCRIPTOR_LENGTH: usize = 20;
const MEMORY_HEADER_LENGTH: usize = 24;

/// Probes both production providers without allocating a session.
///
/// # Errors
///
/// Returns explicit `Unsupported` when the current platform cannot attach production resources.
pub fn ensure_available() -> Result<(), ErrorReport> {
    IoSurfaceProvider::initialize()?;
    DarwinSignal::initialize()?;
    Ok(())
}

/// Capacity and watermark policy encoded with a pair of production descriptors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelConfiguration {
    /// Ring bytes in each direction.
    pub capacity: u32,
    /// Largest record payload before fragmentation.
    pub maximum_inline_message: u32,
    /// Largest negotiated logical message.
    pub maximum_message: u32,
    /// Writable edge low watermark.
    pub low_watermark: u32,
    /// Backpressure high watermark.
    pub high_watermark: u32,
}

impl ChannelConfiguration {
    /// Validates limits by constructing the common negotiated channel policy.
    ///
    /// # Errors
    ///
    /// Rejects an empty or internally inconsistent channel configuration.
    pub fn validate(self) -> Result<Self, ErrorReport> {
        let total = usize::try_from(self.capacity)
            .ok()
            .and_then(|capacity| REGION_HEADER_SIZE.checked_add(capacity))
            .ok_or_else(|| platform_error(ErrorCode::InvalidRange, "macOS channel capacity"))?;
        if total <= REGION_HEADER_SIZE
            || self.maximum_inline_message == 0
            || self.maximum_message < self.maximum_inline_message
            || self.maximum_message == u32::MAX
            || self.low_watermark >= self.high_watermark
            || self.high_watermark >= self.capacity
        {
            return Err(platform_error(
                ErrorCode::InvalidRange,
                "macOS channel configuration",
            ));
        }
        Ok(self)
    }
}

impl Default for ChannelConfiguration {
    fn default() -> Self {
        Self {
            capacity: 2 * 1024 * 1024,
            maximum_inline_message: 16 * 1024,
            maximum_message: 1024 * 1024,
            low_watermark: 512 * 1024,
            high_watermark: 1536 * 1024,
        }
    }
}

/// Host-owned mappings and transferable descriptor bundles for one generation.
pub struct PreparedMacosTransport {
    renderer_to_peer: IoSurfaceMapping,
    peer_to_renderer: IoSurfaceMapping,
    memory: Vec<u8>,
    signal: Vec<u8>,
}

impl PreparedMacosTransport {
    /// Creates and initializes both unidirectional regions and signal names.
    ///
    /// # Errors
    ///
    /// Propagates platform allocation and common layout validation failures.
    pub fn prepare(
        session_id: SessionId,
        generation: Generation,
        configuration: ChannelConfiguration,
    ) -> Result<Self, ErrorReport> {
        let configuration = configuration.validate()?;
        let provider = IoSurfaceProvider::initialize()?;
        let total_length =
            REGION_HEADER_SIZE
                .checked_add(usize::try_from(configuration.capacity).map_err(|_| {
                    platform_error(ErrorCode::InvalidRange, "macOS channel capacity")
                })?)
                .ok_or_else(|| platform_error(ErrorCode::InvalidRange, "macOS channel capacity"))?;
        let (mut renderer_to_peer, renderer_descriptor) =
            provider.create(total_length, generation)?;
        let (mut peer_to_renderer, peer_descriptor) = provider.create(total_length, generation)?;
        initialize_region(
            &mut renderer_to_peer,
            RegionLayout::new(
                session_id,
                generation,
                OwnerRole::Renderer,
                u64::try_from(total_length)
                    .map_err(|_| platform_error(ErrorCode::InvalidRange, "macOS channel length"))?,
                configuration.maximum_inline_message,
            )?,
        )?;
        initialize_region(
            &mut peer_to_renderer,
            RegionLayout::new(
                session_id,
                generation,
                OwnerRole::Peer,
                u64::try_from(total_length)
                    .map_err(|_| platform_error(ErrorCode::InvalidRange, "macOS channel length"))?,
                configuration.maximum_inline_message,
            )?,
        )?;
        let memory = encode_memory(configuration, &renderer_descriptor, &peer_descriptor)?;
        let signal = encode_signals(
            &DarwinSignalDescriptor::new(session_id, generation, SignalDirection::RendererToPeer),
            &DarwinSignalDescriptor::new(session_id, generation, SignalDirection::PeerToRenderer),
        )?;
        Ok(Self {
            renderer_to_peer,
            peer_to_renderer,
            memory,
            signal,
        })
    }

    /// Provider-tagged memory descriptor bundle for bootstrap.
    ///
    /// # Errors
    ///
    /// Returns a schema range error if the encoded provider payload is unsupported.
    pub fn memory_descriptor(&self) -> Result<OpaqueDescriptor, ErrorReport> {
        OpaqueDescriptor::new(ProviderKind::IoSurface, self.memory.clone())
    }

    /// Provider-tagged hybrid signal descriptor bundle for bootstrap.
    ///
    /// # Errors
    ///
    /// Returns a schema range error if the encoded provider payload is unsupported.
    pub fn signal_descriptor(&self) -> Result<OpaqueDescriptor, ErrorReport> {
        OpaqueDescriptor::new(ProviderKind::Hybrid, self.signal.clone())
    }

    /// Keeps the two owner mappings live without exposing their native identities.
    pub const fn mapping_count(&self) -> usize {
        let _ = (&self.renderer_to_peer, &self.peer_to_renderer);
        2
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SignalPostBehavior {
    #[default]
    Immediate,
    #[cfg(feature = "fault-injection")]
    Dropped,
    #[cfg(feature = "fault-injection")]
    Duplicate,
    #[cfg(feature = "fault-injection")]
    Delayed,
}

#[derive(Clone, Debug)]
struct E2eDarwinSender {
    inner: DarwinSender,
    behavior: SignalPostBehavior,
}

impl SignalSender for E2eDarwinSender {
    fn notify(&self) -> Result<(), ErrorReport> {
        match self.behavior {
            SignalPostBehavior::Immediate => self.inner.notify(),
            #[cfg(feature = "fault-injection")]
            SignalPostBehavior::Dropped => Ok(()),
            #[cfg(feature = "fault-injection")]
            SignalPostBehavior::Duplicate => {
                self.inner.notify()?;
                self.inner.notify()
            }
            #[cfg(feature = "fault-injection")]
            SignalPostBehavior::Delayed => {
                let sender = self.inner.clone();
                std::thread::Builder::new()
                    .name("nwipc-delayed-notification".into())
                    .spawn(move || {
                        std::thread::sleep(Duration::from_millis(250));
                        let _ = sender.notify();
                    })
                    .map(|_| ())
                    .map_err(|_| {
                        platform_error(ErrorCode::Internal, "schedule delayed notification")
                    })
            }
        }
    }
}

type MacosChannel = ChannelTransport<E2eDarwinSender, DarwinListener>;

/// Raw complete-frame transport attached from a validated bootstrap envelope.
pub struct MacosEndpointTransport {
    channel: MacosChannel,
    protection: EndpointProtection,
    high_watermark: u32,
    pending_inbound: VecDeque<Vec<u8>>,
    remote_closed: bool,
}

impl MacosEndpointTransport {
    /// Attaches `IOSurface` and Darwin resources for the requested endpoint role.
    ///
    /// # Errors
    ///
    /// Rejects mismatched provider tags, malformed bundles, stale resources, and invalid layouts.
    pub fn attach(envelope: &BootstrapEnvelope, role: EndpointRole) -> Result<Self, ErrorReport> {
        Self::attach_with_signal_behavior(envelope, role, SignalPostBehavior::Immediate)
    }

    fn attach_with_signal_behavior(
        envelope: &BootstrapEnvelope,
        role: EndpointRole,
        signal_behavior: SignalPostBehavior,
    ) -> Result<Self, ErrorReport> {
        if envelope.role() != role
            || envelope.memory().provider() != ProviderKind::IoSurface
            || !matches!(
                envelope.signal().provider(),
                ProviderKind::DarwinNotify | ProviderKind::Hybrid
            )
        {
            return Err(platform_error(
                ErrorCode::ProtocolViolation,
                "attach macOS endpoint providers",
            ));
        }
        let (configuration, renderer_descriptor, peer_descriptor) =
            decode_memory(envelope.memory().bytes())?;
        let (renderer_signal, peer_signal) = decode_signals(envelope.signal().bytes())?;
        let generation = envelope.generation();
        let memory = IoSurfaceProvider::initialize()?;
        let (outbound_descriptor, inbound_descriptor, outbound_owner, inbound_owner) = match role {
            EndpointRole::Renderer => (
                &renderer_descriptor,
                &peer_descriptor,
                OwnerRole::Renderer,
                OwnerRole::Peer,
            ),
            EndpointRole::Peer => (
                &peer_descriptor,
                &renderer_descriptor,
                OwnerRole::Peer,
                OwnerRole::Renderer,
            ),
        };
        let outbound = memory.attach(outbound_descriptor, generation, MappingAccess::ReadWrite)?;
        let inbound = memory.attach(inbound_descriptor, generation, MappingAccess::ReadWrite)?;
        let endpoint = attach_mapped_endpoint(
            outbound,
            inbound,
            expectation(envelope, outbound_owner),
            expectation(envelope, inbound_owner),
            negotiated(configuration)?,
            configuration.low_watermark,
            configuration.high_watermark,
        )?;
        let signal = DarwinSignal::initialize()?;
        let (outbound_signal, inbound_signal) = match role {
            EndpointRole::Renderer => (&renderer_signal, &peer_signal),
            EndpointRole::Peer => (&peer_signal, &renderer_signal),
        };
        let channel = ChannelTransport::new(
            endpoint,
            E2eDarwinSender {
                inner: signal.sender(outbound_signal, generation)?,
                behavior: signal_behavior,
            },
            signal.listener(outbound_signal, generation)?,
            E2eDarwinSender {
                inner: signal.sender(inbound_signal, generation)?,
                behavior: SignalPostBehavior::Immediate,
            },
            signal.listener(inbound_signal, generation)?,
            PollConfig::default(),
        )?;
        let crypto_role = match role {
            EndpointRole::Renderer => CryptoEndpointRole::Renderer,
            EndpointRole::Peer => CryptoEndpointRole::Peer,
        };
        let protection = EndpointProtection::derive(
            envelope.secret().expose(),
            envelope.session_id(),
            generation,
            crypto_role,
        )?;
        Ok(Self {
            channel,
            protection,
            high_watermark: configuration.high_watermark,
            pending_inbound: VecDeque::new(),
            remote_closed: false,
        })
    }

    fn send_frame(&mut self, frame: &[u8]) -> Result<ChannelSend, ErrorReport> {
        let pending = self.protection.prepare(frame)?;
        let published = self.channel.send_with_publication(pending.bytes())?;
        pending.commit();
        published.finish()
    }

    #[cfg(feature = "fault-injection")]
    fn prepare_uncommitted_for_crash(&mut self, frame: &[u8]) -> Result<(), ErrorReport> {
        let pending = self.protection.prepare(frame)?;
        self.channel.prepare_uncommitted_for_crash(pending.bytes())
    }

    #[cfg(feature = "fault-injection")]
    fn send_without_notification(&mut self, frame: &[u8]) -> Result<ChannelSend, ErrorReport> {
        let pending = self.protection.prepare(frame)?;
        let sent = self.channel.send_without_notification(pending.bytes())?;
        pending.commit();
        Ok(sent)
    }

    fn open_message(&mut self, protected: &[u8]) -> Result<Vec<u8>, ErrorReport> {
        self.protection.open(protected)
    }

    fn poll_channel(&mut self) -> Result<Option<ChannelEvent>, ErrorReport> {
        match self.channel.poll()? {
            Some(ChannelEvent::Message(protected)) => self
                .open_message(&protected)
                .map(ChannelEvent::Message)
                .map(Some),
            event => Ok(event),
        }
    }

    fn receive_frame(&mut self) -> Result<Option<Vec<u8>>, ErrorReport> {
        if let Some(frame) = self.pending_inbound.pop_front() {
            return Ok(Some(frame));
        }
        if self.remote_closed {
            return Ok(None);
        }
        loop {
            match self.channel.wait_timeout(Duration::from_millis(64))? {
                Some(ChannelEvent::Message(protected)) => {
                    return self.open_message(&protected).map(Some);
                }
                Some(ChannelEvent::Closed | ChannelEvent::Reset) => return Ok(None),
                Some(ChannelEvent::Writable | ChannelEvent::Control(_)) | None => {}
            }
        }
    }
}

impl PortTransport for MacosEndpointTransport {
    fn send(&mut self, frame: &[u8]) -> Result<(), ErrorReport> {
        match self.send_frame(frame) {
            Ok(_) => Ok(()),
            Err(error) if error.code() == ErrorCode::Backpressured => {
                match self.poll_channel()? {
                    Some(ChannelEvent::Message(frame)) => self.pending_inbound.push_back(frame),
                    Some(ChannelEvent::Closed | ChannelEvent::Reset) => self.remote_closed = true,
                    Some(ChannelEvent::Writable | ChannelEvent::Control(_)) | None => {}
                }
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    fn receive(&mut self) -> Result<Option<Vec<u8>>, ErrorReport> {
        if let Some(frame) = self.pending_inbound.pop_front() {
            return Ok(Some(frame));
        }
        if self.remote_closed {
            return Ok(None);
        }
        match self.poll_channel()? {
            Some(ChannelEvent::Message(frame)) => Ok(Some(frame)),
            Some(ChannelEvent::Closed | ChannelEvent::Reset) => {
                self.remote_closed = true;
                Ok(None)
            }
            Some(ChannelEvent::Writable | ChannelEvent::Control(_)) | None => Ok(None),
        }
    }

    fn wait_receive(&mut self) -> Result<Option<Vec<u8>>, ErrorReport> {
        self.receive_frame()
    }

    fn close(&mut self) -> Result<(), ErrorReport> {
        if self.channel.close().is_err() {
            self.channel.reset()?;
        }
        self.channel.cancel();
        Ok(())
    }
}

/// Expected renderer identity selected by the control plane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RendererExpectation {
    /// Expected session identity.
    pub session_id: SessionId,
    /// Expected current resource generation.
    pub generation: Generation,
    /// Selected protocol major version.
    pub protocol: u16,
}

/// Public renderer data-plane adapter after the common `HELLO`/`ACK` handshake.
pub struct MacosRendererTransport {
    raw: MacosEndpointTransport,
    maximum_message: usize,
    closed: bool,
    #[cfg(feature = "fault-injection")]
    crash_point: WriterCrashPoint,
}

/// Stateless runtime adapter for the production renderer endpoint contract.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MacosRendererTransportFactory {
    #[cfg(feature = "fault-injection")]
    faults: FaultInjection,
}

/// Darwin-notification transformations used by the signed process fault matrix.
#[cfg(feature = "fault-injection")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NotificationFault {
    /// Post notifications normally.
    #[default]
    None,
    /// Suppress every renderer-to-peer notification.
    Dropped,
    /// Post every renderer-to-peer notification twice.
    Duplicate,
    /// Post every renderer-to-peer notification after the correctness-poll interval.
    Delayed,
}

/// Writer process termination point used by the signed process crash matrix.
#[cfg(feature = "fault-injection")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WriterCrashPoint {
    /// Do not terminate the writer.
    #[default]
    None,
    /// Terminate after bytes are written but before cursor publication.
    BeforeCommit,
    /// Terminate after cursor publication but before notification.
    AfterCommit,
}

/// Test-only production transport fault selection.
#[cfg(feature = "fault-injection")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FaultInjection {
    /// Notification transformation applied to renderer-to-peer hints.
    pub notification: NotificationFault,
    /// Writer termination point applied to the first application send.
    pub writer_crash: WriterCrashPoint,
}

#[cfg(feature = "fault-injection")]
impl MacosRendererTransportFactory {
    /// Creates a factory for the signed process fault matrix.
    #[doc(hidden)]
    pub const fn with_fault_injection(faults: FaultInjection) -> Self {
        Self { faults }
    }
}

impl RendererTransportFactory for MacosRendererTransportFactory {
    type Transport = MacosRendererTransport;

    fn create(
        &mut self,
        envelope: BootstrapEnvelope,
        session_id: SessionId,
        generation: Generation,
        protocol: u16,
    ) -> Result<Self::Transport, ErrorReport> {
        MacosRendererTransport::attach_with_factory_faults(
            envelope,
            RendererExpectation {
                session_id,
                generation,
                protocol,
            },
            *self,
        )
    }
}

impl MacosRendererTransport {
    /// Attaches providers and completes the renderer-to-peer handshake.
    ///
    /// # Errors
    ///
    /// Rejects stale identity, unsupported versions/capabilities, and malformed acknowledgement.
    pub fn attach(
        envelope: BootstrapEnvelope,
        expectation: RendererExpectation,
    ) -> Result<Self, ErrorReport> {
        Self::attach_with_factory_faults(
            envelope,
            expectation,
            MacosRendererTransportFactory::default(),
        )
    }

    fn attach_with_factory_faults(
        envelope: BootstrapEnvelope,
        expectation: RendererExpectation,
        factory: MacosRendererTransportFactory,
    ) -> Result<Self, ErrorReport> {
        envelope.validate_for(
            EndpointRole::Renderer,
            expectation.session_id,
            expectation.generation,
            expectation.protocol,
        )?;
        #[cfg(feature = "fault-injection")]
        let signal_behavior = match factory.faults.notification {
            NotificationFault::None => SignalPostBehavior::Immediate,
            NotificationFault::Dropped => SignalPostBehavior::Dropped,
            NotificationFault::Duplicate => SignalPostBehavior::Duplicate,
            NotificationFault::Delayed => SignalPostBehavior::Delayed,
        };
        #[cfg(not(feature = "fault-injection"))]
        let signal_behavior = {
            let _ = factory;
            SignalPostBehavior::Immediate
        };
        let mut raw = MacosEndpointTransport::attach_with_signal_behavior(
            &envelope,
            EndpointRole::Renderer,
            signal_behavior,
        )?;
        let major = u8::try_from(expectation.protocol).map_err(|_| {
            platform_error(
                ErrorCode::LayoutVersionMismatch,
                "renderer protocol version",
            )
        })?;
        if major == 0 {
            return Err(platform_error(
                ErrorCode::LayoutVersionMismatch,
                "renderer protocol version",
            ));
        }
        let configuration = decode_memory(envelope.memory().bytes())?.0;
        let mut handshake = InitiatorHandshake::new(InitiatorConfig {
            identity: HandshakeIdentity {
                session_id: expectation.session_id,
                generation: expectation.generation,
                role: ProtocolEndpointRole::Renderer,
            },
            remote_role: ProtocolEndpointRole::Peer,
            versions: VersionRange::exact(ProtocolVersion::new(major, 0)),
            requested: nwipc_capabilities::RequestedCapabilities::new(production_capabilities()),
            required: nwipc_capabilities::RequiredCapabilities::new(production_capabilities()),
            maximum_message: configuration.maximum_message,
            proof: envelope.secret().expose().to_vec(),
        })?;
        raw.send_frame(&handshake.hello()?)?;
        let acknowledgement = raw.receive_frame()?.ok_or_else(|| {
            platform_error(ErrorCode::Timeout, "renderer handshake acknowledgement")
        })?;
        let negotiated = handshake.acknowledge(&acknowledgement)?;
        drop(envelope);
        Ok(Self {
            raw,
            maximum_message: usize::try_from(negotiated.maximum_message).map_err(|_| {
                platform_error(ErrorCode::InvalidRange, "renderer negotiated message limit")
            })?,
            closed: false,
            #[cfg(feature = "fault-injection")]
            crash_point: factory.faults.writer_crash,
        })
    }
}

impl RendererTransport for MacosRendererTransport {
    fn send(&mut self, payload: &[u8]) -> Result<SendDisposition, ErrorReport> {
        if self.closed {
            return Err(platform_error(ErrorCode::Closed, "renderer send"));
        }
        if payload.len() > self.maximum_message {
            return Err(platform_error(ErrorCode::MessageTooLarge, "renderer send"));
        }
        let mut frame = Vec::with_capacity(payload.len() + 1);
        frame.push(0);
        frame.extend_from_slice(payload);
        #[cfg(feature = "fault-injection")]
        match std::mem::replace(&mut self.crash_point, WriterCrashPoint::None) {
            WriterCrashPoint::BeforeCommit => {
                self.raw.prepare_uncommitted_for_crash(&frame)?;
                std::process::abort();
            }
            WriterCrashPoint::AfterCommit => {
                self.raw.send_without_notification(&frame)?;
                std::process::abort();
            }
            WriterCrashPoint::None => {}
        }
        self.raw.send_frame(&frame).map(|sent| {
            if sent.buffered_amount >= self.raw.high_watermark {
                SendDisposition::Backpressured
            } else {
                SendDisposition::Sent
            }
        })
    }

    fn buffered_amount(&self) -> Result<u32, ErrorReport> {
        self.raw.channel.buffered_amount()
    }

    fn poll(&mut self) -> Result<Option<RendererEvent>, ErrorReport> {
        match self.raw.poll_channel()? {
            Some(ChannelEvent::Message(frame)) => match frame.split_first() {
                Some((&0, payload)) if payload.len() <= self.maximum_message => {
                    Ok(Some(RendererEvent::Message(payload.to_vec())))
                }
                Some((&1, [])) => {
                    self.closed = true;
                    Ok(Some(RendererEvent::Closed))
                }
                _ => Err(platform_error(
                    ErrorCode::ProtocolViolation,
                    "renderer receive frame",
                )),
            },
            Some(ChannelEvent::Writable) => Ok(Some(RendererEvent::Writable)),
            Some(ChannelEvent::Closed | ChannelEvent::Reset) => {
                self.closed = true;
                Ok(Some(RendererEvent::Closed))
            }
            Some(ChannelEvent::Control(_)) | None => Ok(None),
        }
    }

    fn close(&mut self) -> Result<(), ErrorReport> {
        if self.closed {
            return Ok(());
        }
        self.raw.send_frame(&[1])?;
        self.raw.channel.close()?;
        self.closed = true;
        Ok(())
    }

    fn diagnostics(&self) -> RendererTransportDiagnostics {
        let diagnostics = self.raw.channel.diagnostics();
        RendererTransportDiagnostics {
            primary_wakeups: diagnostics.primary_wakeups,
            polling_wakeups: diagnostics.polling_wakeups,
            coalesced_wakeups: diagnostics.coalesced_wakeups,
            polling_recoveries: diagnostics.polling_recoveries,
            signal_failures: diagnostics.signal_failures,
        }
    }
}

fn expectation(envelope: &BootstrapEnvelope, owner: OwnerRole) -> RegionExpectation {
    RegionExpectation {
        session_id: envelope.session_id(),
        generation: envelope.generation(),
        owner,
    }
}

fn negotiated(configuration: ChannelConfiguration) -> Result<NegotiatedProtocol, ErrorReport> {
    let frame_overhead = u32::try_from(FRAME_OVERHEAD)
        .map_err(|_| platform_error(ErrorCode::InvalidRange, "frame protection overhead"))?;
    Ok(NegotiatedProtocol {
        version: ProtocolVersion::new(1, 0),
        capabilities: NegotiatedCapabilities::new(production_capabilities()),
        maximum_message: configuration
            .maximum_message
            .checked_add(1)
            .and_then(|maximum| maximum.checked_add(frame_overhead))
            .ok_or_else(|| platform_error(ErrorCode::InvalidRange, "macOS framed message limit"))?,
    })
}

/// Capabilities guaranteed by the macOS production adapter.
pub const fn production_capabilities() -> TransportCapabilities {
    TransportCapabilities::SHARED_MEMORY_DATA_PLANE
        .union(TransportCapabilities::BINARY_MESSAGES)
        .union(TransportCapabilities::BOUNDED_BACKPRESSURE)
        .union(TransportCapabilities::DIRECT_SIGNAL)
        .union(TransportCapabilities::AUTHENTICATED_ENCRYPTION)
        .union(TransportCapabilities::FRAGMENTATION)
}

fn encode_memory(
    configuration: ChannelConfiguration,
    renderer: &IoSurfaceDescriptor,
    peer: &IoSurfaceDescriptor,
) -> Result<Vec<u8>, ErrorReport> {
    let mut output = Vec::with_capacity(MEMORY_HEADER_LENGTH + IOSURFACE_DESCRIPTOR_LENGTH * 2);
    output.extend_from_slice(MEMORY_MAGIC);
    for value in [
        configuration.capacity,
        configuration.maximum_inline_message,
        configuration.maximum_message,
        configuration.low_watermark,
        configuration.high_watermark,
    ] {
        output.extend_from_slice(&value.to_le_bytes());
    }
    output.extend_from_slice(&renderer.encode()?);
    output.extend_from_slice(&peer.encode()?);
    Ok(output)
}

fn decode_memory(
    input: &[u8],
) -> Result<
    (
        ChannelConfiguration,
        IoSurfaceDescriptor,
        IoSurfaceDescriptor,
    ),
    ErrorReport,
> {
    if input.len() != MEMORY_HEADER_LENGTH + IOSURFACE_DESCRIPTOR_LENGTH * 2
        || &input[..4] != MEMORY_MAGIC
    {
        return Err(platform_error(
            ErrorCode::Truncated,
            "decode macOS memory bundle",
        ));
    }
    let read = |offset| {
        input
            .get(offset..offset + 4)
            .and_then(|bytes| bytes.try_into().ok())
            .map(u32::from_le_bytes)
            .ok_or_else(|| platform_error(ErrorCode::Truncated, "decode macOS memory bundle"))
    };
    let configuration = ChannelConfiguration {
        capacity: read(4)?,
        maximum_inline_message: read(8)?,
        maximum_message: read(12)?,
        low_watermark: read(16)?,
        high_watermark: read(20)?,
    }
    .validate()?;
    let renderer = IoSurfaceDescriptor::decode(&input[24..44])?;
    let peer = IoSurfaceDescriptor::decode(&input[44..64])?;
    Ok((configuration, renderer, peer))
}

fn encode_signals(
    renderer: &DarwinSignalDescriptor,
    peer: &DarwinSignalDescriptor,
) -> Result<Vec<u8>, ErrorReport> {
    let renderer = renderer.encode();
    let peer = peer.encode();
    let renderer_length = u16::try_from(renderer.len())
        .map_err(|_| platform_error(ErrorCode::InvalidRange, "encode macOS signal bundle"))?;
    let peer_length = u16::try_from(peer.len())
        .map_err(|_| platform_error(ErrorCode::InvalidRange, "encode macOS signal bundle"))?;
    let mut output = Vec::with_capacity(8 + renderer.len() + peer.len());
    output.extend_from_slice(SIGNAL_MAGIC);
    output.extend_from_slice(&renderer_length.to_le_bytes());
    output.extend_from_slice(&peer_length.to_le_bytes());
    output.extend_from_slice(&renderer);
    output.extend_from_slice(&peer);
    Ok(output)
}

fn decode_signals(
    input: &[u8],
) -> Result<(DarwinSignalDescriptor, DarwinSignalDescriptor), ErrorReport> {
    if input.len() < 8 || &input[..4] != SIGNAL_MAGIC {
        return Err(platform_error(
            ErrorCode::Truncated,
            "decode macOS signal bundle",
        ));
    }
    let renderer_length = usize::from(u16::from_le_bytes([input[4], input[5]]));
    let peer_length = usize::from(u16::from_le_bytes([input[6], input[7]]));
    let renderer_end = 8_usize
        .checked_add(renderer_length)
        .ok_or_else(|| platform_error(ErrorCode::InvalidRange, "decode macOS signal bundle"))?;
    let peer_end = renderer_end
        .checked_add(peer_length)
        .ok_or_else(|| platform_error(ErrorCode::InvalidRange, "decode macOS signal bundle"))?;
    if peer_end != input.len() {
        return Err(platform_error(
            ErrorCode::Truncated,
            "decode macOS signal bundle",
        ));
    }
    Ok((
        DarwinSignalDescriptor::decode(&input[8..renderer_end])?,
        DarwinSignalDescriptor::decode(&input[renderer_end..peer_end])?,
    ))
}

fn platform_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        match code {
            ErrorCode::Closed => ErrorCategory::Closed,
            ErrorCode::MessageTooLarge | ErrorCode::Backpressured => ErrorCategory::Resource,
            ErrorCode::ProtocolViolation
            | ErrorCode::LayoutVersionMismatch
            | ErrorCode::RequiredCapabilityMissing => ErrorCategory::Protocol,
            ErrorCode::Timeout => ErrorCategory::Timeout,
            _ => ErrorCategory::Configuration,
        },
        code,
        if code == ErrorCode::Backpressured {
            Recoverability::Retryable
        } else {
            Recoverability::ReplaceEndpoint
        },
        operation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_rejects_invalid_watermarks() {
        let invalid = ChannelConfiguration {
            low_watermark: 200,
            high_watermark: 100,
            ..ChannelConfiguration::default()
        };
        assert_eq!(
            invalid.validate().unwrap_err().code(),
            ErrorCode::InvalidRange
        );
    }

    #[test]
    fn malformed_descriptor_bundles_fail_closed() {
        assert!(decode_memory(b"NWM1").is_err());
        assert!(decode_signals(b"NWS1").is_err());
    }

    #[test]
    fn production_requires_authenticated_encryption() {
        assert!(
            production_capabilities().contains(TransportCapabilities::AUTHENTICATED_ENCRYPTION)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn prepared_transport_exposes_iosurface_and_darwin_provider_tags() {
        let prepared = PreparedMacosTransport::prepare(
            SessionId::from_u128(7).unwrap(),
            Generation::new(3).unwrap(),
            ChannelConfiguration::default(),
        )
        .unwrap();

        assert_eq!(
            prepared.memory_descriptor().unwrap().provider(),
            ProviderKind::IoSurface
        );
        assert_eq!(
            prepared.signal_descriptor().unwrap().provider(),
            ProviderKind::Hybrid
        );
        assert_eq!(prepared.mapping_count(), 2);
    }
}
