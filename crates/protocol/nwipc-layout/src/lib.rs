//! Platform-independent NWIPC shared-region wire layout.
//!
//! Every multibyte field is encoded explicitly as little-endian bytes. Rust struct layout is
//! never used as a serialization format.

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_types::{Generation, SessionId};

/// Region identification bytes.
pub const REGION_MAGIC: [u8; 8] = *b"NWIPC\0\r\n";
/// Current layout version.
pub const LAYOUT_VERSION: u16 = 1;
/// Little-endian marker encoded in little-endian order.
pub const BYTE_ORDER_MARKER: u16 = 0x4c45;
/// Fixed, page-sized region header.
pub const REGION_HEADER_SIZE: usize = 4096;
/// Fixed prefix containing immutable layout metadata.
pub const REGION_PREFIX_SIZE: usize = 64;
/// Required record and ring capacity alignment.
pub const RECORD_ALIGNMENT: usize = 8;
/// Cache-line spacing reserved for independently updated cursors.
pub const CACHE_LINE_SIZE: usize = 64;
/// Producer cursor byte offset.
pub const PRODUCER_CURSOR_OFFSET: usize = 64;
/// Consumer cursor byte offset.
pub const CONSUMER_CURSOR_OFFSET: usize = 128;
/// Ring data byte offset.
pub const RING_DATA_OFFSET: usize = REGION_HEADER_SIZE;
/// Cursor width fixed by layout version 1.
pub const CURSOR_WIDTH: usize = size_of::<u32>();
/// Maximum unambiguous wrapping distance, rounded down to record alignment.
pub const MAX_RING_CAPACITY: u32 = 2_147_483_640;
/// Maximum inline payload accepted by the first protocol slice.
pub const MAX_INLINE_MESSAGE_SIZE: u32 = 1024 * 1024;
const REGION_HEADER_SIZE_U32: u32 = 4096;
const RECORD_ALIGNMENT_U32: u32 = 8;
const RECORD_ALIGNMENT_U16: u16 = 8;
const CURSOR_WIDTH_U8: u8 = 4;
const RING_DATA_OFFSET_U32: u32 = 4096;

/// Endpoint that owns the producer cursor for a directional region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum OwnerRole {
    /// The `WebKit` renderer writes the region.
    Renderer = 1,
    /// The native peer writes the region.
    Peer = 2,
}

impl OwnerRole {
    const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Renderer),
            2 => Some(Self::Peer),
            _ => None,
        }
    }
}

/// Validated immutable metadata for one directional shared-memory region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegionLayout {
    session_id: SessionId,
    generation: Generation,
    owner: OwnerRole,
    total_length: u64,
    capacity: u32,
    maximum_inline_message: u32,
}

impl RegionLayout {
    /// Creates and validates a version-1 region layout.
    ///
    /// # Errors
    ///
    /// Returns a stable range or alignment error when the requested region cannot be represented.
    pub fn new(
        session_id: SessionId,
        generation: Generation,
        owner: OwnerRole,
        total_length: u64,
        maximum_inline_message: u32,
    ) -> Result<Self, ErrorReport> {
        let capacity = total_length
            .checked_sub(REGION_HEADER_SIZE as u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| layout_error(ErrorCode::InvalidRange, "region total length"))?;
        if !(RECORD_ALIGNMENT_U32..=MAX_RING_CAPACITY).contains(&capacity) {
            return Err(layout_error(ErrorCode::InvalidRange, "ring capacity"));
        }
        if capacity % RECORD_ALIGNMENT_U32 != 0 {
            return Err(layout_error(
                ErrorCode::InvalidAlignment,
                "ring capacity alignment",
            ));
        }
        if maximum_inline_message > MAX_INLINE_MESSAGE_SIZE || maximum_inline_message > capacity {
            return Err(layout_error(
                ErrorCode::InvalidRange,
                "maximum inline message",
            ));
        }
        Ok(Self {
            session_id,
            generation,
            owner,
            total_length,
            capacity,
            maximum_inline_message,
        })
    }

    /// Returns the session identity bound to this region.
    pub const fn session_id(self) -> SessionId {
        self.session_id
    }

    /// Returns the resource generation bound to this region.
    pub const fn generation(self) -> Generation {
        self.generation
    }

