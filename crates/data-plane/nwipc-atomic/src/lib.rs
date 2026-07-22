//! Acquire/release atomics and an in-process shared-byte provider.
//!
//! This is the only core crate that turns shared byte storage into references. A producer and a
//! consumer receive distinct, non-cloneable handles. Their safe methods permit only the SPSC
//! access pattern: the producer writes unpublished bytes and the consumer reads committed bytes.

use std::cell::UnsafeCell;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_layout::{MAX_RING_CAPACITY, RECORD_ALIGNMENT};

const RECORD_ALIGNMENT_U32: u32 = 8;
const _: () = assert!(RECORD_ALIGNMENT == RECORD_ALIGNMENT_U32 as usize);

/// An aligned shared `u32` accessed with the wire-defined memory orderings.
#[derive(Debug)]
pub struct SharedAtomic<'mapping> {
    atomic: &'mapping AtomicU32,
}

impl<'mapping> SharedAtomic<'mapping> {
    /// Wraps an ordinary aligned atomic.
    pub const fn new(atomic: &'mapping AtomicU32) -> Self {
        Self { atomic }
    }

    /// Wraps a `u32` inside a live shared mapping.
    ///
    /// # Safety
    ///
    /// `pointer` must remain valid and naturally aligned for `'mapping`. Every process accessing
    /// the location must treat it as an atomic, and the mapping must not be unmapped concurrently.
    ///
    /// # Errors
    ///
    /// Returns `InvalidAlignment` for a null or misaligned pointer.
    pub unsafe fn from_ptr(pointer: *mut u32) -> Result<Self, ErrorReport> {
        let pointer =
            NonNull::new(pointer).ok_or_else(|| atomic_error(ErrorCode::InvalidAlignment))?;
        if pointer.as_ptr().align_offset(align_of::<AtomicU32>()) != 0 {
            return Err(atomic_error(ErrorCode::InvalidAlignment));
        }
        // SAFETY: The caller provides the validity, lifetime, and cross-process atomic contract.
        let atomic = unsafe { &*pointer.cast::<AtomicU32>().as_ptr() };
        Ok(Self { atomic })
    }

    /// Loads a cursor after observing all bytes published before it.
    pub fn load_acquire(&self) -> u32 {
        self.atomic.load(Ordering::Acquire)
    }

    /// Publishes a cursor after all preceding byte accesses.
    pub fn store_release(&self, value: u32) {
        self.atomic.store(value, Ordering::Release);
    }

    /// Atomically advances an epoch and returns its previous value.
    pub fn fetch_add_acq_rel(&self, value: u32) -> u32 {
        self.atomic.fetch_add(value, Ordering::AcqRel)
    }
}

/// Creates a bounded in-process SPSC byte ring.
///
/// This provider models a mapped region for deterministic core tests. OS-backed mappings can
/// supply equivalent producer and consumer handles in a later provider phase.
///
/// # Errors
///
/// Returns `InvalidRange` unless capacity is non-zero, record-aligned, and unambiguous.
pub fn in_process_ring(capacity: u32) -> Result<(ProducerMemory, ConsumerMemory), ErrorReport> {
    if capacity == 0 || capacity > MAX_RING_CAPACITY || capacity % RECORD_ALIGNMENT_U32 != 0 {
        return Err(atomic_error(ErrorCode::InvalidRange));
    }
    let inner = Arc::new(SharedRing {
        bytes: (0..capacity).map(|_| UnsafeCell::new(0_u8)).collect(),
        producer: AtomicU32::new(0),
        consumer: AtomicU32::new(0),
        capacity,
    });
    Ok((
        ProducerMemory {
            inner: Arc::clone(&inner),
        },
        ConsumerMemory { inner },
    ))
}

struct SharedRing {
    bytes: Box<[UnsafeCell<u8>]>,
    producer: AtomicU32,
    consumer: AtomicU32,
    capacity: u32,
}

// SAFETY: Handles are not cloneable. The producer alone writes bytes before release-publishing
// them, and the consumer alone reads published bytes before release-publishing consumption.
unsafe impl Sync for SharedRing {}

/// Producer-owned view of shared bytes and cursors.
pub struct ProducerMemory {
    inner: Arc<SharedRing>,
}

impl ProducerMemory {
    /// Returns the ring's byte capacity.
    pub fn capacity(&self) -> u32 {
        self.inner.capacity
    }

    /// Returns the locally owned producer cursor.
    pub fn producer_cursor(&self) -> u32 {
        self.inner.producer.load(Ordering::Relaxed)
    }

    /// Acquires the latest consumer cursor.
    pub fn consumer_cursor(&self) -> u32 {
        self.inner.consumer.load(Ordering::Acquire)
    }

