//! Fixed-width NWIPC record prefix and its platform-independent codec.
//!
//! Encoding produces an unpublished record. Decoding is deliberately named `decode_committed`:
//! callers must first establish the committed range by an acquire-load of the producer cursor.

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_layout::{MAX_INLINE_MESSAGE_SIZE, RECORD_ALIGNMENT};
use nwipc_types::{MessageId, Sequence};

/// Fixed record prefix size in layout version 1.
pub const RECORD_PREFIX_SIZE: usize = 24;
const RECORD_PREFIX_SIZE_U32: u32 = 24;
const RECORD_ALIGNMENT_U32: u32 = 8;

/// Record kind. Unknown values remain skippable by their encoded record length.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordKind {
    /// Protocol negotiation request.
    Hello,
    /// Protocol negotiation acknowledgement.
    HelloAck,
    /// Application payload.
    Data,
    /// Graceful endpoint close.
    Close,
    /// Immediate generation reset.
    Reset,
    /// Liveness request.
    Ping,
    /// Liveness response.
    Pong,
    /// Consumes the remainder at the end of a wrapping ring.
    Padding,
    /// Bounded wire error report.
    Error,
    /// A future kind not interpreted by this version.
    Unknown(u16),
}

impl RecordKind {
    /// Returns the preserved wire value.
    pub const fn to_wire(self) -> u16 {
        match self {
            Self::Hello => 1,
            Self::HelloAck => 2,
            Self::Data => 3,
            Self::Close => 4,
            Self::Reset => 5,
            Self::Ping => 6,
            Self::Pong => 7,
            Self::Padding => 8,
            Self::Error => 9,
            Self::Unknown(value) => value,
        }
    }

    /// Preserves known and unknown wire values.
    pub const fn from_wire(value: u16) -> Self {
        match value {
            1 => Self::Hello,
            2 => Self::HelloAck,
            3 => Self::Data,
            4 => Self::Close,
            5 => Self::Reset,
            6 => Self::Ping,
            7 => Self::Pong,
            8 => Self::Padding,
            9 => Self::Error,
            unknown => Self::Unknown(unknown),
        }
    }
}

/// Forward-compatible record flags.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct RecordFlags(u16);

impl RecordFlags {
    /// No flags.
    pub const NONE: Self = Self(0);
    /// This record ends a logical message.
    pub const END_OF_MESSAGE: Self = Self(1 << 0);
    /// Fragment metadata is present. Negotiation keeps this disabled in the first slice.
    pub const FRAGMENTED: Self = Self(1 << 1);
    /// A protocol acknowledgement is requested.
    pub const ACK_REQUIRED: Self = Self(1 << 2);
    /// Optional flag bits available to future versions.
    pub const OPTIONAL_MASK: u16 = 0x00ff;
    /// Required flag bits; unknown values in this range fail closed.
    pub const REQUIRED_MASK: u16 = 0xff00;
    const KNOWN: u16 = Self::END_OF_MESSAGE.0 | Self::FRAGMENTED.0 | Self::ACK_REQUIRED.0;

    /// Preserves optional and required wire bits for validation.
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    /// Returns all preserved bits.
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Returns unknown optional bits, which may be ignored while forwarding the record.
    pub const fn unknown_optional_bits(self) -> u16 {
        self.0 & Self::OPTIONAL_MASK & !Self::KNOWN
    }

    /// Returns unknown required bits, which must reject the record.
    pub const fn unknown_required_bits(self) -> u16 {
        self.0 & Self::REQUIRED_MASK
    }

    /// Returns the union of two flag sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Validated fields of a record prefix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParsedRecordHeader {
    /// Complete aligned record length, including prefix and padding.
    pub record_length: u32,
    /// Exact payload byte length.
    pub payload_length: u32,
    /// Non-zero message identity; absent only for padding.
    pub message_id: Option<MessageId>,
    /// Wrapping FIFO sequence.
    pub sequence: Sequence,
    /// Known or preserved future record kind.
    pub kind: RecordKind,
    /// Known and preserved optional flags.
    pub flags: RecordFlags,
}