    /// Returns the sole writer role.
    pub const fn owner(self) -> OwnerRole {
        self.owner
    }

    /// Returns the complete mapped region length.
    pub const fn total_length(self) -> u64 {
        self.total_length
    }

    /// Returns the ring data capacity.
    pub const fn capacity(self) -> u32 {
        self.capacity
    }

    /// Returns the configured inline payload limit.
    pub const fn maximum_inline_message(self) -> u32 {
        self.maximum_inline_message
    }

    /// Writes a deterministic version-1 header with zeroed cursors and reserved bytes.
    ///
    /// # Errors
    ///
    /// Returns `Truncated` unless `bytes` contains the complete page-sized header.
    pub fn encode(self, bytes: &mut [u8]) -> Result<(), ErrorReport> {
        let header = bytes
            .get_mut(..REGION_HEADER_SIZE)
            .ok_or_else(|| layout_error(ErrorCode::Truncated, "region header encode"))?;
        header.fill(0);
        header[0..8].copy_from_slice(&REGION_MAGIC);
        put_u16(header, 8, LAYOUT_VERSION);
        put_u16(header, 10, BYTE_ORDER_MARKER);
        put_u32(header, 12, REGION_HEADER_SIZE_U32);
        put_u64(header, 16, self.total_length);
        put_u64(header, 24, self.generation.get());
        header[32..48].copy_from_slice(&self.session_id.to_bytes());
        header[48] = self.owner as u8;
        header[49] = CURSOR_WIDTH_U8;
        put_u16(header, 50, RECORD_ALIGNMENT_U16);
        put_u32(header, 52, RING_DATA_OFFSET_U32);
        put_u32(header, 56, self.capacity);
        put_u32(header, 60, self.maximum_inline_message);
        Ok(())
    }

    /// Decodes and validates immutable version-1 metadata.
    ///
    /// Mutable cursor bytes are intentionally not read here; the atomic layer owns them.
    ///
    /// # Errors
    ///
    /// Returns a stable protocol error for truncated or malformed input.
    pub fn decode(bytes: &[u8]) -> Result<Self, ErrorReport> {
        let header = bytes
            .get(..REGION_HEADER_SIZE)
            .ok_or_else(|| layout_error(ErrorCode::Truncated, "region header decode"))?;
        if header[0..8] != REGION_MAGIC {
            return Err(layout_error(ErrorCode::InvalidMagic, "region magic"));
        }
        if get_u16(header, 8) != LAYOUT_VERSION {
            return Err(layout_error(
                ErrorCode::LayoutVersionMismatch,
                "layout version",
            ));
        }
        if get_u16(header, 10) != BYTE_ORDER_MARKER {
            return Err(layout_error(
                ErrorCode::ByteOrderMismatch,
                "layout byte order",
            ));
        }
        if get_u32(header, 12) != REGION_HEADER_SIZE_U32
            || header[49] != CURSOR_WIDTH_U8
            || get_u16(header, 50) != RECORD_ALIGNMENT_U16
            || get_u32(header, 52) != RING_DATA_OFFSET_U32
        {
            return Err(layout_error(
                ErrorCode::ProtocolViolation,
                "layout constants",
            ));
        }
        let session_bytes = header[32..48]
            .try_into()
            .map_err(|_| layout_error(ErrorCode::Truncated, "session id"))?;
        let session_id = SessionId::from_bytes(session_bytes)
            .ok_or_else(|| layout_error(ErrorCode::ProtocolViolation, "zero session id"))?;
        let generation = Generation::new(get_u64(header, 24))
            .ok_or_else(|| layout_error(ErrorCode::ProtocolViolation, "zero generation"))?;
        let owner = OwnerRole::from_wire(header[48])
            .ok_or_else(|| layout_error(ErrorCode::ProtocolViolation, "region owner"))?;
        let layout = Self::new(
            session_id,
            generation,
            owner,
            get_u64(header, 16),
            get_u32(header, 60),
        )?;
        if layout.capacity != get_u32(header, 56) {
            return Err(layout_error(
                ErrorCode::InvalidRange,
                "encoded ring capacity",
            ));
        }
        Ok(layout)
    }
}

