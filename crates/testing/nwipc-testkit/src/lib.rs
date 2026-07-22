//! Deterministic in-process memory and signal providers for contract tests.

use std::collections::VecDeque;

/// An owned byte region standing in for a mapped shared-memory region.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FakeMappedRegion {
    bytes: Vec<u8>,
    writable: bool,
}

impl FakeMappedRegion {
    /// Creates a zero-filled region with explicit write access.
    pub fn new(length: usize, writable: bool) -> Self {
        Self {
            bytes: vec![0; length],
            writable,
        }
    }

    /// Returns the mapped length.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Returns whether this region has no mapped bytes.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Returns the current bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns writable bytes when the mapping permits writes.
    pub fn bytes_mut(&mut self) -> Option<&mut [u8]> {
        self.writable.then_some(self.bytes.as_mut_slice())
    }
}

/// Controls how a deterministic signal transforms notification hints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FakeSignalMode {
    /// Retain every notification.
    Deliver,
    /// Retain at most one outstanding notification.
    Coalesce,
    /// Drop all notifications.
    Drop,
}

/// A deterministic signal queue that can deliver, coalesce, or drop hints.
#[derive(Debug)]
pub struct FakeSignal {
    mode: FakeSignalMode,
    pending: VecDeque<()>,
}

impl FakeSignal {
    /// Creates a signal with the selected fault behavior.
    pub fn new(mode: FakeSignalMode) -> Self {
        Self {
            mode,
            pending: VecDeque::new(),
        }
    }

    /// Posts one notification hint.
    pub fn notify(&mut self) {
        match self.mode {
            FakeSignalMode::Deliver => self.pending.push_back(()),
            FakeSignalMode::Coalesce if self.pending.is_empty() => self.pending.push_back(()),
            FakeSignalMode::Coalesce | FakeSignalMode::Drop => {}
        }
    }

    /// Consumes an outstanding hint without blocking.
    pub fn try_wait(&mut self) -> bool {
        self.pending.pop_front().is_some()
    }

    /// Returns the number of retained hints for fault-injection assertions.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_region_rejects_mutable_view() {
        assert!(FakeMappedRegion::new(8, false).bytes_mut().is_none());
    }

    #[test]
    fn coalescing_retains_one_hint() {
        let mut signal = FakeSignal::new(FakeSignalMode::Coalesce);
        signal.notify();
        signal.notify();
        assert!(signal.try_wait());
        assert!(!signal.try_wait());
    }
}
