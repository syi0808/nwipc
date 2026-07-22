//! Lock-free operational counters with provider-independent snapshots.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Immutable counter values suitable for diagnostics export.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MetricsSnapshot {
    /// Logical sessions successfully created.
    pub sessions_created: u64,
    /// Logical sessions closed by the public facade.
    pub sessions_closed: u64,
    /// Messages accepted for sending.
    pub messages_sent: u64,
    /// Messages delivered to a receiver.
    pub messages_received: u64,
    /// Application payload bytes accepted for sending.
    pub bytes_sent: u64,
    /// Application payload bytes delivered to a receiver.
    pub bytes_received: u64,
    /// Backpressure observations.
    pub backpressure: u64,
    /// Writable recovery edges.
    pub writable: u64,
    /// Primary provider wake-ups observed by a transport.
    pub primary_wakeups: u64,
    /// Correctness-poll wake-ups requested by a transport.
    pub polling_wakeups: u64,
    /// Notification posts suppressed because shared state was already pending.
    pub coalesced_wakeups: u64,
    /// Correctness polls which found shared-state progress independent of a notification.
    pub polling_recoveries: u64,
    /// Notification provider failures.
    pub signal_failures: u64,
    /// Validation or protocol-contract failures.
    pub validation_failures: u64,
    /// Authentication or trust failures.
    pub authentication_failures: u64,
    /// Stable failures observed at a public boundary.
    pub failures: u64,
    /// Generation replacements.
    pub replacements: u64,
}

#[derive(Default)]
struct Counters {
    sessions_created: AtomicU64,
    sessions_closed: AtomicU64,
    messages_sent: AtomicU64,
    messages_received: AtomicU64,
    bytes_sent: AtomicU64,
    bytes_received: AtomicU64,
    backpressure: AtomicU64,
    writable: AtomicU64,
    primary_wakeups: AtomicU64,
    polling_wakeups: AtomicU64,
    coalesced_wakeups: AtomicU64,
    polling_recoveries: AtomicU64,
    signal_failures: AtomicU64,
    validation_failures: AtomicU64,
    authentication_failures: AtomicU64,
    failures: AtomicU64,
    replacements: AtomicU64,
}

/// Cheaply cloneable metrics recorder without a telemetry SDK dependency.
#[derive(Clone, Default)]
pub struct Metrics(Arc<Counters>);

impl Metrics {
    /// Creates an empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records creation of one logical session.
    pub fn record_session_created(&self) {
        increment(&self.0.sessions_created, 1);
    }

    /// Records closure of one logical session.
    pub fn record_session_closed(&self) {
        increment(&self.0.sessions_closed, 1);
    }

    /// Records an accepted outbound application message.
    pub fn record_sent(&self, bytes: usize) {
        increment(&self.0.messages_sent, 1);
        increment(&self.0.bytes_sent, saturating_u64(bytes));
    }

    /// Records a delivered inbound application message.
    pub fn record_received(&self, bytes: usize) {
        increment(&self.0.messages_received, 1);
        increment(&self.0.bytes_received, saturating_u64(bytes));
    }

    /// Records a backpressure observation.
    pub fn record_backpressure(&self) {
        increment(&self.0.backpressure, 1);
    }

    /// Records a writable recovery edge.
    pub fn record_writable(&self) {
        increment(&self.0.writable, 1);
    }

    /// Adds provider-independent wake-up observations.
    pub fn record_wakeups(&self, primary: u64, polling: u64, coalesced: u64, recovered: u64) {
        increment(&self.0.primary_wakeups, primary);
        increment(&self.0.polling_wakeups, polling);
        increment(&self.0.coalesced_wakeups, coalesced);
        increment(&self.0.polling_recoveries, recovered);
    }

    /// Adds notification-provider failures.
    pub fn record_signal_failures(&self, failures: u64) {
        increment(&self.0.signal_failures, failures);
    }

    /// Records a validation or protocol-contract failure.
    pub fn record_validation_failure(&self) {
        increment(&self.0.validation_failures, 1);
    }

    /// Records an authentication or trust failure.
    pub fn record_authentication_failure(&self) {
        increment(&self.0.authentication_failures, 1);
    }

    /// Records one stable public failure.
    pub fn record_failure(&self) {
        increment(&self.0.failures, 1);
    }

    /// Records generation replacement.
    pub fn record_replacement(&self) {
        increment(&self.0.replacements, 1);
    }

    /// Reads a consistent-enough monotonic operational snapshot.
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            sessions_created: load(&self.0.sessions_created),
            sessions_closed: load(&self.0.sessions_closed),
            messages_sent: load(&self.0.messages_sent),
            messages_received: load(&self.0.messages_received),
            bytes_sent: load(&self.0.bytes_sent),
            bytes_received: load(&self.0.bytes_received),
            backpressure: load(&self.0.backpressure),
            writable: load(&self.0.writable),
            primary_wakeups: load(&self.0.primary_wakeups),
            polling_wakeups: load(&self.0.polling_wakeups),
            coalesced_wakeups: load(&self.0.coalesced_wakeups),
            polling_recoveries: load(&self.0.polling_recoveries),
            signal_failures: load(&self.0.signal_failures),
            validation_failures: load(&self.0.validation_failures),
            authentication_failures: load(&self.0.authentication_failures),
            failures: load(&self.0.failures),
            replacements: load(&self.0.replacements),
        }
    }
}

fn increment(counter: &AtomicU64, value: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(value))
    });
}

fn load(counter: &AtomicU64) -> u64 {
    counter.load(Ordering::Relaxed)
}

fn saturating_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloned_recorders_share_monotonic_counters() {
        let metrics = Metrics::new();
        let clone = metrics.clone();
        metrics.record_sent(7);
        clone.record_failure();
        clone.record_wakeups(2, 3, 4, 1);
        clone.record_signal_failures(5);
        clone.record_validation_failure();
        clone.record_authentication_failure();
        assert_eq!(
            metrics.snapshot(),
            MetricsSnapshot {
                messages_sent: 1,
                bytes_sent: 7,
                failures: 1,
                primary_wakeups: 2,
                polling_wakeups: 3,
                coalesced_wakeups: 4,
                polling_recoveries: 1,
                signal_failures: 5,
                validation_failures: 1,
                authentication_failures: 1,
                ..MetricsSnapshot::default()
            }
        );
    }
}