/// Opaque cache-line-sized storage used to assert the cursor padding contract.
#[repr(C, align(64))]
pub struct CursorCacheLine([u8; CACHE_LINE_SIZE]);

const _: () = assert!(REGION_PREFIX_SIZE == PRODUCER_CURSOR_OFFSET);
const _: () = assert!(PRODUCER_CURSOR_OFFSET % CACHE_LINE_SIZE == 0);
const _: () = assert!(CONSUMER_CURSOR_OFFSET % CACHE_LINE_SIZE == 0);
const _: () = assert!(CONSUMER_CURSOR_OFFSET - PRODUCER_CURSOR_OFFSET == CACHE_LINE_SIZE);
const _: () = assert!(RING_DATA_OFFSET % REGION_HEADER_SIZE == 0);
const _: () = assert!(size_of::<CursorCacheLine>() == CACHE_LINE_SIZE);
const _: () = assert!(align_of::<CursorCacheLine>() == CACHE_LINE_SIZE);

fn layout_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
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

fn get_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes[offset..offset + 8]
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

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_layout() -> RegionLayout {
        RegionLayout::new(
            SessionId::from_bytes([1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]).unwrap(),
            Generation::new(7).unwrap(),
            OwnerRole::Renderer,
            (REGION_HEADER_SIZE + 8192) as u64,
            4096,
        )
        .unwrap()
    }

    #[test]
    fn golden_prefix_is_architecture_independent() {
        let mut bytes = [0xa5; REGION_HEADER_SIZE];
        fixture_layout().encode(&mut bytes).unwrap();
        let expected = decode_hex(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../tests/protocol-fixtures/layout-v1-prefix.hex"
        )));
        assert_eq!(bytes[..REGION_PREFIX_SIZE], expected);
        assert!(bytes[REGION_PREFIX_SIZE..].iter().all(|byte| *byte == 0));
        assert_eq!(RegionLayout::decode(&bytes), Ok(fixture_layout()));
    }

    #[test]
    fn mutable_cursors_do_not_affect_metadata_decode() {
        let mut bytes = [0; REGION_HEADER_SIZE];
        fixture_layout().encode(&mut bytes).unwrap();
        bytes[PRODUCER_CURSOR_OFFSET..PRODUCER_CURSOR_OFFSET + 4]
            .copy_from_slice(&128_u32.to_le_bytes());
        bytes[CONSUMER_CURSOR_OFFSET..CONSUMER_CURSOR_OFFSET + 4]
            .copy_from_slice(&64_u32.to_le_bytes());
        assert_eq!(RegionLayout::decode(&bytes), Ok(fixture_layout()));
    }

    #[test]
    fn rejects_truncation_mismatch_and_overflow() {
        let mut bytes = [0; REGION_HEADER_SIZE];
        fixture_layout().encode(&mut bytes).unwrap();
        assert_eq!(
            RegionLayout::decode(&bytes[..REGION_HEADER_SIZE - 1])
                .unwrap_err()
                .code(),
            ErrorCode::Truncated
        );
        bytes[8] = 2;
        assert_eq!(
            RegionLayout::decode(&bytes).unwrap_err().code(),
            ErrorCode::LayoutVersionMismatch
        );
        assert!(
            RegionLayout::new(
                fixture_layout().session_id(),
                fixture_layout().generation(),
                OwnerRole::Peer,
                u64::MAX,
                0,
            )
            .is_err()
        );
    }

    #[test]
    fn enforces_capacity_and_inline_boundaries() {
        let base = fixture_layout();
        assert!(
            RegionLayout::new(
                base.session_id(),
                base.generation(),
                base.owner(),
                (REGION_HEADER_SIZE + RECORD_ALIGNMENT) as u64,
                0,
            )
            .is_ok()
        );
        assert!(
            RegionLayout::new(
                base.session_id(),
                base.generation(),
                base.owner(),
                (REGION_HEADER_SIZE + RECORD_ALIGNMENT + 1) as u64,
                0,
            )
            .is_err()
        );
    }

    fn decode_hex(source: &str) -> Vec<u8> {
        source
            .split_ascii_whitespace()
            .map(|byte| u8::from_str_radix(byte, 16).unwrap())
            .collect()
    }
}
