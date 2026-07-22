//! Runtime-neutral synchronous native peer port.

use nwipc_bootstrap_schema::{BootstrapEnvelope, EndpointRole, ProviderKind};
use nwipc_capabilities::TransportCapabilities;
use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_types::{Generation, SessionId};

const HELLO_MAGIC: &[u8; 4] = b"NWPH";
const ACK_MAGIC: &[u8; 4] = b"NWPA";
const CLOSE_FRAME: &[u8] = &[1];
const DATA_KIND: u8 = 0;

/// Identity and protocol selected by the parent control plane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeerExpectation {
    /// Expected session identity.
    pub session_id: SessionId,
    /// Expected current resource generation.
    pub generation: Generation,
    /// Selected protocol version.
    pub protocol: u16,
}

/// Minimal framed transport supplied by a provider adapter.
pub trait PortTransport {
    /// Sends one complete transport frame.
    ///
    /// # Errors
    ///
    /// Returns a provider-specific transport failure.
    fn send(&mut self, frame: &[u8]) -> Result<(), ErrorReport>;
    /// Receives one frame, or `None` when no frame is currently available.
    ///
    /// # Errors
    ///
    /// Returns a provider-specific transport failure.
    fn receive(&mut self) -> Result<Option<Vec<u8>>, ErrorReport>;
    /// Releases provider resources. Calls must be idempotent.
    ///
    /// # Errors
    ///
    /// Returns the first provider cleanup failure.
    fn close(&mut self) -> Result<(), ErrorReport>;
}

impl<T: PortTransport + ?Sized> PortTransport for Box<T> {
    fn send(&mut self, frame: &[u8]) -> Result<(), ErrorReport> {
        (**self).send(frame)
    }

    fn receive(&mut self) -> Result<Option<Vec<u8>>, ErrorReport> {
        (**self).receive()
    }

    fn close(&mut self) -> Result<(), ErrorReport> {
        (**self).close()
    }
}

/// State visible at the peer facade boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortState {
    /// HELLO/ACK completed and application messages are accepted.
    Open,
    /// Local or remote close completed.
    Closed,
    /// A transport or protocol failure invalidated the generation.
    Failed,
}

/// One received peer event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PortEvent {
    /// Complete binary application message.
    Message(Vec<u8>),
    /// The remote endpoint closed gracefully.
    Closed,
}

/// Attached native port independent of threads and async runtimes.
pub struct NativePort<T> {
    transport: T,
    state: PortState,
    identity: PeerExpectation,
    maximum_message: usize,
}

impl<T: PortTransport> NativePort<T> {
    /// Validates bootstrap resources and completes HELLO/ACK before opening the port.
    ///
    /// # Errors
    ///
    /// Rejects stale identity, unsupported Phase 3 providers, or a malformed handshake.
    pub fn attach(
        envelope: BootstrapEnvelope,
        expectation: PeerExpectation,
        mut transport: T,
        maximum_message: usize,
    ) -> Result<Self, ErrorReport> {
        envelope.validate_for(
            EndpointRole::Peer,
            expectation.session_id,
            expectation.generation,
            expectation.protocol,
        )?;
        if maximum_message == 0
            || envelope.memory().provider() != ProviderKind::ProcessTest
            || envelope.signal().provider() != ProviderKind::ProcessTest
        {
            return Err(peer_error(
                ErrorCode::Unsupported,
                Recoverability::ReplaceEndpoint,
                "attach peer provider",
            ));
        }
        let hello = hello_frame(&envelope, expectation)?;
        drop(envelope);
        transport.send(&hello)?;
        let acknowledgement = transport.receive()?.ok_or_else(|| {
            peer_error(
                ErrorCode::Timeout,
                Recoverability::ReplaceEndpoint,
                "peer handshake acknowledgement",
            )
        })?;
        validate_ack(&acknowledgement, expectation)?;
        Ok(Self {
            transport,
            state: PortState::Open,
            identity: expectation,
            maximum_message,
        })
    }

    /// Sends one complete application message.
    ///
    /// # Errors
    ///
    /// Returns `Closed`, `MessageTooLarge`, `Backpressured`, or a transport failure.
    pub fn try_send(&mut self, payload: &[u8]) -> Result<(), ErrorReport> {
        self.ensure_open("peer send")?;
        if payload.len() > self.maximum_message {
            return Err(peer_error(
                ErrorCode::MessageTooLarge,
                Recoverability::Terminal,
                "peer send",
            ));
        }
        let mut frame = Vec::with_capacity(payload.len() + 1);
        frame.push(DATA_KIND);
        frame.extend_from_slice(payload);
        self.transport.send(&frame).inspect_err(|_| {
            self.state = PortState::Failed;
        })
    }

