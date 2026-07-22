//! Bounded logical-message fragmentation and single-message reassembly.

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_layout::RECORD_ALIGNMENT;
use nwipc_record::{ParsedRecordHeader, RECORD_PREFIX_SIZE, RecordFlags};
use nwipc_types::MessageId;

/// One borrowed payload fragment and its wire flags.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Fragment<'message> {
    payload: &'message [u8],
    flags: RecordFlags,
}

impl<'message> Fragment<'message> {
    /// Returns the borrowed payload range.
    pub const fn payload(self) -> &'message [u8] {
        self.payload
    }

    /// Returns the record flags for this fragment.
    pub const fn flags(self) -> RecordFlags {
        self.flags
    }
}

/// Validates logical message limits and produces deterministic fragment boundaries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Fragmenter {
    maximum_inline_message: u32,
    maximum_message: u32,
}

impl Fragmenter {
    /// Creates a fragmentation policy.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRange` unless both limits are non-zero and the logical limit is at least
    /// the inline limit.
    pub fn new(maximum_inline_message: u32, maximum_message: u32) -> Result<Self, ErrorReport> {
        if maximum_inline_message == 0 || maximum_message < maximum_inline_message {
            return Err(fragment_error(
                ErrorCode::InvalidRange,
                Recoverability::Terminal,
                "fragment limits",
            ));
        }
        Ok(Self {
            maximum_inline_message,
            maximum_message,
        })
    }

    /// Splits one logical message without copying its payload.
    ///
    /// # Errors
    ///
    /// Returns `MessageTooLarge` when the payload exceeds the configured logical limit.
    pub fn fragments(self, payload: &[u8]) -> Result<Vec<Fragment<'_>>, ErrorReport> {
        let payload_length = u32::try_from(payload.len()).map_err(|_| message_too_large())?;
        if payload_length > self.maximum_message {
            return Err(message_too_large());
        }
        if payload_length <= self.maximum_inline_message {
            return Ok(vec![Fragment {
                payload,
                flags: RecordFlags::END_OF_MESSAGE,
            }]);
        }

        let chunks = payload.chunks(self.maximum_inline_message as usize);
        let fragment_count = chunks.len();
        Ok(chunks
            .enumerate()
            .map(|(index, payload)| Fragment {
                payload,
                flags: if index + 1 == fragment_count {
                    RecordFlags::FRAGMENTED.union(RecordFlags::END_OF_MESSAGE)
                } else {
                    RecordFlags::FRAGMENTED
                },
            })
            .collect())
    }

    /// Returns encoded record bytes for the largest configured logical message, excluding wrap
    /// padding.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRange` if the calculation cannot be represented.
    pub fn maximum_wire_bytes(self) -> Result<u32, ErrorReport> {
        wire_bytes(self.maximum_inline_message, self.maximum_message)
    }

    /// Returns the aligned encoded size of a maximum-sized fragment.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRange` if the calculation cannot be represented.
    pub fn maximum_record_length(self) -> Result<u32, ErrorReport> {
        record_length(self.maximum_inline_message)
    }

    /// Returns the worst-case ring bytes for the largest logical message, including wrap
    /// padding before one fragment.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRange` if the calculation cannot be represented.
    pub fn maximum_batch_bytes(self) -> Result<u32, ErrorReport> {
        let alignment = u32::try_from(RECORD_ALIGNMENT).map_err(|_| invalid_range())?;
        self.maximum_wire_bytes()?
            .checked_add(self.maximum_record_length()?)
            .and_then(|bytes| bytes.checked_sub(alignment))
            .ok_or_else(invalid_range)
    }
}

/// Result of accepting one validated data record into a reassembler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reassembly {
    /// More records with the same message identity are required.
    Pending,
    /// One complete JS/application-owned logical message.
    Complete(Vec<u8>),
}

struct PartialMessage {
    message_id: MessageId,
    payload: Vec<u8>,
}

/// Reassembles at most one fragmented message at a time.
pub struct Reassembler {
    maximum_inline_message: u32,
    maximum_message: u32,
    partial: Option<PartialMessage>,
}

impl Reassembler {
    /// Creates an empty reassembler using the same limits as its sender.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRange` for inconsistent limits.
    pub fn new(maximum_inline_message: u32, maximum_message: u32) -> Result<Self, ErrorReport> {
        Fragmenter::new(maximum_inline_message, maximum_message)?;
        Ok(Self {
            maximum_inline_message,
            maximum_message,
            partial: None,
        })
    }

