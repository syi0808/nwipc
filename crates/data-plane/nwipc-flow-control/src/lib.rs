//! Byte-based backpressure with high/low-watermark hysteresis.

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};

/// Tracks whether a bounded producer should be considered writable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FlowControl {
    capacity: u32,
    high: u32,
    low: u32,
    backpressured: bool,
    buffered: u32,
}

impl FlowControl {
    /// Creates watermarks satisfying `low < high <= capacity`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRange` for inconsistent thresholds.
    pub fn new(capacity: u32, low: u32, high: u32) -> Result<Self, ErrorReport> {
        if capacity == 0 || low >= high || high > capacity {
            return Err(flow_error(ErrorCode::InvalidRange));
        }
        Ok(Self {
            capacity,
            high,
            low,
            backpressured: false,
            buffered: 0,
        })
    }

    /// Observes current buffered bytes and reports a single writable-return edge.
    ///
    /// # Errors
    ///
    /// Returns `InvalidCursor` if buffered bytes exceed fixed capacity.
    pub fn update(&mut self, buffered: u32) -> Result<FlowUpdate, ErrorReport> {
        if buffered > self.capacity {
            return Err(flow_error(ErrorCode::InvalidCursor));
        }
        let was_backpressured = self.backpressured;
        if self.backpressured {
            if buffered <= self.low {
                self.backpressured = false;
            }
        } else if buffered >= self.high {
            self.backpressured = true;
        }
        self.buffered = buffered;
        Ok(FlowUpdate {
            buffered,
            backpressured: self.backpressured,
            became_writable: was_backpressured && !self.backpressured,
        })
    }

    /// Returns the most recently observed buffered amount.
    pub const fn buffered(&self) -> u32 {
        self.buffered
    }
}

/// Result of one watermark observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FlowUpdate {
    /// Current committed byte count.
    pub buffered: u32,
    /// Whether sends should currently apply backpressure.
    pub backpressured: bool,
    /// Whether this observation crossed the low watermark once.
    pub became_writable: bool,
}

fn flow_error(code: ErrorCode) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Resource,
        code,
        Recoverability::ReplaceEndpoint,
        "flow control",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_one_writable_edge_with_hysteresis() {
        let mut flow = FlowControl::new(100, 25, 75).unwrap();
        assert!(flow.update(75).unwrap().backpressured);
        assert!(!flow.update(30).unwrap().became_writable);
        assert!(flow.update(25).unwrap().became_writable);
        assert!(!flow.update(20).unwrap().became_writable);
    }
}
