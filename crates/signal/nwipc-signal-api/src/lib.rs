//! Provider-neutral notification-hint contracts.

use std::time::Duration;

use nwipc_error::ErrorReport;

/// Logical shared-state direction associated with a notification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignalDirection {
    /// Renderer-to-peer state changed.
    RendererToPeer,
    /// Peer-to-renderer state changed.
    PeerToRenderer,
}

impl SignalDirection {
    /// Stable provider suffix.
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::RendererToPeer => "r2p",
            Self::PeerToRenderer => "p2r",
        }
    }
}

/// Result of observing a notification provider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitOutcome {
    /// Shared state may have changed; the consumer must drain it.
    Signaled,
    /// No hint arrived before the bounded deadline.
    TimedOut,
    /// The listener was explicitly cancelled.
    Cancelled,
}

/// Notification sender. A successful post remains only a hint, never a message count.
pub trait SignalSender: Send + Sync + 'static {
    /// Posts a change hint.
    ///
    /// # Errors
    ///
    /// Returns a typed provider error.
    fn notify(&self) -> Result<(), ErrorReport>;
}

/// Notification listener with explicit cancellation.
pub trait SignalListener: Send + 'static {
    /// Observes a pending hint without blocking.
    ///
    /// # Errors
    ///
    /// Returns a typed provider error.
    fn try_wait(&mut self) -> Result<WaitOutcome, ErrorReport>;

    /// Waits up to a bounded duration for a hint.
    ///
    /// # Errors
    ///
    /// Returns a typed provider error.
    fn wait_timeout(&mut self, timeout: Duration) -> Result<WaitOutcome, ErrorReport>;

    /// Cancels this listener. Cancellation is idempotent and terminal.
    fn cancel(&mut self);
}

/// Provider counters safe to expose in diagnostics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SignalDiagnostics {
    /// Successful primary notifications observed.
    pub primary_wakes: u64,
    /// Correctness polls requested.
    pub poll_wakes: u64,
    /// Polls which recovered progress after a missing primary hint.
    pub recovered_wakes: u64,
    /// Provider failures observed by the adapter.
    pub provider_failures: u64,
}