impl ParsedRecordHeader {
    /// Creates a non-padding record header and computes its aligned wire length.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRange` when the inline payload or aligned record is not representable.
    pub fn new(
        payload_length: u32,
        message_id: MessageId,
        sequence: Sequence,
        kind: RecordKind,
        flags: RecordFlags,
    ) -> Result<Self, ErrorReport> {
        if kind == RecordKind::Padding || payload_length > MAX_INLINE_MESSAGE_SIZE {
            return Err(record_error(
                ErrorCode::InvalidRange,
                "record payload length",
            ));
        }
        validate_required_flags(flags)?;
        let unaligned = RECORD_PREFIX_SIZE_U32
            .checked_add(payload_length)
            .ok_or_else(|| record_error(ErrorCode::InvalidRange, "record length"))?;
        let record_length = align_up(unaligned)
            .ok_or_else(|| record_error(ErrorCode::InvalidRange, "record length"))?;
        Ok(Self {
            record_length,
            payload_length,
            message_id: Some(message_id),
            sequence,
            kind,
            flags,
        })
    }

    /// Creates a padding record that consumes an aligned ring tail.
    ///
    /// # Errors
    ///
    /// Returns an error unless the tail can hold a prefix and is record-aligned.
    pub fn padding(record_length: u32, sequence: Sequence) -> Result<Self, ErrorReport> {
        if record_length < RECORD_PREFIX_SIZE_U32 || record_length % RECORD_ALIGNMENT_U32 != 0 {
            return Err(record_error(
                ErrorCode::InvalidAlignment,
                "padding record length",
            ));
        }
        Ok(Self {
            record_length,
            payload_length: 0,
            message_id: None,
            sequence,
            kind: RecordKind::Padding,
            flags: RecordFlags::NONE,
        })
    }

    /// Encodes the complete record as zeroed, unpublished bytes.
    ///
    /// The returned payload may be filled by the writer. This crate deliberately offers no
    /// commit operation; the ring writer publishes only by release-storing the producer cursor.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` when the output does not contain `record_length` bytes.
    pub fn encode_unpublished(
        self,
        output: &mut [u8],
    ) -> Result<UnpublishedRecord<'_>, ErrorReport> {
        let record = output
            .get_mut(..self.record_length as usize)
            .ok_or_else(|| record_error(ErrorCode::Truncated, "record encode"))?;
        record.fill(0);
        put_u32(record, 0, self.record_length);
        put_u32(record, 4, self.payload_length);
        put_u32(record, 8, self.message_id.map_or(0, MessageId::get));
        put_u32(record, 12, self.sequence.get());
        put_u16(record, 16, self.kind.to_wire());
        put_u16(record, 18, self.flags.bits());
        Ok(UnpublishedRecord {
            header: self,
            bytes: record,
        })
    }

    /// Decodes a prefix from a range already proven committed by the cursor layer.
    ///
    /// # Errors
    ///
    /// Returns a stable error for truncation, invalid lengths, reserved fields, or unknown
    /// required flags. Unknown kinds and unknown optional flags are preserved.
    pub fn decode_committed(
        committed: &[u8],
        maximum_inline_message: u32,
    ) -> Result<Self, ErrorReport> {
        let prefix = committed
            .get(..RECORD_PREFIX_SIZE)
            .ok_or_else(|| record_error(ErrorCode::Truncated, "record prefix decode"))?;
        let record_length = get_u32(prefix, 0);
        let payload_length = get_u32(prefix, 4);
        let raw_message_id = get_u32(prefix, 8);
        let sequence = Sequence::new(get_u32(prefix, 12));
        let kind = RecordKind::from_wire(get_u16(prefix, 16));
        let flags = RecordFlags::from_bits(get_u16(prefix, 18));
        if get_u32(prefix, 20) != 0 {
            return Err(record_error(
                ErrorCode::ProtocolViolation,
                "record reserved field",
            ));
        }
        validate_required_flags(flags)?;
        if record_length < RECORD_PREFIX_SIZE_U32 || record_length % RECORD_ALIGNMENT_U32 != 0 {
            return Err(record_error(
                ErrorCode::InvalidAlignment,
                "record length alignment",
            ));
        }
        if committed.len() < record_length as usize {
            return Err(record_error(ErrorCode::Truncated, "committed record"));
        }
        let message_id = MessageId::new(raw_message_id);
        if kind == RecordKind::Padding {
            if payload_length != 0 || message_id.is_some() || flags != RecordFlags::NONE {
                return Err(record_error(ErrorCode::ProtocolViolation, "padding fields"));
            }
        } else {
            if payload_length > maximum_inline_message
                || payload_length > MAX_INLINE_MESSAGE_SIZE
                || align_up(RECORD_PREFIX_SIZE_U32.saturating_add(payload_length))
                    != Some(record_length)
            {
                return Err(record_error(
                    ErrorCode::InvalidRange,
                    "record payload range",
                ));
            }
            if message_id.is_none() {
                return Err(record_error(
                    ErrorCode::ProtocolViolation,
                    "zero message id",
                ));
            }
        }
        Ok(Self {
            record_length,
            payload_length,
            message_id,
            sequence,
            kind,
            flags,
        })
    }
}