    /// Receives one complete event.
    ///
    /// # Errors
    ///
    /// Returns a transport or protocol failure and invalidates the current port.
    pub fn try_receive(&mut self) -> Result<Option<PortEvent>, ErrorReport> {
        self.ensure_open("peer receive")?;
        let Some(frame) = self.transport.receive().inspect_err(|_| {
            self.state = PortState::Failed;
        })?
        else {
            return Ok(None);
        };
        let Some((&kind, payload)) = frame.split_first() else {
            self.state = PortState::Failed;
            return Err(peer_error(
                ErrorCode::ProtocolViolation,
                Recoverability::ReplaceEndpoint,
                "peer receive frame",
            ));
        };
        match kind {
            DATA_KIND if payload.len() <= self.maximum_message => {
                Ok(Some(PortEvent::Message(payload.to_vec())))
            }
            1 if payload.is_empty() => {
                self.state = PortState::Closed;
                self.transport.close()?;
                Ok(Some(PortEvent::Closed))
            }
            _ => {
                self.state = PortState::Failed;
                Err(peer_error(
                    ErrorCode::ProtocolViolation,
                    Recoverability::ReplaceEndpoint,
                    "peer receive frame",
                ))
            }
        }
    }

    /// Sends a graceful close and idempotently releases transport resources.
    ///
    /// # Errors
    ///
    /// Returns the first provider cleanup failure.
    pub fn close(&mut self) -> Result<(), ErrorReport> {
        if self.state == PortState::Closed {
            return Ok(());
        }
        if self.state == PortState::Open {
            self.transport.send(CLOSE_FRAME)?;
        }
        self.transport.close()?;
        self.state = PortState::Closed;
        Ok(())
    }

    /// Current port state.
    pub const fn state(&self) -> PortState {
        self.state
    }

    /// Generation identity attached to this port.
    pub const fn identity(&self) -> PeerExpectation {
        self.identity
    }

    /// Capabilities guaranteed by the Phase 3 process-test transport.
    pub const fn capabilities(&self) -> TransportCapabilities {
        TransportCapabilities::SHARED_MEMORY_DATA_PLANE
            .union(TransportCapabilities::BINARY_MESSAGES)
            .union(TransportCapabilities::BOUNDED_BACKPRESSURE)
    }

    fn ensure_open(&self, operation: &'static str) -> Result<(), ErrorReport> {
        if self.state == PortState::Open {
            Ok(())
        } else {
            Err(peer_error(
                ErrorCode::Closed,
                Recoverability::Terminal,
                operation,
            ))
        }
    }
}

/// Builds the exact HELLO expected by a process harness.
///
/// # Errors
///
/// Returns `InvalidRange` if authentication material cannot be represented.
pub fn hello_frame(
    envelope: &BootstrapEnvelope,
    expectation: PeerExpectation,
) -> Result<Vec<u8>, ErrorReport> {
    let secret_length = u16::try_from(envelope.secret().expose().len()).map_err(|_| {
        peer_error(
            ErrorCode::InvalidRange,
            Recoverability::ReplaceEndpoint,
            "peer hello secret",
        )
    })?;
    let mut frame = Vec::with_capacity(30 + usize::from(secret_length));
    frame.extend_from_slice(HELLO_MAGIC);
    frame.extend_from_slice(&expectation.session_id.to_bytes());
    frame.extend_from_slice(&expectation.generation.get().to_le_bytes());
    frame.extend_from_slice(&secret_length.to_le_bytes());
    frame.extend_from_slice(envelope.secret().expose());
    Ok(frame)
}

/// Validates HELLO at the parent and returns the matching ACK.
///
/// # Errors
///
/// Returns `ProtocolViolation` for malformed, stale, or unauthenticated input.
pub fn acknowledge_hello(
    hello: &[u8],
    expectation: PeerExpectation,
    secret: &[u8],
) -> Result<Vec<u8>, ErrorReport> {
    let fixed = 30;
    if hello.len() < fixed || &hello[..4] != HELLO_MAGIC {
        return Err(handshake_error("validate peer hello"));
    }
    if hello[4..20] != expectation.session_id.to_bytes()
        || hello[20..28] != expectation.generation.get().to_le_bytes()
    {
        return Err(handshake_error("validate peer hello identity"));
    }
    let secret_length = usize::from(u16::from_le_bytes([hello[28], hello[29]]));
    if hello.len() != fixed + secret_length || hello[fixed..] != *secret {
        return Err(handshake_error("validate peer hello secret"));
    }
    let mut acknowledgement = Vec::with_capacity(28);
    acknowledgement.extend_from_slice(ACK_MAGIC);
    acknowledgement.extend_from_slice(&expectation.session_id.to_bytes());
    acknowledgement.extend_from_slice(&expectation.generation.get().to_le_bytes());
    Ok(acknowledgement)
}

