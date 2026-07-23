//! Runtime-neutral async adapter for native peer ports.

use std::future::Future;

use nwipc_error::{ErrorCode, ErrorReport, Recoverability};
pub use nwipc_peer_core::PeerPort;
use nwipc_peer_core::{PortEvent, PortState};

/// Readiness direction awaited after a nonblocking operation makes no progress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Interest {
    /// A receive operation may now produce an event.
    Readable,
    /// Outbound capacity may now accept a message.
    Writable,
}

/// Executor-specific readiness registration.
///
/// Implementations must not lose a readiness edge between `wait` returning its future and the
/// first poll of that future. Spurious completion is allowed because operations are always retried.
pub trait Readiness {
    /// Future returned by one readiness registration.
    type Wait<'a>: Future<Output = Result<(), ErrorReport>> + 'a
    where
        Self: 'a;

    /// Registers interest in one direction.
    fn wait(&self, interest: Interest) -> Self::Wait<'_>;
}

/// Async operations layered over a synchronous nonblocking port and readiness source.
pub struct AsyncPeer<Port, Ready> {
    port: Port,
    readiness: Ready,
}

impl<Port, Ready> AsyncPeer<Port, Ready> {
    /// Wraps an owned port without starting a thread or executor.
    pub const fn new(port: Port, readiness: Ready) -> Self {
        Self { port, readiness }
    }

    /// Returns the underlying port.
    pub const fn port(&self) -> &Port {
        &self.port
    }

    /// Returns the underlying port mutably for nonblocking operations.
    pub const fn port_mut(&mut self) -> &mut Port {
        &mut self.port
    }

    /// Returns the readiness registration source.
    pub const fn readiness(&self) -> &Ready {
        &self.readiness
    }

    /// Releases the port and readiness source.
    pub fn into_parts(self) -> (Port, Ready) {
        (self.port, self.readiness)
    }
}

impl<Port: PeerPort, Ready: Readiness> AsyncPeer<Port, Ready> {
    /// Sends one complete message, awaiting writable readiness under backpressure.
    ///
    /// # Errors
    ///
    /// Returns the first terminal port or readiness failure.
    pub async fn send(&mut self, payload: &[u8]) -> Result<(), ErrorReport> {
        loop {
            match self.port.try_send(payload) {
                Ok(()) => return Ok(()),
                Err(error) if is_retryable_backpressure(&error) => {
                    let wait = self.readiness.wait(Interest::Writable);
                    match self.port.try_send(payload) {
                        Ok(()) => return Ok(()),
                        Err(error) if is_retryable_backpressure(&error) => wait.await?,
                        Err(error) => return Err(error),
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Receives the next message or close event, awaiting readable readiness when empty.
    ///
    /// # Errors
    ///
    /// Returns the first port or readiness failure.
    pub async fn receive(&mut self) -> Result<PortEvent, ErrorReport> {
        loop {
            if let Some(event) = self.port.try_receive()? {
                return Ok(event);
            }
            let wait = self.readiness.wait(Interest::Readable);
            if let Some(event) = self.port.try_receive()? {
                return Ok(event);
            }
            wait.await?;
        }
    }

    /// Gracefully closes the peer, awaiting writable readiness if the close marker is backpressured.
    ///
    /// # Errors
    ///
    /// Returns the first terminal port or readiness failure.
    pub async fn close(&mut self) -> Result<(), ErrorReport> {
        loop {
            match self.port.close() {
                Ok(()) => return Ok(()),
                Err(error) if is_retryable_backpressure(&error) => {
                    self.readiness.wait(Interest::Writable).await?;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Returns the current synchronous port state.
    pub fn state(&self) -> PortState {
        self.port.state()
    }
}

fn is_retryable_backpressure(error: &ErrorReport) -> bool {
    error.code() == ErrorCode::Backpressured && error.recoverability() == Recoverability::Retryable
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::future::{Ready, ready};

    use nwipc_error::{ErrorCategory, Recoverability};

    use super::*;

    #[derive(Default)]
    struct ImmediateReadiness;

    impl Readiness for ImmediateReadiness {
        type Wait<'a> = Ready<Result<(), ErrorReport>>;

        fn wait(&self, _interest: Interest) -> Self::Wait<'_> {
            ready(Ok(()))
        }
    }

    struct FakePort {
        sends: VecDeque<Result<(), ErrorReport>>,
        events: VecDeque<PortEvent>,
        closed: bool,
    }

    impl PeerPort for FakePort {
        fn try_send(&mut self, _payload: &[u8]) -> Result<(), ErrorReport> {
            self.sends.pop_front().unwrap_or(Ok(()))
        }

        fn try_receive(&mut self) -> Result<Option<PortEvent>, ErrorReport> {
            Ok(self.events.pop_front())
        }

        fn close(&mut self) -> Result<(), ErrorReport> {
            self.closed = true;
            Ok(())
        }

        fn state(&self) -> PortState {
            if self.closed {
                PortState::Closed
            } else {
                PortState::Open
            }
        }
    }

    fn backpressured() -> ErrorReport {
        ErrorReport::new(
            ErrorCategory::Resource,
            ErrorCode::Backpressured,
            Recoverability::Retryable,
            "async test",
        )
    }

    #[tokio::test]
    async fn retries_backpressure_and_preserves_events() {
        let port = FakePort {
            sends: VecDeque::from([Err(backpressured()), Err(backpressured()), Ok(())]),
            events: VecDeque::from([PortEvent::Message(vec![1, 2, 3])]),
            closed: false,
        };
        let mut peer = AsyncPeer::new(port, ImmediateReadiness);

        peer.send(b"request").await.unwrap();
        assert_eq!(
            peer.receive().await.unwrap(),
            PortEvent::Message(vec![1, 2, 3])
        );
        peer.close().await.unwrap();
        assert_eq!(peer.state(), PortState::Closed);
    }

    #[tokio::test]
    async fn terminal_backpressure_is_not_retried() {
        let terminal = ErrorReport::new(
            ErrorCategory::Resource,
            ErrorCode::Backpressured,
            Recoverability::Terminal,
            "async terminal test",
        );
        let port = FakePort {
            sends: VecDeque::from([Err(terminal), Ok(())]),
            events: VecDeque::new(),
            closed: false,
        };
        let mut peer = AsyncPeer::new(port, ImmediateReadiness);

        assert_eq!(
            peer.send(b"request").await.unwrap_err().recoverability(),
            Recoverability::Terminal
        );
    }
}
