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
use nwipc_layout::{
    CONSUMER_CURSOR_OFFSET, MAX_RING_CAPACITY, PRODUCER_CURSOR_OFFSET, RECORD_ALIGNMENT,
    RING_DATA_OFFSET,
};
use nwipc_memory_api::{MappedRegion, MappingAccess};

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
            inner: ProducerInner::InProcess(Arc::clone(&inner)),
        },
        ConsumerMemory {
            inner: ConsumerInner::InProcess(inner),
        },
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
    inner: ProducerInner,
}

enum ProducerInner {
    InProcess(Arc<SharedRing>),
    Mapped {
        mapping: Box<dyn MappedRegion>,
        capacity: u32,
    },
}

impl ProducerMemory {
    /// Returns the ring's byte capacity.
    pub fn capacity(&self) -> u32 {
        match &self.inner {
            ProducerInner::InProcess(inner) => inner.capacity,
            ProducerInner::Mapped { capacity, .. } => *capacity,
        }
    }

    /// Returns the locally owned producer cursor.
    ///
    /// # Errors
    ///
    /// Propagates a mapped provider's atomic-load failure.
    pub fn producer_cursor(&self) -> Result<u32, ErrorReport> {
        match &self.inner {
            ProducerInner::InProcess(inner) => Ok(inner.producer.load(Ordering::Relaxed)),
            ProducerInner::Mapped { mapping, .. } => {
                <dyn MappedRegion>::load_u32_acquire(mapping.as_ref(), PRODUCER_CURSOR_OFFSET)
            }
        }
    }

    /// Acquires the latest consumer cursor.
    ///
    /// # Errors
    ///
    /// Propagates a mapped provider's atomic-load failure.
    pub fn consumer_cursor(&self) -> Result<u32, ErrorReport> {
        match &self.inner {
            ProducerInner::InProcess(inner) => Ok(inner.consumer.load(Ordering::Acquire)),
            ProducerInner::Mapped { mapping, .. } => {
                <dyn MappedRegion>::load_u32_acquire(mapping.as_ref(), CONSUMER_CURSOR_OFFSET)
            }
        }
    }

    /// Writes a contiguous unpublished range at an absolute logical cursor.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRange` when the range crosses the physical ring end or is not wholly
    /// inside producer-owned free space.
    pub fn write(&mut self, cursor: u32, source: &[u8]) -> Result<(), ErrorReport> {
        let producer = self.producer_cursor()?;
        let capacity = self.capacity();
        let free = self
            .capacity()
            .checked_sub(producer.wrapping_sub(self.consumer_cursor()?))
            .ok_or_else(|| atomic_error(ErrorCode::InvalidCursor))?;
        let distance = cursor.wrapping_sub(producer);
        let length =
            u32::try_from(source.len()).map_err(|_| atomic_error(ErrorCode::InvalidRange))?;
        if distance > free || length > free - distance {
            return Err(atomic_error(ErrorCode::InvalidRange));
        }
        let offset = cursor % capacity;
        let range = checked_range(capacity, offset, source.len())?;
        match &mut self.inner {
            ProducerInner::InProcess(inner) => {
                for (cell, byte) in inner.bytes[range].iter().zip(source) {
                    // SAFETY: Only this unique producer handle writes unpublished/free ranges.
                    unsafe { *cell.get() = *byte };
                }
            }
            ProducerInner::Mapped { mapping, .. } => {
                mapping.write(RING_DATA_OFFSET + range.start, source)?;
            }
        }
        Ok(())
    }

    /// Release-publishes all writes through `cursor`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidCursor` rather than publishing beyond producer-owned free space.
    pub fn publish(&mut self, cursor: u32) -> Result<(), ErrorReport> {
        let current = self.producer_cursor()?;
        let used = current.wrapping_sub(self.consumer_cursor()?);
        let advance = cursor.wrapping_sub(current);
        if used > self.capacity() || advance > self.capacity() - used {
            return Err(atomic_error(ErrorCode::InvalidCursor));
        }
        match &mut self.inner {
            ProducerInner::InProcess(inner) => inner.producer.store(cursor, Ordering::Release),
            ProducerInner::Mapped { mapping, .. } => {
                <dyn MappedRegion>::store_u32_release(
                    mapping.as_mut(),
                    PRODUCER_CURSOR_OFFSET,
                    cursor,
                )?;
            }
        }
        Ok(())
    }
}

/// Consumer-owned view of shared bytes and cursors.
pub struct ConsumerMemory {
    inner: ConsumerInner,
}

enum ConsumerInner {
    InProcess(Arc<SharedRing>),
    Mapped {
        mapping: Box<dyn MappedRegion>,
        capacity: u32,
    },
}

impl ConsumerMemory {
    /// Returns the ring's byte capacity.
    pub fn capacity(&self) -> u32 {
        match &self.inner {
            ConsumerInner::InProcess(inner) => inner.capacity,
            ConsumerInner::Mapped { capacity, .. } => *capacity,
        }
    }

    /// Acquires the latest producer cursor.
    ///
    /// # Errors
    ///
    /// Propagates a mapped provider's atomic-load failure.
    pub fn producer_cursor(&self) -> Result<u32, ErrorReport> {
        match &self.inner {
            ConsumerInner::InProcess(inner) => Ok(inner.producer.load(Ordering::Acquire)),
            ConsumerInner::Mapped { mapping, .. } => {
                <dyn MappedRegion>::load_u32_acquire(mapping.as_ref(), PRODUCER_CURSOR_OFFSET)
            }
        }
    }

