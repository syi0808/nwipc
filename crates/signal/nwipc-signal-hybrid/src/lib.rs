//! Hybrid primary notification and correctness-poll scheduling.

use std::time::Duration;

use nwipc_error::ErrorReport;
use nwipc_signal_api::{SignalDiagnostics, SignalListener, WaitOutcome};
use nwipc_signal_poll::AdaptivePoller;

/// Reason the consumer should inspect and drain shared state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WakeSource {
    /// Primary provider reported a hint.
    Primary,
    /// Adaptive correctness deadline elapsed.
    Poll,
    /// Overall caller deadline elapsed before the next poll.
    TimedOut,
    /// Listener was cancelled.
    Cancelled,
}

/// Listener that bounds lost-hint recovery with adaptive polling.
#[derive(Debug)]
pub struct HybridSignal<Listener> {
    primary: Listener,
    poller: AdaptivePoller,
    diagnostics: SignalDiagnostics,
}

impl<Listener: SignalListener> HybridSignal<Listener> {
    /// Combines a primary provider with an adaptive correctness poller.
    pub const fn new(primary: Listener, poller: AdaptivePoller) -> Self {
        Self {
            primary,
            poller,
            diagnostics: SignalDiagnostics {
                primary_wakes: 0,
                poll_wakes: 0,
                recovered_wakes: 0,
                provider_failures: 0,
            },
        }
    }

    /// Waits for either the primary provider or the next bounded correctness poll.
    ///
    /// # Errors
    ///
    /// Propagates typed primary-provider errors and records them in diagnostics.
    pub fn wait_timeout(&mut self, timeout: Duration) -> Result<WakeSource, ErrorReport> {
        let poll_interval = self.poller.next_interval();
        let wait = timeout.min(poll_interval);
        let outcome = match self.primary.wait_timeout(wait) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.diagnostics.provider_failures =
                    self.diagnostics.provider_failures.saturating_add(1);
                return Err(error);
            }
        };
        match outcome {
            WaitOutcome::Signaled => {
                self.diagnostics.primary_wakes = self.diagnostics.primary_wakes.saturating_add(1);
                Ok(WakeSource::Primary)
            }
            WaitOutcome::Cancelled => Ok(WakeSource::Cancelled),
            WaitOutcome::TimedOut if poll_interval <= timeout => {
                self.diagnostics.poll_wakes = self.diagnostics.poll_wakes.saturating_add(1);
                Ok(WakeSource::Poll)
            }
            WaitOutcome::TimedOut => Ok(WakeSource::TimedOut),
        }
    }

    /// Records the result of the common drain path after either wake source.
    pub fn record_drain(&mut self, source: WakeSource, found_progress: bool) {
        if source == WakeSource::Poll && found_progress {
            self.diagnostics.recovered_wakes = self.diagnostics.recovered_wakes.saturating_add(1);
        }
        if matches!(source, WakeSource::Primary | WakeSource::Poll) {
            self.poller.record_poll(found_progress);
        }
    }

    /// Cancels the primary listener.
    pub fn cancel(&mut self) {
        self.primary.cancel();
    }

    /// Redacted provider counters.
    pub const fn diagnostics(&self) -> SignalDiagnostics {
        self.diagnostics
    }

    /// Borrows the adaptive poller for scheduling diagnostics.
    pub const fn poller(&self) -> &AdaptivePoller {
        &self.poller
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use nwipc_signal_poll::PollConfig;

    use super::*;

    #[derive(Debug)]
    struct Listener {
        outcomes: VecDeque<WaitOutcome>,
        cancelled: bool,
    }

    impl SignalListener for Listener {
        fn try_wait(&mut self) -> Result<WaitOutcome, ErrorReport> {
            Ok(self.outcomes.pop_front().unwrap_or(WaitOutcome::TimedOut))
        }

        fn wait_timeout(&mut self, _: Duration) -> Result<WaitOutcome, ErrorReport> {
            self.try_wait()
        }

        fn cancel(&mut self) {
            self.cancelled = true;
        }
    }

    #[test]
    fn dropped_primary_is_recovered_by_bounded_poll() {
        let listener = Listener {
            outcomes: VecDeque::from([WaitOutcome::TimedOut]),
            cancelled: false,
        };
        let poller = AdaptivePoller::new(PollConfig::default()).unwrap();
        let mut hybrid = HybridSignal::new(listener, poller);
        let source = hybrid.wait_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(source, WakeSource::Poll);
        hybrid.record_drain(source, true);
        assert_eq!(hybrid.diagnostics().recovered_wakes, 1);
    }
}
