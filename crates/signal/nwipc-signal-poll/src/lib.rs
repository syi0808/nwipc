//! Adaptive correctness polling without a platform event dependency.

use std::time::Duration;

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};

/// Bounds for adaptive polling intervals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PollConfig {
    /// Interval immediately after observed activity.
    pub active: Duration,
    /// First interval while idle.
    pub idle: Duration,
    /// Maximum idle interval.
    pub maximum: Duration,
}

impl PollConfig {
    /// Validates non-zero, ascending interval bounds.
    ///
    /// # Errors
    ///
    /// Rejects zero or descending durations.
    pub fn validate(self) -> Result<Self, ErrorReport> {
        if self.active.is_zero()
            || self.idle.is_zero()
            || self.active > self.idle
            || self.idle > self.maximum
        {
            return Err(ErrorReport::new(
                ErrorCategory::Configuration,
                ErrorCode::InvalidRange,
                Recoverability::Terminal,
                "validate poll configuration",
            ));
        }
        Ok(self)
    }
}

impl Default for PollConfig {
    fn default() -> Self {
        Self {
            active: Duration::from_millis(1),
            idle: Duration::from_millis(8),
            maximum: Duration::from_millis(64),
        }
    }
}

/// Deterministic adaptive interval state; sleeping remains the caller's responsibility.
#[derive(Clone, Debug)]
pub struct AdaptivePoller {
    config: PollConfig,
    next: Duration,
    polls: u64,
}

impl AdaptivePoller {
    /// Creates a poller at its active interval.
    ///
    /// # Errors
    ///
    /// Returns a configuration error for invalid bounds.
    pub fn new(config: PollConfig) -> Result<Self, ErrorReport> {
        let config = config.validate()?;
        Ok(Self {
            next: config.active,
            config,
            polls: 0,
        })
    }

    /// Current time until the next correctness poll.
    pub const fn next_interval(&self) -> Duration {
        self.next
    }

    /// Records a poll and adapts based on whether shared-state progress was found.
    pub fn record_poll(&mut self, found_progress: bool) {
        self.polls = self.polls.saturating_add(1);
        if found_progress {
            self.next = self.config.active;
        } else if self.next < self.config.idle {
            self.next = self.config.idle;
        } else {
            self.next = self.next.saturating_mul(2).min(self.config.maximum);
        }
    }

    /// Number of correctness polls requested.
    pub const fn polls(&self) -> u64 {
        self.polls
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backs_off_and_resets_without_busy_loop() {
        let mut poller = AdaptivePoller::new(PollConfig::default()).unwrap();
        assert_eq!(poller.next_interval(), Duration::from_millis(1));
        poller.record_poll(false);
        assert_eq!(poller.next_interval(), Duration::from_millis(8));
        for _ in 0..8 {
            poller.record_poll(false);
        }
        assert_eq!(poller.next_interval(), Duration::from_millis(64));
        poller.record_poll(true);
        assert_eq!(poller.next_interval(), Duration::from_millis(1));
    }
}
