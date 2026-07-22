//! Length-prefixed, one-shot bootstrap transport for an inherited anonymous pipe.

use std::io::{self, Read, Write};
use std::sync::mpsc;
use std::time::Duration;

use nwipc_bootstrap_codec::{decode, encode};
use nwipc_bootstrap_schema::{BootstrapEnvelope, MAX_ENVELOPE_LENGTH};
use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};

const LENGTH_PREFIX: usize = 4;

/// Writes exactly one framed bootstrap envelope.
///
/// # Errors
///
/// Returns a typed bootstrap error when encoding or pipe output fails.
pub fn write_envelope(
    writer: &mut impl Write,
    envelope: &BootstrapEnvelope,
) -> Result<(), ErrorReport> {
    let encoded = encode(envelope)?;
    write_frame(writer, &encoded)
}

/// Reads and consumes exactly one framed bootstrap envelope.
///
/// The reader is taken by value so descriptor ownership cannot accidentally be reused.
///
/// # Errors
///
/// Returns a typed bootstrap error for early EOF, oversized input, or malformed envelopes.
pub fn consume(reader: impl Read) -> Result<BootstrapEnvelope, ErrorReport> {
    decode(&read_frame(reader)?)
}

/// Reads a one-shot envelope with a bounded deadline.
///
/// The owned reader stays in the worker and is closed when that read completes. Production
/// adapters should additionally configure an OS read timeout on their inherited descriptor.
///
/// # Errors
///
/// Returns `Timeout` when the deadline expires before an entire envelope is available.
pub fn consume_with_timeout(
    reader: impl Read + Send + 'static,
    timeout: Duration,
) -> Result<BootstrapEnvelope, ErrorReport> {
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("nwipc-bootstrap-read".into())
        .spawn(move || {
            let _ = sender.send(consume(reader));
        })
        .map_err(|_| bootstrap_error(ErrorCode::Internal, "spawn bootstrap reader"))?;
    receiver
        .recv_timeout(timeout)
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => {
                bootstrap_error(ErrorCode::Timeout, "read bootstrap timeout")
            }
            mpsc::RecvTimeoutError::Disconnected => {
                bootstrap_error(ErrorCode::Internal, "read bootstrap worker")
            }
        })?
}

/// Writes a bounded length-prefixed binary frame.
///
/// # Errors
///
/// Returns `InvalidRange` or a redacted I/O failure.
pub fn write_frame(writer: &mut impl Write, payload: &[u8]) -> Result<(), ErrorReport> {
    if payload.is_empty() || payload.len() > MAX_ENVELOPE_LENGTH {
        return Err(bootstrap_error(
            ErrorCode::InvalidRange,
            "write bootstrap length",
        ));
    }
    let length = u32::try_from(payload.len())
        .map_err(|_| bootstrap_error(ErrorCode::InvalidRange, "write bootstrap length"))?;
    writer
        .write_all(&length.to_le_bytes())
        .and_then(|()| writer.write_all(payload))
        .and_then(|()| writer.flush())
        .map_err(|error| io_error(&error, "write bootstrap pipe"))
}

fn read_frame(mut reader: impl Read) -> Result<Vec<u8>, ErrorReport> {
    let mut prefix = [0; LENGTH_PREFIX];
    reader
        .read_exact(&mut prefix)
        .map_err(|error| io_error(&error, "read bootstrap length"))?;
    let length = usize::try_from(u32::from_le_bytes(prefix))
        .map_err(|_| bootstrap_error(ErrorCode::InvalidRange, "read bootstrap length"))?;
    if length == 0 || length > MAX_ENVELOPE_LENGTH {
        return Err(bootstrap_error(
            ErrorCode::InvalidRange,
            "read bootstrap length",
        ));
    }
    let mut payload = vec![0; length];
    reader
        .read_exact(&mut payload)
        .map_err(|error| io_error(&error, "read bootstrap body"))?;
    Ok(payload)
}

fn io_error(error: &io::Error, operation: &'static str) -> ErrorReport {
    let code = match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => ErrorCode::Timeout,
        io::ErrorKind::UnexpectedEof => ErrorCode::Truncated,
        _ => ErrorCode::ProtocolViolation,
    };
    bootstrap_error(code, operation)
}

fn bootstrap_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        if code == ErrorCode::Timeout {
            ErrorCategory::Timeout
        } else {
            ErrorCategory::Bootstrap
        },
        code,
        Recoverability::ReplaceEndpoint,
        operation,
    )
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use nwipc_bootstrap_schema::{
        BootstrapSecret, EndpointRole, OpaqueDescriptor, ProtocolRange, ProviderKind,
    };
    use nwipc_types::{Generation, SessionId};

    use super::*;

    fn envelope() -> BootstrapEnvelope {
        BootstrapEnvelope::new(
            SessionId::from_u128(1).unwrap(),
            Generation::new(1).unwrap(),
            ProtocolRange::new(1, 1).unwrap(),
            EndpointRole::Peer,
            OpaqueDescriptor::new(ProviderKind::ProcessTest, vec![1]).unwrap(),
            OpaqueDescriptor::new(ProviderKind::ProcessTest, vec![2]).unwrap(),
            BootstrapSecret::new(vec![3; 16]).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn accepts_partial_pipe_reads() {
        struct OneByte(Cursor<Vec<u8>>);
        impl Read for OneByte {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                let length = output.len().min(1);
                self.0.read(&mut output[..length])
            }
        }

        let mut bytes = Vec::new();
        write_envelope(&mut bytes, &envelope()).unwrap();
        assert_eq!(consume(OneByte(Cursor::new(bytes))).unwrap(), envelope());
    }

    #[test]
    fn rejects_early_eof_and_oversize() {
        assert_eq!(
            consume(Cursor::new(vec![1, 0])).unwrap_err().code(),
            ErrorCode::Truncated
        );
        let oversized = u32::try_from(MAX_ENVELOPE_LENGTH + 1)
            .unwrap()
            .to_le_bytes();
        assert_eq!(
            consume(Cursor::new(oversized)).unwrap_err().code(),
            ErrorCode::InvalidRange
        );
    }

    #[test]
    fn timeout_is_bounded() {
        struct Slow;
        impl Read for Slow {
            fn read(&mut self, _output: &mut [u8]) -> io::Result<usize> {
                std::thread::sleep(Duration::from_millis(100));
                Ok(0)
            }
        }
        let error = consume_with_timeout(Slow, Duration::from_millis(5)).unwrap_err();
        assert_eq!(error.code(), ErrorCode::Timeout);
    }
}
