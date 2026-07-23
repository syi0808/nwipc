//! Bounded fixed-size payload slabs with loan, commit, borrow, and release ownership.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};

/// Creates one bounded SPSC chunk pool.
///
/// The producer loans a fixed-size slab and commits its initialized prefix. The consumer borrows
/// that prefix until the received chunk is dropped or explicitly released. Uncommitted loans are
/// returned to the free queue automatically.
///
/// # Errors
///
/// Rejects a zero chunk count, zero capacity, or an allocation size that overflows `usize`.
pub fn chunk_pool(
    chunk_count: usize,
    chunk_capacity: usize,
) -> Result<(ChunkProducer, ChunkConsumer), ErrorReport> {
    let allocation = chunk_count
        .checked_mul(chunk_capacity)
        .ok_or_else(invalid_configuration)?;
    if chunk_count == 0 || chunk_capacity == 0 || allocation > isize::MAX as usize {
        return Err(invalid_configuration());
    }

    let mut slots = Vec::new();
    slots
        .try_reserve_exact(chunk_count)
        .map_err(|_| allocation_failed())?;
    for _ in 0..chunk_count {
        let mut buffer = Vec::new();
        buffer
            .try_reserve_exact(chunk_capacity)
            .map_err(|_| allocation_failed())?;
        buffer.resize(chunk_capacity, 0);
        slots.push(Some(buffer.into_boxed_slice()));
    }
    let mut free = VecDeque::new();
    free.try_reserve(chunk_count)
        .map_err(|_| allocation_failed())?;
    free.extend(0..chunk_count);

    let shared = Arc::new(Shared {
        chunk_capacity,
        chunk_count,
        state: Mutex::new(State {
            slots,
            free,
            ready: VecDeque::new(),
            loaned: 0,
            received: 0,
        }),
    });
    Ok((
        ChunkProducer {
            shared: Arc::clone(&shared),
        },
        ChunkConsumer { shared },
    ))
}

struct Shared {
    chunk_capacity: usize,
    chunk_count: usize,
    state: Mutex<State>,
}

struct State {
    slots: Vec<Option<Box<[u8]>>>,
    free: VecDeque<usize>,
    ready: VecDeque<(usize, usize)>,
    loaned: usize,
    received: usize,
}

/// The sole submission side of a chunk pool.
pub struct ChunkProducer {
    shared: Arc<Shared>,
}

impl ChunkProducer {
    /// Loans one slab whose initialized payload has exactly `length` bytes.
    ///
    /// # Errors
    ///
    /// Returns `MessageTooLarge` when `length` exceeds the slab capacity and `Backpressured` when
    /// every slab is loaned, ready, or borrowed by the consumer.
    pub fn loan(&mut self, length: usize) -> Result<LoanedChunk<'_>, ErrorReport> {
        if length > self.shared.chunk_capacity {
            return Err(pool_error(
                ErrorCode::MessageTooLarge,
                Recoverability::Terminal,
                "loan chunk",
            ));
        }
        let (index, buffer) = {
            let mut state = lock(&self.shared)?;
            let index = state.free.pop_front().ok_or_else(backpressured)?;
            state.loaned += 1;
            let buffer = state.slots[index].take().ok_or_else(invalid_pool_state)?;
            (index, buffer)
        };
        Ok(LoanedChunk {
            producer: self,
            index,
            length,
            buffer: Some(buffer),
        })
    }

    /// Returns the fixed payload capacity of every slab.
    pub fn chunk_capacity(&self) -> usize {
        self.shared.chunk_capacity
    }

    /// Returns a point-in-time ownership snapshot.
    ///
    /// # Errors
    ///
    /// Returns `Internal` if another thread panicked while changing pool ownership.
    pub fn diagnostics(&self) -> Result<PoolDiagnostics, ErrorReport> {
        diagnostics(&self.shared)
    }
}

/// A producer-owned slab that has not been published.
#[must_use]
pub struct LoanedChunk<'producer> {
    producer: &'producer mut ChunkProducer,
    index: usize,
    length: usize,
    buffer: Option<Box<[u8]>>,
}