    /// Accepts one data record and either buffers or completes its logical message.
    ///
    /// # Errors
    ///
    /// Rejects malformed flags, invalid message identities, interleaving, and size overflow.
    pub fn push(
        &mut self,
        header: ParsedRecordHeader,
        payload: &[u8],
    ) -> Result<Reassembly, ErrorReport> {
        let Some(message_id) = header.message_id else {
            return self.fail(ErrorCode::ProtocolViolation, "fragment message id");
        };
        if payload.len() != header.payload_length as usize
            || payload.len() > self.maximum_inline_message as usize
        {
            return self.fail(ErrorCode::ProtocolViolation, "fragment payload range");
        }
        let fragmented = has_flag(header.flags, RecordFlags::FRAGMENTED);
        let ends_message = has_flag(header.flags, RecordFlags::END_OF_MESSAGE);

        if !fragmented {
            if !ends_message || self.partial.is_some() {
                return self.fail(ErrorCode::ProtocolViolation, "fragment interleaving");
            }
            return Ok(Reassembly::Complete(payload.to_vec()));
        }
        if payload.is_empty() || (self.partial.is_none() && ends_message) {
            return self.fail(ErrorCode::ProtocolViolation, "fragment flags");
        }

        if self.partial.is_none() {
            self.partial = Some(PartialMessage {
                message_id,
                payload: Vec::with_capacity(payload.len()),
            });
        }
        if self
            .partial
            .as_ref()
            .is_some_and(|partial| partial.message_id != message_id)
        {
            return self.fail(
                ErrorCode::ProtocolViolation,
                "fragment message interleaving",
            );
        }
        let Some(partial) = self.partial.as_mut() else {
            return Err(fragment_error(
                ErrorCode::Internal,
                Recoverability::ReplaceEndpoint,
                "fragment state",
            ));
        };
        let combined = partial
            .payload
            .len()
            .checked_add(payload.len())
            .filter(|length| *length <= self.maximum_message as usize);
        if combined.is_none() {
            return self.fail(ErrorCode::MessageTooLarge, "fragment message size");
        }
        partial.payload.extend_from_slice(payload);
        if ends_message {
            match self.partial.take() {
                Some(complete) => Ok(Reassembly::Complete(complete.payload)),
                None => self.fail(ErrorCode::Internal, "fragment state"),
            }
        } else {
            Ok(Reassembly::Pending)
        }
    }

    /// Drops an incomplete logical message, for close/reset or generation replacement.
    pub fn discard(&mut self) {
        self.partial = None;
    }

    /// Returns whether a logical message is currently incomplete.
    pub const fn is_pending(&self) -> bool {
        self.partial.is_some()
    }

    fn fail<T>(&mut self, code: ErrorCode, operation: &'static str) -> Result<T, ErrorReport> {
        self.discard();
        Err(fragment_error(
            code,
            Recoverability::ReplaceEndpoint,
            operation,
        ))
    }
}

fn has_flag(flags: RecordFlags, flag: RecordFlags) -> bool {
    flags.bits() & flag.bits() == flag.bits()
}

fn wire_bytes(inline: u32, message: u32) -> Result<u32, ErrorReport> {
    let full_fragments = message / inline;
    let remainder = message % inline;
    let full_bytes = record_length(inline)?
        .checked_mul(full_fragments)
        .ok_or_else(invalid_range)?;
    if remainder == 0 {
        Ok(full_bytes)
    } else {
        full_bytes
            .checked_add(record_length(remainder)?)
            .ok_or_else(invalid_range)
    }
}

fn record_length(payload: u32) -> Result<u32, ErrorReport> {
    let unaligned = u32::try_from(RECORD_PREFIX_SIZE)
        .ok()
        .and_then(|prefix| prefix.checked_add(payload))
        .ok_or_else(invalid_range)?;
    let alignment = u32::try_from(RECORD_ALIGNMENT).map_err(|_| invalid_range())?;
    unaligned
        .checked_add(alignment - 1)
        .map(|length| length & !(alignment - 1))
        .ok_or_else(invalid_range)
}

fn invalid_range() -> ErrorReport {
    fragment_error(
        ErrorCode::InvalidRange,
        Recoverability::Terminal,
        "fragment encoded size",
    )
}

fn message_too_large() -> ErrorReport {
    fragment_error(
        ErrorCode::MessageTooLarge,
        Recoverability::Terminal,
        "logical message size",
    )
}

