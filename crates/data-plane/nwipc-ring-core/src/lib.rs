//! Pure SPSC ring geometry and wrap planning.

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_layout::{MAX_RING_CAPACITY, RECORD_ALIGNMENT};
use nwipc_record::RECORD_PREFIX_SIZE;

const RECORD_ALIGNMENT_U32: u32 = 8;
const RECORD_PREFIX_SIZE_U32: u32 = 24;
const _: () = assert!(RECORD_ALIGNMENT == RECORD_ALIGNMENT_U32 as usize);
const _: () = assert!(RECORD_PREFIX_SIZE == RECORD_PREFIX_SIZE_U32 as usize);

/// A validated snapshot of the two wrapping byte cursors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RingSnapshot {
    capacity: u32,
    producer: u32,
    consumer: u32,
    used: u32,
}

impl RingSnapshot {
    /// Validates capacity and the unambiguous forward cursor distance.
    ///
    /// # Errors
    ///
    /// Returns `InvalidCursor` when shared cursors imply more committed bytes than capacity.
    pub fn new(capacity: u32, producer: u32, consumer: u32) -> Result<Self, ErrorReport> {
        let alignment = RECORD_ALIGNMENT_U32;
        if capacity == 0 || capacity > MAX_RING_CAPACITY || capacity % alignment != 0 {
            return Err(ring_error(ErrorCode::InvalidRange, "ring capacity"));
        }
        let used = producer.wrapping_sub(consumer);
        if used > capacity {
            return Err(ring_error(ErrorCode::InvalidCursor, "ring cursor distance"));
        }
        Ok(Self {
            capacity,
            producer,
            consumer,
            used,
        })
    }

    /// Returns committed but unconsumed bytes.
    pub const fn used(self) -> u32 {
        self.used
    }

    /// Returns bytes currently available to the producer.
    pub const fn free(self) -> u32 {
        self.capacity - self.used
    }

    /// Returns whether no record is committed.
    pub const fn is_empty(self) -> bool {
        self.used == 0
    }

    /// Returns the physical producer offset.
    pub const fn producer_offset(self) -> u32 {
        self.producer % self.capacity
    }

    /// Returns the physical consumer offset.
    pub const fn consumer_offset(self) -> u32 {
        self.consumer % self.capacity
    }

    /// Returns the absolute wrapping producer cursor.
    pub const fn producer_cursor(self) -> u32 {
        self.producer
    }

    /// Returns the absolute wrapping consumer cursor.
    pub const fn consumer_cursor(self) -> u32 {
        self.consumer
    }

    /// Plans a contiguous record, including any physical tail consumed before wrapping.
    ///
    /// # Errors
    ///
    /// Returns `Backpressured` when free capacity cannot fit the record and its wrap tail.
    pub fn plan_write(self, record_length: u32) -> Result<WritePlan, ErrorReport> {
        let alignment = RECORD_ALIGNMENT_U32;
        if record_length < RECORD_PREFIX_SIZE_U32
            || record_length > self.capacity
            || record_length % alignment != 0
        {
            return Err(ring_error(ErrorCode::InvalidRange, "ring record length"));
        }
        let tail = self.capacity - self.producer_offset();
        let padding_length = if record_length > tail { tail } else { 0 };
        let required = padding_length
            .checked_add(record_length)
            .ok_or_else(|| ring_error(ErrorCode::InvalidRange, "ring write plan"))?;
        if required > self.free() {
            return Err(ring_error(ErrorCode::Backpressured, "ring write capacity"));
        }
        let record_offset = if padding_length == 0 {
            self.producer_offset()
        } else {
            0
        };
        Ok(WritePlan {
            start_cursor: self.producer,
            record_offset,
            record_length,
            padding_offset: self.producer_offset(),
            padding_length,
            publish_cursor: self.producer.wrapping_add(required),
            signal_non_empty: self.is_empty(),
        })
    }

    /// Returns an implicit tail that cannot contain a record prefix, if present.
    pub fn implicit_read_padding(self) -> u32 {
        let tail = self.capacity - self.consumer_offset();
        if tail < RECORD_PREFIX_SIZE_U32 && tail <= self.used {
            tail
        } else {
            0
        }
    }
}

/// Producer geometry for one atomic publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WritePlan {
    /// Absolute producer cursor before this write.
    pub start_cursor: u32,
    /// Physical start of the actual record.
    pub record_offset: u32,
    /// Aligned bytes occupied by the actual record.
    pub record_length: u32,
    /// Physical start of a wrap tail.
    pub padding_offset: u32,
    /// Bytes consumed at the end before wrapping.
    pub padding_length: u32,
    /// Absolute cursor published after all writes complete.
    pub publish_cursor: u32,
    /// Whether publication changes the ring from empty to non-empty.
    pub signal_non_empty: bool,
}

impl WritePlan {
    /// Returns whether the wrap tail can encode an explicit padding record.
    pub fn has_explicit_padding(self) -> bool {
        self.padding_length >= RECORD_PREFIX_SIZE_U32
    }
}

fn ring_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Resource,
        code,
        if code == ErrorCode::Backpressured {
            Recoverability::Retryable
        } else {
            Recoverability::ReplaceEndpoint
        },
        operation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_used_and_free_across_cursor_wrap() {
        let snapshot = RingSnapshot::new(64, 16, u32::MAX - 15).unwrap();
        assert_eq!(snapshot.used(), 32);
        assert_eq!(snapshot.free(), 32);
    }

    #[test]
    fn plans_exact_fit_and_wrapped_records() {
        let exact = RingSnapshot::new(64, 0, 0).unwrap().plan_write(64).unwrap();
        assert_eq!(exact.record_offset, 0);
        assert_eq!(exact.publish_cursor, 64);

        let wrapped = RingSnapshot::new(64, 48, 32)
            .unwrap()
            .plan_write(32)
            .unwrap();
        assert_eq!(wrapped.padding_length, 16);
        assert_eq!(wrapped.record_offset, 0);
        assert_eq!(wrapped.publish_cursor, 96);
    }

    #[test]
    fn rejects_one_byte_short_and_corrupt_distance() {
        assert_eq!(
            RingSnapshot::new(64, 40, 0)
                .unwrap()
                .plan_write(32)
                .unwrap_err()
                .code(),
            ErrorCode::Backpressured
        );
        assert_eq!(
            RingSnapshot::new(64, 65, 0).unwrap_err().code(),
            ErrorCode::InvalidCursor
        );
    }
}