impl LoanedChunk<'_> {
    /// Returns the initialized payload prefix for application writes.
    pub fn payload_mut(&mut self) -> &mut [u8] {
        self.buffer
            .as_deref_mut()
            .and_then(|buffer| buffer.get_mut(..self.length))
            .unwrap_or_default()
    }

    /// Returns the initialized payload prefix.
    pub fn payload(&self) -> &[u8] {
        self.buffer
            .as_deref()
            .and_then(|buffer| buffer.get(..self.length))
            .unwrap_or_default()
    }

    /// Publishes the chunk to the ready queue.
    ///
    /// Dropping a loan instead returns its slab directly to the free queue.
    ///
    /// # Errors
    ///
    /// Returns `Internal` if another thread panicked while changing pool ownership.
    pub fn commit(mut self) -> Result<(), ErrorReport> {
        let mut state = lock(&self.producer.shared)?;
        state.slots[self.index] = self.buffer.take();
        state.loaned -= 1;
        state.ready.push_back((self.index, self.length));
        Ok(())
    }
}

impl Drop for LoanedChunk<'_> {
    fn drop(&mut self) {
        let Some(buffer) = self.buffer.take() else {
            return;
        };
        if let Ok(mut state) = self.producer.shared.state.lock() {
            state.slots[self.index] = Some(buffer);
            state.loaned -= 1;
            state.free.push_back(self.index);
        }
    }
}

/// The sole receive side of a chunk pool.
pub struct ChunkConsumer {
    shared: Arc<Shared>,
}

impl ChunkConsumer {
    /// Borrows the next committed payload in FIFO order.
    ///
    /// The slab cannot return to the producer until the receipt is dropped or released.
    ///
    /// # Errors
    ///
    /// Returns `Internal` if another thread panicked while changing pool ownership.
    pub fn receive(&mut self) -> Result<Option<ReceivedChunk<'_>>, ErrorReport> {
        let ready = {
            let mut state = lock(&self.shared)?;
            let Some((index, length)) = state.ready.pop_front() else {
                return Ok(None);
            };
            state.received += 1;
            let buffer = state.slots[index].take().ok_or_else(invalid_pool_state)?;
            (index, length, buffer)
        };
        Ok(Some(ReceivedChunk {
            consumer: self,
            index: ready.0,
            length: ready.1,
            buffer: Some(ready.2),
        }))
    }

    /// Returns the fixed payload capacity of every slab.
    pub fn chunk_capacity(&self) -> usize {
        self.shared.chunk_capacity
    }

    /// Returns a point-in-time ownership snapshot.
    ///
    /// # Errors
    ///
    /// Returns `Internal` if another thread panicked while changing pool ownership.
    pub fn diagnostics(&self) -> Result<PoolDiagnostics, ErrorReport> {
        diagnostics(&self.shared)
    }
}

/// One committed payload borrowed from the pool.
#[must_use]
pub struct ReceivedChunk<'consumer> {
    consumer: &'consumer mut ChunkConsumer,
    index: usize,
    length: usize,
    buffer: Option<Box<[u8]>>,
}

impl ReceivedChunk<'_> {
    /// Borrows the committed payload bytes.
    pub fn payload(&self) -> &[u8] {
        self.buffer
            .as_deref()
            .and_then(|buffer| buffer.get(..self.length))
            .unwrap_or_default()
    }

    /// Completes consumption and returns the slab to the free queue.
    ///
    /// Dropping the receipt has the same effect.
    pub fn release(self) {}
}

impl Drop for ReceivedChunk<'_> {
    fn drop(&mut self) {
        let Some(buffer) = self.buffer.take() else {
            return;
        };
        if let Ok(mut state) = self.consumer.shared.state.lock() {
            state.slots[self.index] = Some(buffer);
            state.received -= 1;
            state.free.push_back(self.index);
        }
    }
}

/// Point-in-time counts for every exclusive chunk ownership state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PoolDiagnostics {
    /// Total fixed slab count.
    pub chunk_count: usize,
    /// Slabs currently available to loan.
    pub free: usize,
    /// Slabs held by uncommitted producer loans.
    pub loaned: usize,
    /// Committed slabs waiting for the consumer.
    pub ready: usize,
    /// Slabs currently borrowed by the consumer.
    pub received: usize,
}