fn fragment_error(
    code: ErrorCode,
    recoverability: Recoverability,
    operation: &'static str,
) -> ErrorReport {
    ErrorReport::new(
        if matches!(code, ErrorCode::MessageTooLarge | ErrorCode::InvalidRange) {
            ErrorCategory::Resource
        } else {
            ErrorCategory::Protocol
        },
        code,
        recoverability,
        operation,
    )
}

#[cfg(test)]
mod tests {
    use nwipc_record::RecordKind;
    use nwipc_types::Sequence;

    use super::*;

    fn header(
        payload_length: u32,
        message_id: u32,
        sequence: u32,
        flags: RecordFlags,
    ) -> ParsedRecordHeader {
        ParsedRecordHeader::new(
            payload_length,
            MessageId::new(message_id).unwrap(),
            Sequence::new(sequence),
            RecordKind::Data,
            flags,
        )
        .unwrap()
    }

    #[test]
    fn splits_boundaries_without_copying() {
        let fragmenter = Fragmenter::new(4, 10).unwrap();
        let payload = *b"abcdefghij";
        let fragments = fragmenter.fragments(&payload).unwrap();
        assert_eq!(fragments.len(), 3);
        assert_eq!(fragments[0].payload(), b"abcd");
        assert_eq!(fragments[1].payload(), b"efgh");
        assert_eq!(fragments[2].payload(), b"ij");
        assert_eq!(fragments[0].flags(), RecordFlags::FRAGMENTED);
        assert_eq!(
            fragments[2].flags(),
            RecordFlags::FRAGMENTED.union(RecordFlags::END_OF_MESSAGE)
        );
        assert_eq!(fragmenter.fragments(b"abcd").unwrap().len(), 1);
        assert_eq!(
            fragmenter.fragments(b"abcd").unwrap()[0].flags(),
            RecordFlags::END_OF_MESSAGE
        );
    }

    #[test]
    fn reassembles_and_rejects_interleaving() {
        let mut reassembler = Reassembler::new(4, 10).unwrap();
        assert_eq!(
            reassembler.push(header(4, 7, 0, RecordFlags::FRAGMENTED), b"abcd"),
            Ok(Reassembly::Pending)
        );
        assert_eq!(
            reassembler.push(
                header(
                    2,
                    7,
                    1,
                    RecordFlags::FRAGMENTED.union(RecordFlags::END_OF_MESSAGE),
                ),
                b"ef",
            ),
            Ok(Reassembly::Complete(b"abcdef".to_vec()))
        );

        reassembler
            .push(header(4, 8, 2, RecordFlags::FRAGMENTED), b"abcd")
            .unwrap();
        assert_eq!(
            reassembler
                .push(header(1, 9, 3, RecordFlags::FRAGMENTED), b"e")
                .unwrap_err()
                .code(),
            ErrorCode::ProtocolViolation
        );
        assert!(!reassembler.is_pending());
    }

    #[test]
    fn enforces_size_and_terminal_flag_rules() {
        let fragmenter = Fragmenter::new(4, 6).unwrap();
        assert_eq!(
            fragmenter.fragments(b"1234567").unwrap_err().code(),
            ErrorCode::MessageTooLarge
        );
        let mut reassembler = Reassembler::new(4, 6).unwrap();
        assert_eq!(
            reassembler
                .push(
                    header(
                        1,
                        1,
                        0,
                        RecordFlags::FRAGMENTED.union(RecordFlags::END_OF_MESSAGE),
                    ),
                    b"x",
                )
                .unwrap_err()
                .code(),
            ErrorCode::ProtocolViolation
        );
        reassembler
            .push(header(4, 1, 0, RecordFlags::FRAGMENTED), b"1234")
            .unwrap();
        assert_eq!(
            reassembler
                .push(
                    header(
                        3,
                        1,
                        1,
                        RecordFlags::FRAGMENTED.union(RecordFlags::END_OF_MESSAGE),
                    ),
                    b"567",
                )
                .unwrap_err()
                .code(),
            ErrorCode::MessageTooLarge
        );
    }

    #[test]
    fn calculates_aligned_wire_bound() {
        let fragmenter = Fragmenter::new(4, 10).unwrap();
        assert_eq!(fragmenter.maximum_record_length().unwrap(), 32);
        assert_eq!(fragmenter.maximum_wire_bytes().unwrap(), 96);
        assert_eq!(fragmenter.maximum_batch_bytes().unwrap(), 120);
    }
}