fn validate_ack(acknowledgement: &[u8], expectation: PeerExpectation) -> Result<(), ErrorReport> {
    if acknowledgement.len() != 28
        || &acknowledgement[..4] != ACK_MAGIC
        || acknowledgement[4..20] != expectation.session_id.to_bytes()
        || acknowledgement[20..] != expectation.generation.get().to_le_bytes()
    {
        return Err(handshake_error("validate peer acknowledgement"));
    }
    Ok(())
}

fn handshake_error(operation: &'static str) -> ErrorReport {
    peer_error(
        ErrorCode::ProtocolViolation,
        Recoverability::ReplaceEndpoint,
        operation,
    )
}

fn peer_error(
    code: ErrorCode,
    recoverability: Recoverability,
    operation: &'static str,
) -> ErrorReport {
    let category = match code {
        ErrorCode::Unsupported => ErrorCategory::Unsupported,
        ErrorCode::Closed => ErrorCategory::Closed,
        ErrorCode::Timeout => ErrorCategory::Timeout,
        ErrorCode::MessageTooLarge | ErrorCode::Backpressured => ErrorCategory::Resource,
        _ => ErrorCategory::Protocol,
    };
    ErrorReport::new(category, code, recoverability, operation)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use nwipc_bootstrap_schema::{BootstrapSecret, OpaqueDescriptor, ProtocolRange};

    use super::*;

    struct FakeTransport {
        received: VecDeque<Vec<u8>>,
        sent: Vec<Vec<u8>>,
        closed: bool,
    }

    impl PortTransport for FakeTransport {
        fn send(&mut self, frame: &[u8]) -> Result<(), ErrorReport> {
            self.sent.push(frame.to_vec());
            Ok(())
        }

        fn receive(&mut self) -> Result<Option<Vec<u8>>, ErrorReport> {
            Ok(self.received.pop_front())
        }

        fn close(&mut self) -> Result<(), ErrorReport> {
            self.closed = true;
            Ok(())
        }
    }

    fn expectation() -> PeerExpectation {
        PeerExpectation {
            session_id: SessionId::from_u128(5).unwrap(),
            generation: Generation::new(2).unwrap(),
            protocol: 1,
        }
    }

    fn envelope() -> BootstrapEnvelope {
        BootstrapEnvelope::new(
            expectation().session_id,
            expectation().generation,
            ProtocolRange::new(1, 1).unwrap(),
            EndpointRole::Peer,
            OpaqueDescriptor::new(ProviderKind::ProcessTest, vec![1]).unwrap(),
            OpaqueDescriptor::new(ProviderKind::ProcessTest, vec![2]).unwrap(),
            BootstrapSecret::new(b"secret".to_vec()).unwrap(),
        )
        .unwrap()
    }

    fn acknowledgement() -> Vec<u8> {
        let hello = hello_frame(&envelope(), expectation()).unwrap();
        acknowledge_hello(&hello, expectation(), b"secret").unwrap()
    }

    #[test]
    fn attaches_exchanges_and_closes_idempotently() {
        let transport = FakeTransport {
            received: VecDeque::from([acknowledgement(), vec![0, 1, 2]]),
            sent: Vec::new(),
            closed: false,
        };
        let mut port = NativePort::attach(envelope(), expectation(), transport, 16).unwrap();
        port.try_send(b"request").unwrap();
        assert_eq!(
            port.try_receive().unwrap(),
            Some(PortEvent::Message(vec![1, 2]))
        );
        port.close().unwrap();
        port.close().unwrap();
        assert_eq!(port.state(), PortState::Closed);
    }

    #[test]
    fn stale_generation_is_rejected_before_hello() {
        let transport = FakeTransport {
            received: VecDeque::new(),
            sent: Vec::new(),
            closed: false,
        };
        let stale = PeerExpectation {
            generation: Generation::new(3).unwrap(),
            ..expectation()
        };
        assert_eq!(
            NativePort::attach(envelope(), stale, transport, 16)
                .err()
                .unwrap()
                .code(),
            ErrorCode::StaleGeneration
        );
    }
}
