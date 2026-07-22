//! Native two-process bootstrap and echo harness.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use nwipc_bootstrap_schema::{
    BootstrapEnvelope, BootstrapSecret, EndpointRole, OpaqueDescriptor, ProtocolRange, ProviderKind,
};
use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_peer_bootstrap::write_envelope;
use nwipc_peer_core::{PeerExpectation, acknowledge_hello};
use nwipc_types::{Generation, SessionId};

const MAXIMUM_FRAME: usize = 1024 * 1024 + 64;
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

/// Parent-side process harness configuration.
#[derive(Clone, Copy, Debug)]
pub struct ProcessHarness {
    timeout: Duration,
}

impl ProcessHarness {
    /// Creates a harness with a bounded per-operation timeout.
    pub const fn new(timeout: Duration) -> Self {
        Self { timeout }
    }

    /// Spawns, bootstraps, and handshakes with a native peer executable.
    ///
    /// # Errors
    ///
    /// Returns a typed process, bootstrap, timeout, or handshake error.
    pub fn spawn(&self, executable: impl AsRef<Path>) -> Result<ProcessPeer, ErrorReport> {
        let generation = Generation::new(1)
            .ok_or_else(|| process_error(ErrorCode::Internal, "create initial generation"))?;
        self.spawn_with_expected_generation(executable, generation)
    }

    /// Spawns with a caller-selected child expectation, useful for stale-generation tests.
    ///
    /// # Errors
    ///
    /// A generation other than one is rejected by the child before HELLO.
    pub fn spawn_with_expected_generation(
        &self,
        executable: impl AsRef<Path>,
        child_generation: Generation,
    ) -> Result<ProcessPeer, ErrorReport> {
        let session_id = next_session_id();
        let generation = Generation::new(1)
            .ok_or_else(|| process_error(ErrorCode::Internal, "create initial generation"))?;
        let expectation = PeerExpectation {
            session_id,
            generation,
            protocol: 1,
        };
        let secret = session_id.to_bytes().to_vec();
        let envelope = BootstrapEnvelope::new(
            session_id,
            generation,
            ProtocolRange::new(1, 1)?,
            EndpointRole::Peer,
            OpaqueDescriptor::new(ProviderKind::ProcessTest, b"process-memory".to_vec())?,
            OpaqueDescriptor::new(ProviderKind::ProcessTest, b"process-signal".to_vec())?,
            BootstrapSecret::new(secret.clone())?,
        )?;
        let mut child = Command::new(executable.as_ref())
            .env("NWIPC_SESSION_ID", encode_session(session_id))
            .env("NWIPC_GENERATION", child_generation.get().to_string())
            .env("NWIPC_PROTOCOL", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| process_error(ErrorCode::Internal, "spawn native peer"))?;
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| process_error(ErrorCode::Internal, "open peer bootstrap pipe"))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| process_error(ErrorCode::Internal, "open peer output pipe"))?;
        if let Err(error) = write_envelope(&mut input, &envelope) {
            cleanup_child(&mut child);
            return Err(error);
        }
        let (output, hello) = read_frame_with_timeout(output, self.timeout).inspect_err(|_| {
            cleanup_child(&mut child);
        })?;
        let acknowledgement =
            acknowledge_hello(&hello, expectation, &secret).inspect_err(|_| {
                cleanup_child(&mut child);
            })?;
        write_frame(&mut input, &acknowledgement).inspect_err(|_| {
            cleanup_child(&mut child);
        })?;
        Ok(ProcessPeer {
            child,
            input,
            output: Some(output),
            timeout: self.timeout,
            expectation,
            closed: false,
        })
    }
}

impl Default for ProcessHarness {
    fn default() -> Self {
        Self::new(Duration::from_secs(2))
    }
}

/// Live parent-side handle for one native child.
pub struct ProcessPeer {
    child: Child,
    input: ChildStdin,
    output: Option<ChildStdout>,
    timeout: Duration,
    expectation: PeerExpectation,
    closed: bool,
}

impl ProcessPeer {
    /// Sends binary data and waits for the exact echoed frame.
    ///
    /// # Errors
    ///
    /// Returns a typed timeout, framing, or echo mismatch error.
    pub fn echo(&mut self, payload: &[u8]) -> Result<Vec<u8>, ErrorReport> {
        if self.closed {
            return Err(process_error(ErrorCode::Closed, "process peer echo"));
        }
        if payload.len() + 1 > MAXIMUM_FRAME {
            return Err(process_error(
                ErrorCode::MessageTooLarge,
                "process peer echo",
            ));
        }
        let mut frame = Vec::with_capacity(payload.len() + 1);
        frame.push(0);
        frame.extend_from_slice(payload);
        write_frame(&mut self.input, &frame)?;
        let output = self
            .output
            .take()
            .ok_or_else(|| process_error(ErrorCode::Closed, "process peer output"))?;
        let (output, echoed) = read_frame_with_timeout(output, self.timeout)?;
        self.output = Some(output);
        let Some((&kind, echoed_payload)) = echoed.split_first() else {
            return Err(process_error(
                ErrorCode::ProtocolViolation,
                "process peer echo frame",
            ));
        };
        if kind != 0 || echoed_payload != payload {
            return Err(process_error(
                ErrorCode::ProtocolViolation,
                "process peer echo mismatch",
            ));
        }
        Ok(echoed_payload.to_vec())
    }

