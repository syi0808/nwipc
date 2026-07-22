//! Single fail-closed entry point for untrusted region, cursor, record, and payload bytes.

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_layout::{
    CONSUMER_CURSOR_OFFSET, CURSOR_WIDTH, OwnerRole, PRODUCER_CURSOR_OFFSET, RECORD_ALIGNMENT,
    REGION_HEADER_SIZE, RegionLayout,
};
use nwipc_record::{ParsedRecordHeader, RECORD_PREFIX_SIZE};
use nwipc_types::{Generation, Sequence, SessionId};

/// Expected immutable identity of an attached region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegionExpectation {
    /// Session selected by the control plane.
    pub session_id: SessionId,
    /// Currently active generation.
    pub generation: Generation,
    /// Endpoint allowed to write this direction.
    pub owner: OwnerRole,
}

/// Cursor values proven to describe a bounded committed range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidatedCursors {
    /// Wrapping producer position.
    pub producer: u32,
    /// Wrapping consumer position.
    pub consumer: u32,
    /// Committed bytes available to the consumer.
    pub committed: u32,
}

/// Region metadata and cursors validated before mapping-derived ranges are accessed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidatedRegion {
    /// Immutable decoded layout.
    pub layout: RegionLayout,
    /// Validated cursor snapshot.
    pub cursors: ValidatedCursors,
}

/// Record and exact payload slice validated from committed bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidatedRecord<'a> {
    /// Parsed wire prefix.
    pub header: ParsedRecordHeader,
    /// Exact application/control payload, excluding alignment padding.
    pub payload: &'a [u8],
}

/// Stateless validation boundary. Construction cannot fail or bypass checks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Validator;

impl Validator {
    /// Creates the production validator.
    pub const fn new() -> Self {
        Self
    }

    /// Validates a complete mapped region and its cursor snapshot.
    ///
    /// # Errors
    ///
    /// Rejects truncation, identity mismatch, stale generation, or impossible cursor distance.
    pub fn region(
        self,
        bytes: &[u8],
        expectation: RegionExpectation,
    ) -> Result<ValidatedRegion, ErrorReport> {
        let header = bytes
            .get(..REGION_HEADER_SIZE)
            .ok_or_else(|| validation_error(ErrorCode::Truncated, "validate region header"))?;
        let layout = self.region_layout(header, bytes.len(), expectation)?;
        let producer = read_u32(header, PRODUCER_CURSOR_OFFSET)?;
        let consumer = read_u32(header, CONSUMER_CURSOR_OFFSET)?;
        let cursors = self.cursors(producer, consumer, layout.capacity())?;
        Ok(ValidatedRegion { layout, cursors })
    }

    /// Validates immutable mapped-region metadata before any mapping-derived ring access.
    ///
    /// # Errors
    ///
    /// Rejects truncation, identity mismatch, stale generation, or mapped-length mismatch.
    pub fn region_layout(
        self,
        header: &[u8],
        mapped_len: usize,
        expectation: RegionExpectation,
    ) -> Result<RegionLayout, ErrorReport> {
        let header = header
            .get(..REGION_HEADER_SIZE)
            .ok_or_else(|| validation_error(ErrorCode::Truncated, "validate region header"))?;
        let layout = RegionLayout::decode(header)?;
        if layout.session_id() != expectation.session_id || layout.owner() != expectation.owner {
            return Err(validation_error(
                ErrorCode::ProtocolViolation,
                "validate region identity",
            ));
        }
        if layout.generation() != expectation.generation {
            return Err(validation_error(
                ErrorCode::StaleGeneration,
                "validate region generation",
            ));
        }
        let total_length = usize::try_from(layout.total_length())
            .map_err(|_| validation_error(ErrorCode::InvalidRange, "validate region length"))?;
        if mapped_len != total_length {
            return Err(validation_error(
                ErrorCode::InvalidRange,
                "validate mapped region length",
            ));
        }
        Ok(layout)
    }

    /// Validates wrapping cursors without performing pointer or mapping access.
    ///
    /// # Errors
    ///
    /// Rejects invalid capacity, unaligned cursors, and a distance beyond capacity.
    pub fn cursors(
        self,
        producer: u32,
        consumer: u32,
        capacity: u32,
    ) -> Result<ValidatedCursors, ErrorReport> {
        if capacity == 0
            || usize::try_from(capacity).map_or(true, |value| value % RECORD_ALIGNMENT != 0)
        {
            return Err(validation_error(
                ErrorCode::InvalidRange,
                "validate ring capacity",
            ));
        }
        let record_alignment = u32::try_from(RECORD_ALIGNMENT)
            .map_err(|_| validation_error(ErrorCode::InvalidRange, "validate record alignment"))?;
        if producer % record_alignment != 0 || consumer % record_alignment != 0 {
            return Err(validation_error(
                ErrorCode::InvalidAlignment,
                "validate ring cursors",
            ));
        }
        let committed = producer.wrapping_sub(consumer);
        if committed > capacity {
            return Err(validation_error(
                ErrorCode::InvalidCursor,
                "validate cursor distance",
            ));
        }
        Ok(ValidatedCursors {
            producer,
            consumer,
            committed,
        })
    }

