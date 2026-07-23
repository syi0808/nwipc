//! Safe public facade for a native peer process.

use std::env;
use std::io::{self, Read, Write};

use nwipc_bootstrap_schema::ProviderKind;
use nwipc_capabilities::TransportCapabilities;
use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_macos_transport::{MacosEndpointTransport, production_capabilities};
use nwipc_peer_bootstrap::consume;
use nwipc_peer_core::{
    BorrowedPortEvent, NativePort, PeerExpectation, PeerPort, PortEvent, PortState, PortTransport,
};
use nwipc_types::{Generation, SessionId};

/// Default maximum logical application message.
pub const DEFAULT_MAXIMUM_MESSAGE: usize = 1024 * 1024;
const SESSION_ENV: &str = "NWIPC_SESSION_ID";
const GENERATION_ENV: &str = "NWIPC_GENERATION";
const PROTOCOL_ENV: &str = "NWIPC_PROTOCOL";

/// Native peer facade with provider details erased.
pub struct Peer {
    port: NativePort<Box<dyn PortTransport>>,
}

impl Peer {
    /// Consumes bootstrap from standard input and attaches the production memory/signal transport.
    ///
    /// The parent must set `NWIPC_SESSION_ID`, `NWIPC_GENERATION`, and `NWIPC_PROTOCOL`. Bootstrap
    /// validation occurs before HELLO or any provider activity.
    ///
    /// # Errors
    ///
    /// Returns a redacted configuration, bootstrap, or handshake error.
    pub fn initialize() -> Result<Self, ErrorReport> {
        let expectation = expectation_from_env()?;
        let mut reader = io::stdin();
        let envelope = consume(&mut reader)?;
        if envelope.memory().provider() == ProviderKind::ProcessTest
            && envelope.signal().provider() == ProviderKind::ProcessTest
        {
            let transport: Box<dyn PortTransport> = Box::new(StreamTransport {
                reader,
                writer: io::stdout(),
                closed: false,
            });
            let port =
                NativePort::attach(envelope, expectation, transport, DEFAULT_MAXIMUM_MESSAGE)?;
            return Ok(Self { port });
        }
        let transport: Box<dyn PortTransport> = Box::new(MacosEndpointTransport::attach(
            &envelope,
            nwipc_bootstrap_schema::EndpointRole::Peer,
        )?);
        let port = NativePort::accept(
            envelope,
            expectation,
            transport,
            DEFAULT_MAXIMUM_MESSAGE,
            production_capabilities(),
        )?;
        Ok(Self { port })
    }

    /// Opens a peer over owned framed streams. Intended for process adapters and tests.
    ///
    /// # Errors
    ///
    /// Returns a typed bootstrap or handshake error.
    pub fn from_streams(
        mut reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        expectation: PeerExpectation,
    ) -> Result<Self, ErrorReport> {
        let envelope = consume(&mut reader)?;
        let transport: Box<dyn PortTransport> = Box::new(StreamTransport {
            reader,
            writer,
            closed: false,
        });
        let port = NativePort::attach(envelope, expectation, transport, DEFAULT_MAXIMUM_MESSAGE)?;
        Ok(Self { port })
    }

    /// Sends one complete binary message.
    ///
    /// # Errors
    ///
    /// Returns a typed closed, size, backpressure, or transport failure.
    pub fn try_send(&mut self, payload: &[u8]) -> Result<(), ErrorReport> {
        self.port.try_send(payload)
    }

    /// Receives one complete message or close event.
    ///
    /// # Errors
    ///
    /// Returns a typed transport or protocol failure.
    pub fn try_receive(&mut self) -> Result<Option<PortEvent>, ErrorReport> {
        self.port.try_receive()
    }

