//! SPSC record construction and release publication.

use nwipc_atomic::ProducerMemory;
use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_layout::MAX_INLINE_MESSAGE_SIZE;
use nwipc_record::{ParsedRecordHeader, RecordFlags, RecordKind};
use nwipc_ring_core::{RingSnapshot, WritePlan};
use nwipc_types::{MessageId, Sequence};

/// Producer half of one record ring.
pub struct RingWriter {
    memory: ProducerMemory,
    maximum_inline_message: u32,
    next_message_id: u32,
    next_sequence: Sequence,
}

impl RingWriter {
    /// Creates a writer over its unique producer mapping.
    pub fn new(memory: ProducerMemory, maximum_inline_message: u32) -> Self {
        Self {
            memory,
            maximum_inline_message: maximum_inline_message.min(MAX_INLINE_MESSAGE_SIZE),
            next_message_id: 1,
            next_sequence: Sequence::new(0),
        }
    }

    /// Returns committed bytes that have not yet been consumed.
    ///
    /// # Errors
    ///
    /// Returns `InvalidCursor` for corrupted shared cursors.
    pub fn buffered_amount(&self) -> Result<u32, ErrorReport> {
        Ok(self.snapshot()?.used())
    }

    /// Writes a complete record without publishing it.
    ///
    /// Dropping the returned value simulates a producer crash: bytes may have changed, but the
    /// producer cursor remains unchanged and the consumer cannot observe a partial record.
    ///
    /// # Errors
    ///
    /// Returns `MessageTooLarge`, `Backpressured`, or a stable wire/ring validation error.
    pub fn prepare<'writer>(
        &'writer mut self,
        kind: RecordKind,
        flags: RecordFlags,
        payload: &[u8],
    ) -> Result<PendingWrite<'writer>, ErrorReport> {
        let payload_length = u32::try_from(payload.len()).map_err(|_| message_too_large())?;
        if payload_length > self.maximum_inline_message {
            return Err(message_too_large());
        }
        if kind == RecordKind::Padding {
            return Err(writer_error(
                ErrorCode::ProtocolViolation,
                "application padding",
            ));
        }
        let message_id = MessageId::new(self.next_message_id)
            .ok_or_else(|| writer_error(ErrorCode::Internal, "writer message id"))?;
        let header =
            ParsedRecordHeader::new(payload_length, message_id, self.next_sequence, kind, flags)?;
        let plan = self.snapshot()?.plan_write(header.record_length)?;
        write_padding(&mut self.memory, plan, self.next_sequence)?;

        let mut encoded = vec![0; header.record_length as usize];
        let mut unpublished = header.encode_unpublished(&mut encoded)?;
        unpublished.payload_mut().copy_from_slice(payload);
        self.memory.write(
            plan.start_cursor.wrapping_add(plan.padding_length),
            unpublished.bytes(),
        )?;
        Ok(PendingWrite {
            writer: self,
            plan,
            header,
        })
    }

    /// Prepares and immediately publishes one record.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::prepare`].
    pub fn send(
        &mut self,
        kind: RecordKind,
        flags: RecordFlags,
        payload: &[u8],
    ) -> Result<SendOutcome, ErrorReport> {
        self.prepare(kind, flags, payload)?.commit()
    }

    fn snapshot(&self) -> Result<RingSnapshot, ErrorReport> {
        RingSnapshot::new(
            self.memory.capacity(),
            self.memory.producer_cursor(),
            self.memory.consumer_cursor(),
        )
    }
}

/// Fully written but not yet cursor-published record.
pub struct PendingWrite<'writer> {
    writer: &'writer mut RingWriter,
    plan: WritePlan,
    header: ParsedRecordHeader,
}

impl PendingWrite<'_> {
    /// Returns the record header written into shared bytes.
    pub const fn header(&self) -> ParsedRecordHeader {
        self.header
    }

    /// Release-publishes the complete record.
    ///
    /// # Errors
    ///
    /// Returns `InvalidCursor` if shared cursor state changed inconsistently before publication.
    pub fn commit(self) -> Result<SendOutcome, ErrorReport> {
        self.writer.memory.publish(self.plan.publish_cursor)?;
        self.writer.next_sequence = self.writer.next_sequence.wrapping_next();
        self.writer.next_message_id = self.writer.next_message_id.wrapping_add(1).max(1);
        Ok(SendOutcome {
            buffered_amount: self
                .plan
                .publish_cursor
                .wrapping_sub(self.writer.memory.consumer_cursor()),
            signal_non_empty: self.plan.signal_non_empty,
        })
    }
}

/// Result of a committed send.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SendOutcome {
    /// Bytes committed after publication.
    pub buffered_amount: u32,
    /// Whether the ring crossed from empty to non-empty.
    pub signal_non_empty: bool,
}

fn write_padding(
    memory: &mut ProducerMemory,
    plan: WritePlan,
    sequence: Sequence,
) -> Result<(), ErrorReport> {
    if plan.padding_length == 0 {
        return Ok(());
    }
    let mut padding = vec![0; plan.padding_length as usize];
    if plan.has_explicit_padding() {
        ParsedRecordHeader::padding(plan.padding_length, sequence)?
            .encode_unpublished(&mut padding)?;
    }
    memory.write(plan.start_cursor, &padding)
}

fn message_too_large() -> ErrorReport {
    writer_error(ErrorCode::MessageTooLarge, "inline message size")
}

fn writer_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Resource,
        code,
        if matches!(code, ErrorCode::Backpressured) {
            Recoverability::Retryable
        } else {
            Recoverability::Terminal
        },
        operation,
    )
}

#[cfg(test)]
mod tests {
    use nwipc_atomic::in_process_ring;
    use nwipc_record::RECORD_PREFIX_SIZE;

    use super::*;

    #[test]
    fn dropped_pending_write_is_not_published() {
        let (producer, consumer) = in_process_ring(64).unwrap();
        let mut writer = RingWriter::new(producer, 32);
        {
            let _pending = writer
                .prepare(RecordKind::Data, RecordFlags::NONE, b"partial")
                .unwrap();
        }
        assert_eq!(consumer.producer_cursor(), 0);
        assert_eq!(
            consumer.read(0, RECORD_PREFIX_SIZE).unwrap_err().code(),
            ErrorCode::InvalidRange
        );
    }
}
