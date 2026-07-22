//! Deterministic in-process memory and signal providers for contract tests.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_memory_api::{MappedRegion, MappingAccess, RegionDescriptor, SharedMemoryProvider};
use nwipc_signal_api::{SignalListener, SignalSender, WaitOutcome};
use nwipc_types::Generation;

/// An owned byte region standing in for a mapped shared-memory region.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FakeMappedRegion {
    bytes: Vec<u8>,
    writable: bool,
}

impl FakeMappedRegion {
    /// Creates a zero-filled region with explicit write access.
    pub fn new(length: usize, writable: bool) -> Self {
        Self {
            bytes: vec![0; length],
            writable,
        }
    }

    /// Returns the mapped length.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Returns whether this region has no mapped bytes.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Returns the current bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns writable bytes when the mapping permits writes.
    pub fn bytes_mut(&mut self) -> Option<&mut [u8]> {
        self.writable.then_some(self.bytes.as_mut_slice())
    }
}

/// Descriptor used by the provider-neutral shared-memory contract suite.
#[derive(Clone, Debug)]
pub struct FakeMemoryDescriptor {
    bytes: Arc<Mutex<Vec<u8>>>,
    byte_len: usize,
    generation: Generation,
}

impl RegionDescriptor for FakeMemoryDescriptor {
    fn byte_len(&self) -> usize {
        self.byte_len
    }

    fn generation(&self) -> Generation {
        self.generation
    }
}

/// Owned fake mapping which shares bytes with attached mappings.
#[derive(Debug)]
pub struct FakeMemoryMapping {
    bytes: Arc<Mutex<Vec<u8>>>,
    byte_len: usize,
    access: MappingAccess,
}

impl MappedRegion for FakeMemoryMapping {
    fn len(&self) -> usize {
        self.byte_len
    }

    fn access(&self) -> MappingAccess {
        self.access
    }

    fn read(&self, offset: usize, output: &mut [u8]) -> Result<(), ErrorReport> {
        let end = checked_end(offset, output.len(), self.byte_len, "read fake memory")?;
        let bytes = self
            .bytes
            .lock()
            .map_err(|_| fake_error(ErrorCode::Internal, "lock fake memory"))?;
        output.copy_from_slice(&bytes[offset..end]);
        Ok(())
    }

    fn write(&mut self, offset: usize, input: &[u8]) -> Result<(), ErrorReport> {
        if self.access != MappingAccess::ReadWrite {
            return Err(fake_error(
                ErrorCode::RequiredCapabilityMissing,
                "write read-only fake memory",
            ));
        }
        let end = checked_end(offset, input.len(), self.byte_len, "write fake memory")?;
        let mut bytes = self
            .bytes
            .lock()
            .map_err(|_| fake_error(ErrorCode::Internal, "lock fake memory"))?;
        bytes[offset..end].copy_from_slice(input);
        Ok(())
    }
}

/// Deterministic implementation of the platform-neutral memory contract.
#[derive(Clone, Copy, Debug, Default)]
pub struct FakeMemoryProvider;

impl SharedMemoryProvider for FakeMemoryProvider {
    type Descriptor = FakeMemoryDescriptor;
    type Mapping = FakeMemoryMapping;

    fn create(
        &self,
        byte_len: usize,
        generation: Generation,
    ) -> Result<(Self::Mapping, Self::Descriptor), ErrorReport> {
        if byte_len == 0 {
            return Err(fake_error(ErrorCode::InvalidRange, "create fake memory"));
        }
        let bytes = Arc::new(Mutex::new(vec![0; byte_len]));
        Ok((
            FakeMemoryMapping {
                bytes: Arc::clone(&bytes),
                byte_len,
                access: MappingAccess::ReadWrite,
            },
            FakeMemoryDescriptor {
                bytes,
                byte_len,
                generation,
            },
        ))
    }

    fn attach(
        &self,
        descriptor: &Self::Descriptor,
        expected_generation: Generation,
        access: MappingAccess,
    ) -> Result<Self::Mapping, ErrorReport> {
        if descriptor.generation != expected_generation {
            return Err(fake_error(ErrorCode::StaleGeneration, "attach fake memory"));
        }
        Ok(FakeMemoryMapping {
            bytes: Arc::clone(&descriptor.bytes),
            byte_len: descriptor.byte_len,
            access,
        })
    }
}

/// Controls how a deterministic signal transforms notification hints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FakeSignalMode {
    /// Retain every notification.
    Deliver,
    /// Retain at most one outstanding notification.
    Coalesce,
    /// Drop all notifications.
    Drop,
}

/// A deterministic signal queue that can deliver, coalesce, or drop hints.
#[derive(Debug)]
pub struct FakeSignal {
    mode: FakeSignalMode,
    pending: VecDeque<()>,
}

impl FakeSignal {
    /// Creates a signal with the selected fault behavior.
    pub fn new(mode: FakeSignalMode) -> Self {
        Self {
            mode,
            pending: VecDeque::new(),
        }
    }

