//! Engine-independent contracts used by renderer runtimes and JavaScript bindings.

use nwipc_error::ErrorReport;
use nwipc_types::{DocumentGeneration, PortId};

/// The JavaScript document associated with a binding installation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RendererContext {
    /// Monotonically increasing identity assigned to the document.
    pub generation: DocumentGeneration,
}

/// Lifecycle of one generation-scoped renderer port.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortState {
    /// The transport is ready for application traffic.
    Open,
    /// A local close was requested and terminal acknowledgement is pending.
    Closing,
    /// The transport closed normally.
    Closed,
    /// The transport failed.
    Failed,
}

impl PortState {
    /// Returns whether the port accepts no further events or operations.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Closed | Self::Failed)
    }
}

/// Result of accepting a payload into the bounded native transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SendDisposition {
    /// The transport remains writable.
    Sent,
    /// The payload was accepted but the transport crossed its high watermark.
    Backpressured,
}

/// Event obtained by polling the native transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportEvent {
    /// One complete binary application message.
    Message(Vec<u8>),
    /// Previously reported backpressure has cleared.
    Writable,
    /// The remote endpoint closed normally.
    Closed,
    /// A terminal transport failure.
    Error(ErrorReport),
}

/// Native data-plane half owned by one renderer port.
pub trait RendererTransport {
    /// Accepts one complete binary message.
    ///
    /// # Errors
    ///
    /// Returns a typed transport, validation, or backpressure error.
    fn send(&mut self, payload: &[u8]) -> Result<SendDisposition, ErrorReport>;

    /// Returns the current number of committed outbound bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when shared transport state is invalid.
    fn buffered_amount(&self) -> Result<u32, ErrorReport>;

    /// Polls one event without relying on a signal for correctness.
    ///
    /// # Errors
    ///
    /// Returns a typed transport or validation error.
    fn poll(&mut self) -> Result<Option<TransportEvent>, ErrorReport>;

    /// Starts a graceful close.
    ///
    /// # Errors
    ///
    /// Returns a typed transport error when close cannot be queued.
    fn close(&mut self) -> Result<(), ErrorReport>;
}

/// Common lifecycle surface implemented by a renderer runtime.
pub trait RendererRuntime {
    /// Installs or replaces the active document binding.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle or platform installation error.
    fn install_binding(&mut self, context: RendererContext) -> Result<(), ErrorReport>;

    /// Invalidates all objects belonging to `generation`.
    fn invalidate_document(&mut self, generation: DocumentGeneration);

    /// Polls transports after a signal hint or safety tick.
    ///
    /// # Errors
    ///
    /// Returns a runtime dispatch error.
    fn dispatch_readable(&mut self) -> Result<(), ErrorReport>;
}

/// Identifies a callback registered for a port.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CallbackId(u64);

impl CallbackId {
    /// Creates an identifier from the runtime's monotonic counter.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the numeric identity.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Port identity paired with its document generation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DocumentPort {
    /// Owning document.
    pub generation: DocumentGeneration,
    /// Port identity unique within the runtime.
    pub port_id: PortId,
}
