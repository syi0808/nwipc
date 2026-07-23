//! Tokio readiness integration for the runtime-neutral peer async adapter.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_peer_async::{AsyncPeer, Interest, PeerPort, Readiness};
use nwipc_peer_core::{PortEvent, PortState};
use tokio::sync::Notify;

/// Default correctness-poll interval when no provider hint arrives.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(1);

#[derive(Default)]
struct Notifications {
    readable: Notify,
    writable: Notify,
}

/// Cloneable bridge for provider or lifecycle callbacks to publish readiness hints.
#[derive(Clone, Default)]
pub struct ReadinessSignal {
    notifications: Arc<Notifications>,
}

impl ReadinessSignal {
    /// Publishes a readable hint. Hints may be coalesced.
    pub fn notify_readable(&self) {
        self.notifications.readable.notify_one();
    }

    /// Publishes a writable hint. Hints may be coalesced.
    pub fn notify_writable(&self) {
        self.notifications.writable.notify_one();
    }
}

/// Tokio readiness source combining callback hints with a bounded correctness poll.
pub struct TokioReadiness {
    signal: ReadinessSignal,
    poll_interval: Duration,
}

impl TokioReadiness {
    /// Creates readiness integration with a bounded correctness-poll interval.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRange` when `poll_interval` is zero.
    pub fn new(poll_interval: Duration) -> Result<Self, ErrorReport> {
        if poll_interval.is_zero() {
            return Err(adapter_error(
                ErrorCode::InvalidRange,
                "tokio poll interval",
            ));
        }
        Ok(Self {
            signal: ReadinessSignal::default(),
            poll_interval,
        })
    }

    /// Returns a callback-safe readiness signal.
    pub fn signal(&self) -> ReadinessSignal {
        self.signal.clone()
    }
}

impl Readiness for TokioReadiness {
    type Wait<'a> = Pin<Box<dyn Future<Output = Result<(), ErrorReport>> + Send + 'a>>;

    fn wait(&self, interest: Interest) -> Self::Wait<'_> {
        let notified = match interest {
            Interest::Readable => self.signal.notifications.readable.notified(),
            Interest::Writable => self.signal.notifications.writable.notified(),
        };
        Box::pin(async move {
            let _ = tokio::time::timeout(self.poll_interval, notified).await;
            Ok(())
        })
    }
}

/// Tokio-native peer facade with no owned task or thread.
pub struct TokioPeer<Port> {
    inner: AsyncPeer<Port, TokioReadiness>,
}

impl<Port> TokioPeer<Port> {
    /// Wraps a nonblocking peer using the default correctness-poll interval.
    pub fn new(port: Port) -> Self {
        Self {
            inner: AsyncPeer::new(
                port,
                TokioReadiness {
                    signal: ReadinessSignal::default(),
                    poll_interval: DEFAULT_POLL_INTERVAL,
                },
            ),
        }
    }

    /// Wraps a nonblocking peer using a custom correctness-poll interval.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRange` when `poll_interval` is zero.
    pub fn with_poll_interval(port: Port, poll_interval: Duration) -> Result<Self, ErrorReport> {
        Ok(Self {
            inner: AsyncPeer::new(port, TokioReadiness::new(poll_interval)?),
        })
    }

    /// Returns a callback-safe bridge for native readiness hints.
    pub fn readiness_signal(&self) -> ReadinessSignal {
        self.inner.readiness().signal()
    }

    /// Releases the synchronous port.
    pub fn into_inner(self) -> Port {
        self.inner.into_parts().0
    }
}

impl<Port: PeerPort> TokioPeer<Port> {
    /// Sends one message and awaits recovery from backpressure.
    ///
    /// # Errors
    ///
    /// Returns the first terminal port failure.
    pub async fn send(&mut self, payload: &[u8]) -> Result<(), ErrorReport> {
        self.inner.send(payload).await
    }

    /// Awaits the next complete peer event.
    ///
    /// # Errors
    ///
    /// Returns the first terminal port failure.
    pub async fn receive(&mut self) -> Result<PortEvent, ErrorReport> {
        self.inner.receive().await
    }

    /// Gracefully closes the peer.
    ///
    /// # Errors
    ///
    /// Returns the first terminal port failure.
    pub async fn close(&mut self) -> Result<(), ErrorReport> {
        self.inner.close().await
    }

    /// Returns the current synchronous port state.
    pub fn state(&self) -> PortState {
        self.inner.state()
    }
}

fn adapter_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Configuration,
        code,
        Recoverability::Terminal,
        operation,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use nwipc_error::Recoverability;

    use super::*;

    struct FakePort {
        sends: VecDeque<Result<(), ErrorReport>>,
        events: VecDeque<Option<PortEvent>>,
        closed: bool,
    }

    impl PeerPort for FakePort {
        fn try_send(&mut self, _payload: &[u8]) -> Result<(), ErrorReport> {
            self.sends.pop_front().unwrap_or(Ok(()))
        }

        fn try_receive(&mut self) -> Result<Option<PortEvent>, ErrorReport> {
            Ok(self.events.pop_front().flatten())
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
            "tokio test",
        )
    }

    #[tokio::test]
    async fn callback_hints_and_polling_both_make_progress() {
        let port = FakePort {
            sends: VecDeque::from([Err(backpressured()), Err(backpressured()), Ok(())]),
            events: VecDeque::from([None, None, Some(PortEvent::Message(vec![4, 5, 6]))]),
            closed: false,
        };
        let mut peer = TokioPeer::with_poll_interval(port, Duration::from_millis(10)).unwrap();
        let signal = peer.readiness_signal();
        signal.notify_writable();

        peer.send(b"request").await.unwrap();
        assert_eq!(
            peer.receive().await.unwrap(),
            PortEvent::Message(vec![4, 5, 6])
        );
    }

    #[test]
    fn rejects_zero_poll_interval() {
        let port = FakePort {
            sends: VecDeque::new(),
            events: VecDeque::new(),
            closed: false,
        };
        assert_eq!(
            TokioPeer::with_poll_interval(port, Duration::ZERO)
                .err()
                .unwrap()
                .code(),
            ErrorCode::InvalidRange
        );
    }
}
