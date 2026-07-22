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
        assert_eq!(
            metrics.snapshot(),
            MetricsSnapshot {
                messages_sent: 1,
                bytes_sent: 7,
                failures: 1,
                ..MetricsSnapshot::default()
            }
        );
    }
}
