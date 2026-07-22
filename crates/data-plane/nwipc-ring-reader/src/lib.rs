//! Acquire-based committed record reading and consumption.

use nwipc_atomic::ConsumerMemory;
use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_record::{ParsedRecordHeader, RECORD_PREFIX_SIZE, RecordFlags, RecordKind};
use nwipc_ring_core::RingSnapshot;
use nwipc_types::{MessageId, Sequence};

const RECORD_PREFIX_SIZE_U32: u32 = 24;
const _: () = assert!(RECORD_PREFIX_SIZE == RECORD_PREFIX_SIZE_U32 as usize);

/// Consumer half of one record ring.
pub struct RingReader {
    memory: ConsumerMemory,
    maximum_inline_message: u32,
    expected_sequence: Sequence,
}

impl RingReader {
    /// Creates a reader over its unique consumer mapping.
    pub const fn new(memory: ConsumerMemory, maximum_inline_message: u32) -> Self {
        Self {
            memory,
            maximum_inline_message,
            expected_sequence: Sequence::new(0),
        }
    }

    /// Borrows the next committed item without consuming it.
    ///
    /// Explicit and implicit padding are returned as `Padding` receipts. Callers must consume
    /// those receipts and peek again until a record or empty state is reached.
    ///
    /// # Errors
    ///
    /// Returns a protocol error before exposing malformed or out-of-range bytes.
    pub fn peek(&self) -> Result<ReadItem, ErrorReport> {
        let snapshot = self.snapshot()?;
        if snapshot.is_empty() {
            return Ok(ReadItem::Empty);
        }
        let implicit_padding = snapshot.implicit_read_padding();
        if implicit_padding != 0 {
            return Ok(ReadItem::Padding(ReadReceipt {
                current_cursor: snapshot.consumer_cursor(),
                next_cursor: snapshot.consumer_cursor().wrapping_add(implicit_padding),
                sequence: None,
            }));
        }
        let contiguous = (self.memory.capacity() - snapshot.consumer_offset()).min(snapshot.used());
        if contiguous < RECORD_PREFIX_SIZE_U32 {
            return Err(reader_error(
                ErrorCode::Truncated,
                "committed record prefix",
            ));
        }
        let committed = self
            .memory
            .read(snapshot.consumer_cursor(), contiguous as usize)?;
        let header = ParsedRecordHeader::decode_committed(&committed, self.maximum_inline_message)?;
        if header.record_length > snapshot.used() || header.record_length > contiguous {
            return Err(reader_error(ErrorCode::Truncated, "committed record range"));
        }
        let receipt = ReadReceipt {
            current_cursor: snapshot.consumer_cursor(),
            next_cursor: snapshot
                .consumer_cursor()
                .wrapping_add(header.record_length),
            sequence: (header.kind != RecordKind::Padding).then_some(header.sequence),
        };
        if header.kind == RecordKind::Padding {
            return Ok(ReadItem::Padding(receipt));
        }
        if header.sequence != self.expected_sequence {
            return Err(reader_error(
                ErrorCode::ProtocolViolation,
                "record sequence",
            ));
        }
        let payload_end = RECORD_PREFIX_SIZE + header.payload_length as usize;
        Ok(ReadItem::Record(PeekedRecord {
            header,
            payload: committed[RECORD_PREFIX_SIZE..payload_end].to_vec(),
            receipt,
        }))
    }

    /// Consumes a receipt previously returned by [`Self::peek`].
    ///
    /// # Errors
    ///
    /// Returns `InvalidCursor` if the receipt is stale or belongs to another reader position.
    pub fn consume(&mut self, receipt: ReadReceipt) -> Result<ConsumeOutcome, ErrorReport> {
        if self.memory.consumer_cursor()? != receipt.current_cursor {
            return Err(reader_error(ErrorCode::InvalidCursor, "stale read receipt"));
        }
        if let Some(sequence) = receipt.sequence {
            if sequence != self.expected_sequence {
                return Err(reader_error(
                    ErrorCode::ProtocolViolation,
                    "record sequence",
                ));
            }
            self.expected_sequence = self.expected_sequence.wrapping_next();
        }
        self.memory.consume(receipt.next_cursor)?;
        let remaining = self
            .memory
            .producer_cursor()?
            .wrapping_sub(receipt.next_cursor);
        if remaining > self.memory.capacity() {
            return Err(reader_error(
                ErrorCode::InvalidCursor,
                "ring cursor distance",
            ));
        }
        Ok(ConsumeOutcome {
            remaining,
            drained: remaining == 0,
        })
    }

    /// Copies and consumes the next application record, transparently skipping padding.
    ///
    /// # Errors
    ///
    /// Returns a stable validation error for malformed shared bytes.
    pub fn receive(&mut self) -> Result<Option<OwnedRecord>, ErrorReport> {
        loop {
            let (record, receipt) = match self.peek()? {
                ReadItem::Empty => return Ok(None),
                ReadItem::Padding(receipt) => (None, receipt),
                ReadItem::Record(record) => (
                    Some(OwnedRecord {
                        header: record.header,
                        payload: record.payload,
                    }),
                    record.receipt,
                ),
            };
            self.consume(receipt)?;
            if record.is_some() {
                return Ok(record);
            }
        }
    }