    /// Validates one record entirely inside the proven committed range.
    ///
    /// # Errors
    ///
    /// Rejects truncated, malformed, oversized, or out-of-sequence records.
    pub fn record(
        self,
        committed: &[u8],
        committed_length: u32,
        maximum_payload: u32,
        expected_sequence: Option<Sequence>,
    ) -> Result<ValidatedRecord<'_>, ErrorReport> {
        let committed_length = usize::try_from(committed_length)
            .map_err(|_| validation_error(ErrorCode::InvalidRange, "validate committed length"))?;
        let bounded = committed
            .get(..committed_length)
            .ok_or_else(|| validation_error(ErrorCode::Truncated, "validate committed bytes"))?;
        let header = ParsedRecordHeader::decode_committed(bounded, maximum_payload)?;
        if expected_sequence.is_some_and(|sequence| sequence != header.sequence) {
            return Err(validation_error(
                ErrorCode::ProtocolViolation,
                "validate record sequence",
            ));
        }
        let payload_end = RECORD_PREFIX_SIZE
            .checked_add(header.payload_length as usize)
            .ok_or_else(|| validation_error(ErrorCode::InvalidRange, "validate payload end"))?;
        let payload = bounded
            .get(RECORD_PREFIX_SIZE..payload_end)
            .ok_or_else(|| validation_error(ErrorCode::Truncated, "validate record payload"))?;
        Ok(ValidatedRecord { header, payload })
    }

    /// Validates an arbitrary payload range using checked arithmetic before slicing.
    ///
    /// # Errors
    ///
    /// Rejects arithmetic overflow and ranges outside the logical or physical boundary.
    pub fn payload(
        self,
        bytes: &[u8],
        offset: u32,
        length: u32,
        boundary: u32,
    ) -> Result<&[u8], ErrorReport> {
        let end = offset
            .checked_add(length)
            .ok_or_else(|| validation_error(ErrorCode::InvalidRange, "validate payload range"))?;
        if end > boundary {
            return Err(validation_error(
                ErrorCode::InvalidRange,
                "validate payload boundary",
            ));
        }
        let start = usize::try_from(offset)
            .map_err(|_| validation_error(ErrorCode::InvalidRange, "validate payload offset"))?;
        let end = usize::try_from(end)
            .map_err(|_| validation_error(ErrorCode::InvalidRange, "validate payload end"))?;
        bytes
            .get(start..end)
            .ok_or_else(|| validation_error(ErrorCode::Truncated, "validate payload bytes"))
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, ErrorReport> {
    let value = bytes
        .get(
            offset..offset.checked_add(CURSOR_WIDTH).ok_or_else(|| {
                validation_error(ErrorCode::InvalidRange, "validate cursor offset")
            })?,
        )
        .ok_or_else(|| validation_error(ErrorCode::Truncated, "validate cursor bytes"))?;
    let value: [u8; CURSOR_WIDTH] = value
        .try_into()
        .map_err(|_| validation_error(ErrorCode::Truncated, "validate cursor width"))?;
    Ok(u32::from_le_bytes(value))
}

fn validation_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Protocol,
        code,
        Recoverability::ReplaceEndpoint,
        operation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nwipc_layout::{MAX_INLINE_MESSAGE_SIZE, RING_DATA_OFFSET};
    use nwipc_record::{RecordFlags, RecordKind};
    use nwipc_types::MessageId;

    fn identity() -> RegionExpectation {
        RegionExpectation {
            session_id: SessionId::from_u128(9).unwrap(),
            generation: Generation::new(3).unwrap(),
            owner: OwnerRole::Peer,
        }
    }
    fn region() -> Vec<u8> {
        let mut bytes = vec![0; RING_DATA_OFFSET + 4096];
        RegionLayout::new(
            identity().session_id,
            identity().generation,
            identity().owner,
            bytes.len() as u64,
            1024,
        )
        .unwrap()
        .encode(&mut bytes)
        .unwrap();
        bytes
    }

    #[test]
    fn validates_region_before_ranges() {
        let mut bytes = region();
        bytes[PRODUCER_CURSOR_OFFSET..PRODUCER_CURSOR_OFFSET + 4]
            .copy_from_slice(&32_u32.to_le_bytes());
        let validated = Validator::new().region(&bytes, identity()).unwrap();
        assert_eq!(validated.cursors.committed, 32);
        bytes[CONSUMER_CURSOR_OFFSET..CONSUMER_CURSOR_OFFSET + 4]
            .copy_from_slice(&64_u32.to_le_bytes());
        assert_eq!(
            Validator::new()
                .region(&bytes, identity())
                .unwrap_err()
                .code(),
            ErrorCode::InvalidCursor
        );
    }
    #[test]
    fn validates_record_and_exact_payload() {
        let header = ParsedRecordHeader::new(
            3,
            MessageId::new(1).unwrap(),
            Sequence::new(4),
            RecordKind::Data,
            RecordFlags::NONE,
        )
        .unwrap();
        let mut bytes = vec![0; header.record_length as usize];
        {
            let mut unpublished = header.encode_unpublished(&mut bytes).unwrap();
            unpublished.payload_mut().copy_from_slice(b"abc");
        }
        let record = Validator::new()
            .record(
                &bytes,
                header.record_length,
                MAX_INLINE_MESSAGE_SIZE,
                Some(Sequence::new(4)),
            )
            .unwrap();
        assert_eq!(record.payload, b"abc");
    }
    #[test]
    fn rejects_overflow_truncation_and_alignment() {
        assert_eq!(
            Validator::new()
                .payload(&[], u32::MAX, 1, u32::MAX)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidRange
        );
        assert_eq!(
            Validator::new().cursors(1, 0, 64).unwrap_err().code(),
            ErrorCode::InvalidAlignment
        );
        for length in 0..REGION_HEADER_SIZE {
            let _ = Validator::new().region(&region()[..length], identity());
        }
    }
}