/// Mutable record bytes that have not been made visible through the producer cursor.
pub struct UnpublishedRecord<'a> {
    header: ParsedRecordHeader,
    bytes: &'a mut [u8],
}

impl UnpublishedRecord<'_> {
    /// Returns the validated header.
    pub const fn header(&self) -> ParsedRecordHeader {
        self.header
    }

    /// Returns the exact payload range to fill before publication.
    pub fn payload_mut(&mut self) -> &mut [u8] {
        let end = RECORD_PREFIX_SIZE + self.header.payload_length as usize;
        &mut self.bytes[RECORD_PREFIX_SIZE..end]
    }

    /// Returns the unpublished bytes for a writer-owned region.
    pub fn bytes(&self) -> &[u8] {
        self.bytes
    }
}

const _: () = assert!(RECORD_PREFIX_SIZE % RECORD_ALIGNMENT == 0);
const _: () = assert!(size_of::<RecordFlags>() == 2);

fn validate_required_flags(flags: RecordFlags) -> Result<(), ErrorReport> {
    if flags.unknown_required_bits() != 0 {
        return Err(record_error(
            ErrorCode::UnknownRequiredFlag,
            "record required flags",
        ));
    }
    Ok(())
}

fn align_up(value: u32) -> Option<u32> {
    let mask = RECORD_ALIGNMENT_U32 - 1;
    value.checked_add(mask).map(|value| value & !mask)
}

fn record_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Protocol,
        code,
        Recoverability::ReplaceEndpoint,
        operation,
    )
}

fn get_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(
        bytes[offset..offset + 2]
            .try_into()
            .expect("validated range"),
    )
}