    fn snapshot(&self) -> Result<RingSnapshot, ErrorReport> {
        RingSnapshot::new(
            self.memory.capacity(),
            self.memory.producer_cursor()?,
            self.memory.consumer_cursor()?,
        )
    }
}

/// Next committed item in physical ring order.
pub enum ReadItem {
    /// No bytes are committed.
    Empty,
    /// Wrap padding to consume before peeking again.
    Padding(ReadReceipt),
    /// A validated record with copied payload bytes.
    Record(PeekedRecord),
}

/// A validated record copied before its shared range is consumed.
pub struct PeekedRecord {
    /// Parsed fixed-width record header.
    pub header: ParsedRecordHeader,
    /// Exact payload without alignment padding.
    pub payload: Vec<u8>,
    receipt: ReadReceipt,
}

impl PeekedRecord {
    /// Returns the token used to publish consumption after inspection ends.
    pub const fn receipt(&self) -> ReadReceipt {
        self.receipt
    }
}

/// Opaque proof of a validated committed range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadReceipt {
    current_cursor: u32,
    next_cursor: u32,
    sequence: Option<Sequence>,
}

/// Owned record returned by the convenience receive path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedRecord {
    /// Parsed record header.
    pub header: ParsedRecordHeader,
    /// Exact copied payload bytes.
    pub payload: Vec<u8>,
}

impl OwnedRecord {
    /// Returns the non-zero message identity.
    pub const fn message_id(&self) -> Option<MessageId> {
        self.header.message_id
    }

    /// Returns the record kind.
    pub const fn kind(&self) -> RecordKind {
        self.header.kind
    }

    /// Returns the preserved record flags.
    pub const fn flags(&self) -> RecordFlags {
        self.header.flags
    }
}

/// Result of publishing consumption.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConsumeOutcome {
    /// Committed bytes remaining after consumption.
    pub remaining: u32,
    /// Whether consumption drained the ring completely.
    pub drained: bool,
}

fn reader_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Protocol,
        code,
        Recoverability::ReplaceEndpoint,
        operation,
    )
}

#[cfg(test)]
mod tests {
    use std::thread;

    use nwipc_atomic::in_process_ring;
    use nwipc_ring_writer::RingWriter;

    use super::*;

    #[test]
    fn borrows_then_consumes_a_complete_record() {
        let (producer, consumer) = in_process_ring(64).unwrap();
        let mut writer = RingWriter::new(producer, 32);
        let mut reader = RingReader::new(consumer, 32);
        writer
            .send(RecordKind::Data, RecordFlags::END_OF_MESSAGE, b"hello")
            .unwrap();
        let receipt = match reader.peek().unwrap() {
            ReadItem::Record(record) => {
                assert_eq!(record.payload, b"hello");
                record.receipt()
            }
            _ => panic!("expected record"),
        };
        assert!(reader.consume(receipt).unwrap().drained);
        assert!(matches!(reader.peek().unwrap(), ReadItem::Empty));
    }

    #[test]
    fn skips_explicit_and_implicit_wrap_padding() {
        let (producer, consumer) = in_process_ring(128).unwrap();
        let mut writer = RingWriter::new(producer, 64);
        let mut reader = RingReader::new(consumer, 64);
        for value in 0_u8..5 {
            writer
                .send(RecordKind::Data, RecordFlags::NONE, &[])
                .unwrap();
            if value < 4 {
                assert!(reader.receive().unwrap().is_some());
            }
        }
        writer
            .send(RecordKind::Data, RecordFlags::NONE, b"implicit")
            .unwrap();
        assert_eq!(reader.receive().unwrap().unwrap().payload, b"");
        assert_eq!(reader.receive().unwrap().unwrap().payload, b"implicit");

        writer
            .send(RecordKind::Data, RecordFlags::NONE, b"12345678")
            .unwrap();
        writer
            .send(RecordKind::Data, RecordFlags::NONE, b"abcdefgh")
            .unwrap();
        assert_eq!(reader.receive().unwrap().unwrap().payload, b"12345678");
        writer
            .send(RecordKind::Data, RecordFlags::NONE, b"explicit-padding")
            .unwrap();
        assert_eq!(reader.receive().unwrap().unwrap().payload, b"abcdefgh");
        assert_eq!(
            reader.receive().unwrap().unwrap().payload,
            b"explicit-padding"
        );
    }

    #[test]
    fn concurrent_spsc_never_observes_partial_records() {
        const COUNT: u32 = 2_000;
        let (producer, consumer) = in_process_ring(256).unwrap();
        let producer_thread = thread::spawn(move || {
            let mut writer = RingWriter::new(producer, 32);
            for value in 0..COUNT {
                let payload = value.to_le_bytes();
                loop {
                    match writer.send(RecordKind::Data, RecordFlags::NONE, &payload) {
                        Ok(_) => break,
                        Err(error) if error.code() == ErrorCode::Backpressured => {
                            thread::yield_now();
                        }
                        Err(error) => panic!("unexpected writer error: {error}"),
                    }
                }
            }
        });
        let consumer_thread = thread::spawn(move || {
            let mut reader = RingReader::new(consumer, 32);
            for expected in 0..COUNT {
                loop {
                    if let Some(record) = reader.receive().unwrap() {
                        assert_eq!(record.payload, expected.to_le_bytes());
                        break;
                    }
                    thread::yield_now();
                }
            }
        });
        producer_thread.join().unwrap();
        consumer_thread.join().unwrap();
    }
}