    /// Returns the locally owned consumer cursor.
    ///
    /// # Errors
    ///
    /// Propagates a mapped provider's atomic-load failure.
    pub fn consumer_cursor(&self) -> Result<u32, ErrorReport> {
        match &self.inner {
            ConsumerInner::InProcess(inner) => Ok(inner.consumer.load(Ordering::Relaxed)),
            ConsumerInner::Mapped { mapping, .. } => {
                <dyn MappedRegion>::load_u32_acquire(mapping.as_ref(), CONSUMER_CURSOR_OFFSET)
            }
        }
    }

    /// Copies one contiguous committed range at an absolute logical cursor.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRange` when the range crosses the physical ring end or is not wholly
    /// inside consumer-owned committed space.
    pub fn read(&self, cursor: u32, length: usize) -> Result<Vec<u8>, ErrorReport> {
        let consumer = self.consumer_cursor()?;
        let used = self.producer_cursor()?.wrapping_sub(consumer);
        let distance = cursor.wrapping_sub(consumer);
        let length_u32 =
            u32::try_from(length).map_err(|_| atomic_error(ErrorCode::InvalidRange))?;
        if used > self.capacity() || distance > used || length_u32 > used - distance {
            return Err(atomic_error(ErrorCode::InvalidRange));
        }
        let capacity = self.capacity();
        let offset = cursor % capacity;
        let range = checked_range(capacity, offset, length)?;
        let mut output = vec![0; length];
        match &self.inner {
            ConsumerInner::InProcess(inner) => {
                for (output, cell) in output.iter_mut().zip(&inner.bytes[range]) {
                    // SAFETY: Acquire preceded this read and only the consumer reads committed bytes.
                    *output = unsafe { *cell.get() };
                }
            }
            ConsumerInner::Mapped { mapping, .. } => {
                mapping.read(RING_DATA_OFFSET + range.start, &mut output)?;
            }
        }
        Ok(output)
    }

    /// Release-publishes consumption through `cursor`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidCursor` rather than consuming beyond committed bytes.
    pub fn consume(&mut self, cursor: u32) -> Result<(), ErrorReport> {
        let current = self.consumer_cursor()?;
        let used = self.producer_cursor()?.wrapping_sub(current);
        let advance = cursor.wrapping_sub(current);
        if used > self.capacity() || advance > used {
            return Err(atomic_error(ErrorCode::InvalidCursor));
        }
        match &mut self.inner {
            ConsumerInner::InProcess(inner) => inner.consumer.store(cursor, Ordering::Release),
            ConsumerInner::Mapped { mapping, .. } => {
                <dyn MappedRegion>::store_u32_release(
                    mapping.as_mut(),
                    CONSUMER_CURSOR_OFFSET,
                    cursor,
                )?;
            }
        }
        Ok(())
    }
}

/// Connects an owned read-write mapping as the sole producer for its directional ring.
///
/// # Errors
///
/// Rejects a read-only, truncated, oversized, or misaligned mapping.
pub fn mapped_producer(
    mapping: impl MappedRegion,
    capacity: u32,
) -> Result<ProducerMemory, ErrorReport> {
    validate_mapping(&mapping, capacity)?;
    Ok(ProducerMemory {
        inner: ProducerInner::Mapped {
            mapping: Box::new(mapping),
            capacity,
        },
    })
}

/// Connects an owned read-write mapping as the sole consumer for its directional ring.
///
/// # Errors
///
/// Rejects a read-only, truncated, oversized, or misaligned mapping.
pub fn mapped_consumer(
    mapping: impl MappedRegion,
    capacity: u32,
) -> Result<ConsumerMemory, ErrorReport> {
    validate_mapping(&mapping, capacity)?;
    Ok(ConsumerMemory {
        inner: ConsumerInner::Mapped {
            mapping: Box::new(mapping),
            capacity,
        },
    })
}

fn validate_mapping(mapping: &impl MappedRegion, capacity: u32) -> Result<(), ErrorReport> {
    if mapping.access() != MappingAccess::ReadWrite
        || capacity == 0
        || capacity > MAX_RING_CAPACITY
        || capacity % RECORD_ALIGNMENT_U32 != 0
        || mapping.len() != RING_DATA_OFFSET.saturating_add(capacity as usize)
    {
        return Err(atomic_error(ErrorCode::InvalidRange));
    }
    Ok(())
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
        assert_eq!(consumer.producer_cursor().unwrap(), 0);
        producer.publish(8).unwrap();
        assert_eq!(consumer.producer_cursor().unwrap(), 8);
        assert_eq!(consumer.read(0, 8).unwrap(), b"complete");
        consumer.consume(8).unwrap();
        assert_eq!(producer.consumer_cursor().unwrap(), 8);
    }

    #[test]
    fn rejects_invalid_capacity_and_ranges() {
        assert!(in_process_ring(7).is_err());
        let (mut producer, consumer) = in_process_ring(64).unwrap();
        assert!(producer.write(63, b"no").is_err());
        assert!(consumer.read(63, 2).is_err());
    }
}