fn get_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("validated range"),
    )
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message_id() -> MessageId {
        MessageId::new(0x1122_3344).unwrap()
    }

    #[test]
    fn kinds_round_trip_with_golden_bytes() {
        let kinds = [
            RecordKind::Hello,
            RecordKind::HelloAck,
            RecordKind::Data,
            RecordKind::Close,
            RecordKind::Reset,
            RecordKind::Ping,
            RecordKind::Pong,
            RecordKind::Error,
            RecordKind::Unknown(0x1234),
        ];
        for kind in kinds {
            let header = ParsedRecordHeader::new(
                3,
                message_id(),
                Sequence::new(0xaabb_ccdd),
                kind,
                RecordFlags::END_OF_MESSAGE,
            )
            .unwrap();
            let mut bytes = [0xa5; 32];
            let mut unpublished = header.encode_unpublished(&mut bytes).unwrap();
            unpublished.payload_mut().copy_from_slice(b"abc");
            assert_eq!(
                &unpublished.bytes()[..RECORD_PREFIX_SIZE],
                &[
                    0x20,
                    0,
                    0,
                    0,
                    3,
                    0,
                    0,
                    0,
                    0x44,
                    0x33,
                    0x22,
                    0x11,
                    0xdd,
                    0xcc,
                    0xbb,
                    0xaa,
                    kind.to_wire().to_le_bytes()[0],
                    kind.to_wire().to_le_bytes()[1],
                    1,
                    0,
                    0,
                    0,
                    0,
                    0,
                ]
            );
            assert_eq!(
                ParsedRecordHeader::decode_committed(unpublished.bytes(), MAX_INLINE_MESSAGE_SIZE),
                Ok(header)
            );
            assert!(unpublished.bytes()[27..].iter().all(|byte| *byte == 0));
        }
    }

    #[test]
    fn data_record_matches_the_external_golden_fixture() {
        let header = ParsedRecordHeader::new(
            3,
            message_id(),
            Sequence::new(0xaabb_ccdd),
            RecordKind::Data,
            RecordFlags::END_OF_MESSAGE,
        )
        .unwrap();
        let mut bytes = [0; 32];
        let mut unpublished = header.encode_unpublished(&mut bytes).unwrap();
        unpublished.payload_mut().copy_from_slice(b"abc");
        let expected = decode_hex(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../tests/protocol-fixtures/record-v1-data.hex"
        )));
        assert_eq!(unpublished.bytes(), expected);
    }

    #[test]
    fn unknown_optional_flags_survive_but_required_flags_fail_closed() {
        let optional = RecordFlags::from_bits(1 << 7);
        let header = ParsedRecordHeader::new(
            0,
            message_id(),
            Sequence::new(0),
            RecordKind::Data,
            optional,
        )
        .unwrap();
        let mut bytes = [0; RECORD_PREFIX_SIZE];
        header.encode_unpublished(&mut bytes).unwrap();
        let decoded = ParsedRecordHeader::decode_committed(&bytes, 0).unwrap();
        assert_eq!(decoded.flags.unknown_optional_bits(), 1 << 7);

        bytes[19] = 1;
        assert_eq!(
            ParsedRecordHeader::decode_committed(&bytes, 0)
                .unwrap_err()
                .code(),
            ErrorCode::UnknownRequiredFlag
        );
    }

    #[test]
    fn validates_zero_exact_maximum_and_padding_boundaries() {
        for payload_length in [0, 8, MAX_INLINE_MESSAGE_SIZE] {
            let header = ParsedRecordHeader::new(
                payload_length,
                message_id(),
                Sequence::new(payload_length),
                RecordKind::Data,
                RecordFlags::NONE,
            )
            .unwrap();
            assert_eq!(header.record_length % RECORD_ALIGNMENT_U32, 0);
        }
        assert!(
            ParsedRecordHeader::new(
                MAX_INLINE_MESSAGE_SIZE + 1,
                message_id(),
                Sequence::new(0),
                RecordKind::Data,
                RecordFlags::NONE,
            )
            .is_err()
        );
        assert!(ParsedRecordHeader::padding(23, Sequence::new(0)).is_err());
        assert!(ParsedRecordHeader::padding(24, Sequence::new(0)).is_ok());
    }

    #[test]
    fn rejects_truncation_and_corrupt_lengths() {
        let header = ParsedRecordHeader::new(
            1,
            message_id(),
            Sequence::new(0),
            RecordKind::Data,
            RecordFlags::NONE,
        )
        .unwrap();
        let mut bytes = [0; 32];
        header.encode_unpublished(&mut bytes).unwrap();
        assert_eq!(
            ParsedRecordHeader::decode_committed(&bytes[..31], 1)
                .unwrap_err()
                .code(),
            ErrorCode::Truncated
        );
        bytes[0] = 25;
        assert_eq!(
            ParsedRecordHeader::decode_committed(&bytes, 1)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidAlignment
        );
    }

    fn decode_hex(source: &str) -> Vec<u8> {
        source
            .split_ascii_whitespace()
            .map(|byte| u8::from_str_radix(byte, 16).unwrap())
            .collect()
    }
}