    /// Receives an event while borrowing message bytes until the next mutable peer operation.
    ///
    /// # Errors
    ///
    /// Returns a typed transport or protocol failure.
    pub fn try_receive_borrowed(&mut self) -> Result<Option<BorrowedPortEvent<'_>>, ErrorReport> {
        self.port.try_receive_borrowed()
    }

    /// Gracefully and idempotently closes the peer.
    ///
    /// # Errors
    ///
    /// Returns the first transport cleanup failure.
    pub fn close(&mut self) -> Result<(), ErrorReport> {
        self.port.close()
    }

    /// Current peer port state.
    pub const fn state(&self) -> PortState {
        self.port.state()
    }

    /// Returns negotiated transport bits plus local borrowed-buffer API support.
    pub const fn capabilities(&self) -> TransportCapabilities {
        self.port.api_capabilities()
    }

    /// Runs a blocking binary echo loop until the parent closes.
    ///
    /// # Errors
    ///
    /// Returns the first send, receive, or close failure.
    pub fn run_echo(&mut self) -> Result<(), ErrorReport> {
        loop {
            match self.try_receive()? {
                Some(PortEvent::Message(payload)) => loop {
                    match self.try_send(&payload) {
                        Ok(()) => break,
                        Err(error) if error.code() == ErrorCode::Backpressured => {
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                        Err(error) => return Err(error),
                    }
                },
                Some(PortEvent::Closed) => return Ok(()),
                None => std::thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
    }
}

impl PeerPort for Peer {
    fn try_send(&mut self, payload: &[u8]) -> Result<(), ErrorReport> {
        Self::try_send(self, payload)
    }

    fn try_receive(&mut self) -> Result<Option<PortEvent>, ErrorReport> {
        Self::try_receive(self)
    }

    fn close(&mut self) -> Result<(), ErrorReport> {
        Self::close(self)
    }

    fn state(&self) -> PortState {
        Self::state(self)
    }
}

struct StreamTransport<R, W> {
    reader: R,
    writer: W,
    closed: bool,
}

impl<R: Read + Send, W: Write + Send> PortTransport for StreamTransport<R, W> {
    fn send(&mut self, frame: &[u8]) -> Result<(), ErrorReport> {
        if self.closed {
            return Err(stream_error(ErrorCode::Closed, "write peer frame"));
        }
        if frame.is_empty() || frame.len() > DEFAULT_MAXIMUM_MESSAGE + 64 {
            return Err(stream_error(ErrorCode::InvalidRange, "write peer frame"));
        }
        let length = u32::try_from(frame.len())
            .map_err(|_| stream_error(ErrorCode::InvalidRange, "write peer frame"))?;
        self.writer
            .write_all(&length.to_le_bytes())
            .and_then(|()| self.writer.write_all(frame))
            .and_then(|()| self.writer.flush())
            .map_err(|_| stream_error(ErrorCode::Closed, "write peer frame"))
    }

    fn receive(&mut self) -> Result<Option<Vec<u8>>, ErrorReport> {
        if self.closed {
            return Ok(None);
        }
        let mut prefix = [0; 4];
        let first = self
            .reader
            .read(&mut prefix[..1])
            .map_err(|_| stream_error(ErrorCode::Closed, "read peer frame"))?;
        if first == 0 {
            self.closed = true;
            return Err(stream_error(ErrorCode::Closed, "read peer frame"));
        }
        self.reader
            .read_exact(&mut prefix[1..])
            .map_err(|_| stream_error(ErrorCode::Truncated, "read peer frame"))?;
        let length = usize::try_from(u32::from_le_bytes(prefix))
            .map_err(|_| stream_error(ErrorCode::InvalidRange, "read peer frame"))?;
        if length == 0 || length > DEFAULT_MAXIMUM_MESSAGE + 64 {
            return Err(stream_error(ErrorCode::InvalidRange, "read peer frame"));
        }
        let mut frame = vec![0; length];
        self.reader
            .read_exact(&mut frame)
            .map_err(|_| stream_error(ErrorCode::Truncated, "read peer frame"))?;
        Ok(Some(frame))
    }

    fn close(&mut self) -> Result<(), ErrorReport> {
        self.closed = true;
        self.writer
            .flush()
            .map_err(|_| stream_error(ErrorCode::Closed, "close peer stream"))
    }
}

fn expectation_from_env() -> Result<PeerExpectation, ErrorReport> {
    let session = env::var(SESSION_ENV).map_err(|_| configuration_error(SESSION_ENV))?;
    let session_id = decode_session(&session).ok_or_else(|| configuration_error(SESSION_ENV))?;
    let generation = env::var(GENERATION_ENV)
        .ok()
        .and_then(|value| value.parse().ok())
        .and_then(Generation::new)
        .ok_or_else(|| configuration_error(GENERATION_ENV))?;
    let protocol = env::var(PROTOCOL_ENV)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value != 0)
        .ok_or_else(|| configuration_error(PROTOCOL_ENV))?;
    Ok(PeerExpectation {
        session_id,
        generation,
        protocol,
    })
}

fn decode_session(value: &str) -> Option<SessionId> {
    if value.len() != 32 {
        return None;
    }
    let mut bytes = [0; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    SessionId::from_bytes(bytes)
}

fn configuration_error(operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Configuration,
        ErrorCode::ProtocolViolation,
        Recoverability::Terminal,
        operation,
    )
}

fn stream_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        if code == ErrorCode::Closed {
            ErrorCategory::Closed
        } else {
            ErrorCategory::Protocol
        },
        code,
        Recoverability::ReplaceEndpoint,
        operation,
    )
}