    /// Writes a contiguous unpublished range at an absolute logical cursor.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRange` when the range crosses the physical ring end or is not wholly
    /// inside producer-owned free space.
    pub fn write(&mut self, cursor: u32, source: &[u8]) -> Result<(), ErrorReport> {
        let producer = self.producer_cursor();
        let free = self
            .inner
            .capacity
            .checked_sub(producer.wrapping_sub(self.consumer_cursor()))
            .ok_or_else(|| atomic_error(ErrorCode::InvalidCursor))?;
        let distance = cursor.wrapping_sub(producer);
        let length =
            u32::try_from(source.len()).map_err(|_| atomic_error(ErrorCode::InvalidRange))?;
        if distance > free || length > free - distance {
            return Err(atomic_error(ErrorCode::InvalidRange));
        }
        let offset = cursor % self.inner.capacity;
        let range = checked_range(self.inner.capacity, offset, source.len())?;
        for (cell, byte) in self.inner.bytes[range].iter().zip(source) {
            // SAFETY: Only this unique producer handle writes unpublished/free byte ranges.
            unsafe { *cell.get() = *byte };
        }
        Ok(())
    }

    /// Release-publishes all writes through `cursor`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidCursor` rather than publishing beyond producer-owned free space.
    pub fn publish(&mut self, cursor: u32) -> Result<(), ErrorReport> {
        let current = self.producer_cursor();
        let used = current.wrapping_sub(self.consumer_cursor());
        let advance = cursor.wrapping_sub(current);
        if used > self.inner.capacity || advance > self.inner.capacity - used {
            return Err(atomic_error(ErrorCode::InvalidCursor));
        }
        self.inner.producer.store(cursor, Ordering::Release);
        Ok(())
    }
}

/// Consumer-owned view of shared bytes and cursors.
pub struct ConsumerMemory {
    inner: Arc<SharedRing>,
}

impl ConsumerMemory {
    /// Returns the ring's byte capacity.
    pub fn capacity(&self) -> u32 {
        self.inner.capacity
    }

    /// Acquires the latest producer cursor.
    pub fn producer_cursor(&self) -> u32 {
        self.inner.producer.load(Ordering::Acquire)
    }

    /// Returns the locally owned consumer cursor.
    pub fn consumer_cursor(&self) -> u32 {
        self.inner.consumer.load(Ordering::Relaxed)
    }

    /// Borrows one contiguous committed range at an absolute logical cursor.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRange` when the range crosses the physical ring end or is not wholly
    /// inside consumer-owned committed space.
    pub fn read(&self, cursor: u32, length: usize) -> Result<&[u8], ErrorReport> {
        let consumer = self.consumer_cursor();
        let used = self.producer_cursor().wrapping_sub(consumer);
        let distance = cursor.wrapping_sub(consumer);
        let length_u32 =
            u32::try_from(length).map_err(|_| atomic_error(ErrorCode::InvalidRange))?;
        if used > self.inner.capacity || distance > used || length_u32 > used - distance {
            return Err(atomic_error(ErrorCode::InvalidRange));
        }
        let offset = cursor % self.inner.capacity;
        let range = checked_range(self.inner.capacity, offset, length)?;
        let pointer = self.inner.bytes.as_ptr().cast::<u8>();
        // SAFETY: Acquire of the producer cursor precedes this call. The unique consumer handle
        // does not publish consumption while the returned borrow keeps it immutably borrowed.
        Ok(unsafe { std::slice::from_raw_parts(pointer.add(range.start), range.len()) })
    }

    /// Release-publishes consumption through `cursor`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidCursor` rather than consuming beyond committed bytes.
    pub fn consume(&mut self, cursor: u32) -> Result<(), ErrorReport> {
        let current = self.consumer_cursor();
        let used = self.producer_cursor().wrapping_sub(current);
        let advance = cursor.wrapping_sub(current);
        if used > self.inner.capacity || advance > used {
            return Err(atomic_error(ErrorCode::InvalidCursor));
        }
        self.inner.consumer.store(cursor, Ordering::Release);
        Ok(())
    }
}

fn checked_range(
    capacity: u32,
    offset: u32,
    length: usize,
) -> Result<std::ops::Range<usize>, ErrorReport> {
    let start = usize::try_from(offset).map_err(|_| atomic_error(ErrorCode::InvalidRange))?;
    let end = start
        .checked_add(length)
        .ok_or_else(|| atomic_error(ErrorCode::InvalidRange))?;
    if end > capacity as usize {
        return Err(atomic_error(ErrorCode::InvalidRange));
    }
    Ok(start..end)
}

fn atomic_error(code: ErrorCode) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Memory,
        code,
        Recoverability::ReplaceEndpoint,
        "shared ring memory",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_release_cursor_publishes_bytes() {
        let (mut producer, mut consumer) = in_process_ring(64).unwrap();
        producer.write(0, b"complete").unwrap();
        assert_eq!(consumer.producer_cursor(), 0);
        producer.publish(8).unwrap();
        assert_eq!(consumer.producer_cursor(), 8);
        assert_eq!(consumer.read(0, 8).unwrap(), b"complete");
        consumer.consume(8).unwrap();
        assert_eq!(producer.consumer_cursor(), 8);
    }

    #[test]
    fn rejects_invalid_capacity_and_ranges() {
        assert!(in_process_ring(7).is_err());
        let (mut producer, consumer) = in_process_ring(64).unwrap();
        assert!(producer.write(63, b"no").is_err());
        assert!(consumer.read(63, 2).is_err());
    }
}