    /// Posts one notification hint.
    pub fn notify(&mut self) {
        match self.mode {
            FakeSignalMode::Deliver => self.pending.push_back(()),
            FakeSignalMode::Coalesce if self.pending.is_empty() => self.pending.push_back(()),
            FakeSignalMode::Coalesce | FakeSignalMode::Drop => {}
        }
    }

    /// Consumes an outstanding hint without blocking.
    pub fn try_wait(&mut self) -> bool {
        self.pending.pop_front().is_some()
    }

    /// Returns the number of retained hints for fault-injection assertions.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

/// Sender half of a thread-safe deterministic signal provider.
#[derive(Clone, Debug)]
pub struct FakeSignalSender {
    signal: Arc<Mutex<FakeSignal>>,
}

impl SignalSender for FakeSignalSender {
    fn notify(&self) -> Result<(), ErrorReport> {
        self.signal
            .lock()
            .map_err(|_| fake_error(ErrorCode::Internal, "lock fake signal"))?
            .notify();
        Ok(())
    }
}

/// Listener half of a deterministic signal provider.
#[derive(Debug)]
pub struct FakeSignalListener {
    signal: Arc<Mutex<FakeSignal>>,
    cancelled: bool,
}

impl SignalListener for FakeSignalListener {
    fn try_wait(&mut self) -> Result<WaitOutcome, ErrorReport> {
        if self.cancelled {
            return Ok(WaitOutcome::Cancelled);
        }
        let signaled = self
            .signal
            .lock()
            .map_err(|_| fake_error(ErrorCode::Internal, "lock fake signal"))?
            .try_wait();
        Ok(if signaled {
            WaitOutcome::Signaled
        } else {
            WaitOutcome::TimedOut
        })
    }

    fn wait_timeout(&mut self, timeout: Duration) -> Result<WaitOutcome, ErrorReport> {
        let deadline = Instant::now() + timeout;
        loop {
            let outcome = self.try_wait()?;
            if outcome != WaitOutcome::TimedOut || Instant::now() >= deadline {
                return Ok(outcome);
            }
            std::thread::sleep(Duration::from_millis(1).min(timeout));
        }
    }

    fn cancel(&mut self) {
        self.cancelled = true;
    }
}

/// Creates connected deterministic sender and listener halves.
pub fn fake_signal_pair(mode: FakeSignalMode) -> (FakeSignalSender, FakeSignalListener) {
    let signal = Arc::new(Mutex::new(FakeSignal::new(mode)));
    (
        FakeSignalSender {
            signal: Arc::clone(&signal),
        },
        FakeSignalListener {
            signal,
            cancelled: false,
        },
    )
}

fn checked_end(
    offset: usize,
    length: usize,
    byte_len: usize,
    operation: &'static str,
) -> Result<usize, ErrorReport> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| fake_error(ErrorCode::InvalidRange, operation))?;
    if end > byte_len {
        return Err(fake_error(ErrorCode::InvalidRange, operation));
    }
    Ok(end)
}

fn fake_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Internal,
        code,
        if code == ErrorCode::StaleGeneration {
            Recoverability::ReplaceEndpoint
        } else {
            Recoverability::Terminal
        },
        operation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_region_rejects_mutable_view() {
        assert!(FakeMappedRegion::new(8, false).bytes_mut().is_none());
    }

    #[test]
    fn coalescing_retains_one_hint() {
        let mut signal = FakeSignal::new(FakeSignalMode::Coalesce);
        signal.notify();
        signal.notify();
        assert!(signal.try_wait());
        assert!(!signal.try_wait());
    }

    #[test]
    fn fake_memory_obeys_attach_access_and_generation_contract() {
        let generation = Generation::new(1).unwrap();
        let (mut owner, descriptor) = FakeMemoryProvider.create(16, generation).unwrap();
        owner.write(4, b"test").unwrap();
        let attached = FakeMemoryProvider
            .attach(&descriptor, generation, MappingAccess::ReadOnly)
            .unwrap();
        let mut output = [0; 4];
        attached.read(4, &mut output).unwrap();
        assert_eq!(&output, b"test");
        assert_eq!(
            FakeMemoryProvider
                .attach(
                    &descriptor,
                    Generation::new(2).unwrap(),
                    MappingAccess::ReadOnly,
                )
                .unwrap_err()
                .code(),
            ErrorCode::StaleGeneration
        );
    }

    #[test]
    fn fake_signal_obeys_duplicate_drop_and_cancel_contract() {
        let (sender, mut listener) = fake_signal_pair(FakeSignalMode::Coalesce);
        sender.notify().unwrap();
        sender.notify().unwrap();
        assert_eq!(listener.try_wait().unwrap(), WaitOutcome::Signaled);
        assert_eq!(listener.try_wait().unwrap(), WaitOutcome::TimedOut);
        listener.cancel();
        assert_eq!(listener.try_wait().unwrap(), WaitOutcome::Cancelled);

        let (sender, mut listener) = fake_signal_pair(FakeSignalMode::Drop);
        sender.notify().unwrap();
        assert_eq!(listener.try_wait().unwrap(), WaitOutcome::TimedOut);
    }
}