fn diagnostics(shared: &Shared) -> Result<PoolDiagnostics, ErrorReport> {
    let state = lock(shared)?;
    Ok(PoolDiagnostics {
        chunk_count: shared.chunk_count,
        free: state.free.len(),
        loaned: state.loaned,
        ready: state.ready.len(),
        received: state.received,
    })
}

fn lock(shared: &Shared) -> Result<MutexGuard<'_, State>, ErrorReport> {
    shared.state.lock().map_err(|_| invalid_pool_state())
}

fn invalid_configuration() -> ErrorReport {
    pool_error(
        ErrorCode::InvalidRange,
        Recoverability::Terminal,
        "configure chunk pool",
    )
}

fn allocation_failed() -> ErrorReport {
    pool_error(
        ErrorCode::Backpressured,
        Recoverability::Terminal,
        "allocate chunk pool",
    )
}

fn backpressured() -> ErrorReport {
    pool_error(
        ErrorCode::Backpressured,
        Recoverability::Retryable,
        "loan chunk",
    )
}

fn invalid_pool_state() -> ErrorReport {
    pool_error(
        ErrorCode::Internal,
        Recoverability::Terminal,
        "chunk pool ownership",
    )
}

fn pool_error(
    code: ErrorCode,
    recoverability: Recoverability,
    operation: &'static str,
) -> ErrorReport {
    ErrorReport::new(ErrorCategory::Resource, code, recoverability, operation)
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn loans_commits_borrows_and_releases_without_payload_copy() {
        let (mut producer, mut consumer) = chunk_pool(2, 16).unwrap();
        let mut loan = producer.loan(5).unwrap();
        loan.payload_mut().copy_from_slice(b"hello");
        loan.commit().unwrap();

        let received = consumer.receive().unwrap().unwrap();
        assert_eq!(received.payload(), b"hello");
        received.release();
        assert_eq!(
            producer.diagnostics().unwrap(),
            PoolDiagnostics {
                chunk_count: 2,
                free: 2,
                loaned: 0,
                ready: 0,
                received: 0,
            }
        );
    }

    #[test]
    fn dropped_loan_returns_capacity_and_ready_chunks_apply_backpressure() {
        let (mut producer, mut consumer) = chunk_pool(1, 4).unwrap();
        drop(producer.loan(4).unwrap());
        producer.loan(4).unwrap().commit().unwrap();
        let error = match producer.loan(1) {
            Ok(_) => panic!("expected backpressure"),
            Err(error) => error,
        };
        assert_eq!(error.code(), ErrorCode::Backpressured);
        assert_eq!(consumer.receive().unwrap().unwrap().payload(), &[0; 4]);
        assert!(producer.loan(1).is_ok());
    }

    #[test]
    fn rejects_invalid_configuration_and_oversized_loan() {
        assert!(chunk_pool(0, 1).is_err());
        assert!(chunk_pool(1, 0).is_err());
        let (mut producer, _) = chunk_pool(1, 4).unwrap();
        let error = match producer.loan(5) {
            Ok(_) => panic!("expected oversized loan rejection"),
            Err(error) => error,
        };
        assert_eq!(error.code(), ErrorCode::MessageTooLarge);
    }

    #[test]
    fn concurrent_spsc_preserves_fifo_and_recycles_fixed_slabs() {
        const COUNT: u32 = 1_000;
        let (mut producer, mut consumer) = chunk_pool(4, 4).unwrap();
        let producer_thread = thread::spawn(move || {
            for value in 0..COUNT {
                loop {
                    match producer.loan(4) {
                        Ok(mut loan) => {
                            loan.payload_mut().copy_from_slice(&value.to_le_bytes());
                            loan.commit().unwrap();
                            break;
                        }
                        Err(error) if error.code() == ErrorCode::Backpressured => {
                            thread::yield_now();
                        }
                        Err(error) => panic!("unexpected loan error: {error}"),
                    }
                }
            }
        });
        let consumer_thread = thread::spawn(move || {
            for expected in 0..COUNT {
                loop {
                    if let Some(received) = consumer.receive().unwrap() {
                        assert_eq!(received.payload(), expected.to_le_bytes());
                        break;
                    }
                    thread::yield_now();
                }
            }
            assert_eq!(consumer.diagnostics().unwrap().free, 4);
        });
        producer_thread.join().unwrap();
        consumer_thread.join().unwrap();
    }
}