    /// Current session and generation identity.
    pub const fn expectation(&self) -> PeerExpectation {
        self.expectation
    }

    /// Gracefully closes and waits for the child, killing it on timeout.
    ///
    /// # Errors
    ///
    /// Returns a typed close or non-successful-exit error.
    pub fn close(mut self) -> Result<ExitStatus, ErrorReport> {
        if !self.closed {
            write_frame(&mut self.input, &[1])?;
            self.closed = true;
        }
        let status = wait_child(&mut self.child, self.timeout)?;
        if !status.success() {
            return Err(process_error(
                ErrorCode::ProtocolViolation,
                "native peer exit status",
            ));
        }
        Ok(status)
    }

    /// Abruptly kills and reaps the child to exercise crash cleanup.
    ///
    /// # Errors
    ///
    /// Returns a redacted platform error if kill or wait fails.
    pub fn kill(mut self) -> Result<ExitStatus, ErrorReport> {
        self.child
            .kill()
            .map_err(|_| process_error(ErrorCode::Internal, "kill native peer"))?;
        self.child
            .wait()
            .map_err(|_| process_error(ErrorCode::Internal, "reap native peer"))
    }
}

impl Drop for ProcessPeer {
    fn drop(&mut self) {
        if !self.closed {
            cleanup_child(&mut self.child);
        }
    }
}

fn next_session_id() -> SessionId {
    let counter = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
    let value = (u128::from(std::process::id()) << 64) | u128::from(counter);
    SessionId::from_u128(value).expect("process and counter cannot both be zero")
}

fn encode_session(session_id: SessionId) -> String {
    let mut output = String::with_capacity(32);
    for byte in session_id.to_bytes() {
        use std::fmt::Write as _;
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn write_frame(writer: &mut impl Write, frame: &[u8]) -> Result<(), ErrorReport> {
    if frame.is_empty() || frame.len() > MAXIMUM_FRAME {
        return Err(process_error(
            ErrorCode::InvalidRange,
            "write process frame",
        ));
    }
    let length = u32::try_from(frame.len())
        .map_err(|_| process_error(ErrorCode::InvalidRange, "write process frame"))?;
    writer
        .write_all(&length.to_le_bytes())
        .and_then(|()| writer.write_all(frame))
        .and_then(|()| writer.flush())
        .map_err(|_| process_error(ErrorCode::Closed, "write process frame"))
}

fn read_frame_with_timeout(
    mut output: ChildStdout,
    timeout: Duration,
) -> Result<(ChildStdout, Vec<u8>), ErrorReport> {
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("nwipc-process-read".into())
        .spawn(move || {
            let result = read_frame(&mut output);
            let _ = sender.send((output, result));
        })
        .map_err(|_| process_error(ErrorCode::Internal, "spawn process reader"))?;
    let (output, result) = receiver
        .recv_timeout(timeout)
        .map_err(|_| process_error(ErrorCode::Timeout, "read process frame timeout"))?;
    result.map(|frame| (output, frame))
}

fn read_frame(reader: &mut impl Read) -> Result<Vec<u8>, ErrorReport> {
    let mut prefix = [0; 4];
    reader
        .read_exact(&mut prefix)
        .map_err(|_| process_error(ErrorCode::Truncated, "read process frame"))?;
    let length = usize::try_from(u32::from_le_bytes(prefix))
        .map_err(|_| process_error(ErrorCode::InvalidRange, "read process frame"))?;
    if length == 0 || length > MAXIMUM_FRAME {
        return Err(process_error(ErrorCode::InvalidRange, "read process frame"));
    }
    let mut frame = vec![0; length];
    reader
        .read_exact(&mut frame)
        .map_err(|_| process_error(ErrorCode::Truncated, "read process frame"))?;
    Ok(frame)
}

fn wait_child(child: &mut Child, timeout: Duration) -> Result<ExitStatus, ErrorReport> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|_| process_error(ErrorCode::Internal, "wait native peer"))?
        {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            cleanup_child(child);
            return Err(process_error(
                ErrorCode::Timeout,
                "close native peer timeout",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn cleanup_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn process_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    let category = match code {
        ErrorCode::Timeout => ErrorCategory::Timeout,
        ErrorCode::Closed => ErrorCategory::Closed,
        ErrorCode::MessageTooLarge => ErrorCategory::Resource,
        _ => ErrorCategory::Bootstrap,
    };
    ErrorReport::new(category, code, Recoverability::ReplaceEndpoint, operation)
}
