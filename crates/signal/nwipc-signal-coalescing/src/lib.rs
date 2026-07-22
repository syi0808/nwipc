//! Lock-free suppression state for notification hints.

use std::sync::atomic::{AtomicBool, Ordering};

/// Whether a state transition requires an actual provider notification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotifyDecision {
    /// This is the first outstanding change and should be posted.
    Notify,
    /// A previous outstanding hint already covers this change.
    Suppress,
}

/// Coalesces repeated state changes until a consumer drains and re-arms the edge.
#[derive(Debug, Default)]
pub struct SignalCoalescer {
    pending: AtomicBool,
}

impl SignalCoalescer {
    /// Creates an armed coalescer with no outstanding change.
    pub const fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
        }
    }

    /// Marks shared state changed and elects at most one posting caller.
    pub fn mark_changed(&self) -> NotifyDecision {
        if self.pending.swap(true, Ordering::AcqRel) {
            NotifyDecision::Suppress
        } else {
            NotifyDecision::Notify
        }
    }

    /// Re-arms after draining shared state.
    ///
    /// Consumers must inspect shared state once more after this operation. A concurrent producer
    /// then either becomes visible to that inspection or observes the armed edge and posts.
    pub fn rearm(&self) {
        self.pending.store(false, Ordering::Release);
    }

    /// Whether a change is currently outstanding.
    pub fn is_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suppresses_storm_until_rearmed() {
        let coalescer = SignalCoalescer::new();
        assert_eq!(coalescer.mark_changed(), NotifyDecision::Notify);
        for _ in 0..100 {
            assert_eq!(coalescer.mark_changed(), NotifyDecision::Suppress);
        }
        coalescer.rearm();
        assert_eq!(coalescer.mark_changed(), NotifyDecision::Notify);
    }
}
