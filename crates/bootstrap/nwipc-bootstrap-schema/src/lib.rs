//! Provider-independent bootstrap domain model.

use std::fmt;

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_types::{Generation, SessionId};

/// Bootstrap schema emitted by this version of the library.
pub const SCHEMA_VERSION: u16 = 1;
/// Maximum encoded bootstrap envelope accepted by an endpoint.
pub const MAX_ENVELOPE_LENGTH: usize = 16 * 1024;
/// Maximum opaque descriptor payload.
pub const MAX_DESCRIPTOR_LENGTH: usize = 4 * 1024;
/// Maximum bootstrap authentication secret.
pub const MAX_SECRET_LENGTH: usize = 64;

/// Endpoint that is allowed to consume an envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum EndpointRole {
    /// Native peer process.
    Peer = 1,
    /// `WebKit` renderer process.
    Renderer = 2,
}

impl EndpointRole {
    /// Decodes the stable wire value.
    pub const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Peer),
            2 => Some(Self::Renderer),
            _ => None,
        }
    }
}

/// Supported protocol interval, inclusive at both ends.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolRange {
    minimum: u16,
    maximum: u16,
}

impl ProtocolRange {
    /// Creates a non-empty protocol interval.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRange` for zero or descending bounds.
    pub fn new(minimum: u16, maximum: u16) -> Result<Self, ErrorReport> {
        if minimum == 0 || minimum > maximum {
            return Err(schema_error(
                ErrorCode::InvalidRange,
                "bootstrap protocol range",
            ));
        }
        Ok(Self { minimum, maximum })
    }

    /// Lowest supported protocol version.
    pub const fn minimum(self) -> u16 {
        self.minimum
    }

    /// Highest supported protocol version.
    pub const fn maximum(self) -> u16 {
        self.maximum
    }

    /// Whether this interval contains a protocol version.
    pub const fn contains(self, version: u16) -> bool {
        version >= self.minimum && version <= self.maximum
    }
}

/// Provider selected by the host for a resource.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ProviderKind {
    /// Process-test provider; never selected by production platform adapters.
    ProcessTest = 1,
    /// `IOSurface` shared-memory provider.
    IoSurface = 2,
    /// Darwin notification provider.
    DarwinNotify = 3,
    /// Poll-only notification provider.
    Poll = 4,
    /// Hybrid Darwin notification and polling provider.
    Hybrid = 5,
}

impl ProviderKind {
    /// Decodes the stable wire value.
    pub const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::ProcessTest),
            2 => Some(Self::IoSurface),
            3 => Some(Self::DarwinNotify),
            4 => Some(Self::Poll),
            5 => Some(Self::Hybrid),
            _ => None,
        }
    }
}

/// A provider-tagged descriptor whose bytes are interpreted only by its adapter.
#[derive(Clone, Eq, PartialEq)]
pub struct OpaqueDescriptor {
    provider: ProviderKind,
    bytes: Vec<u8>,
}

impl OpaqueDescriptor {
    /// Creates a bounded non-empty descriptor.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRange` when the descriptor is empty or oversized.
    pub fn new(provider: ProviderKind, bytes: Vec<u8>) -> Result<Self, ErrorReport> {
        if bytes.is_empty() || bytes.len() > MAX_DESCRIPTOR_LENGTH {
            return Err(schema_error(
                ErrorCode::InvalidRange,
                "bootstrap descriptor length",
            ));
        }
        Ok(Self { provider, bytes })
    }

    /// Selected provider.
    pub const fn provider(&self) -> ProviderKind {
        self.provider
    }

    /// Opaque adapter input.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl fmt::Debug for OpaqueDescriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpaqueDescriptor")
            .field("provider", &self.provider)
            .field(
                "bytes",
                &format_args!("<redacted:{} bytes>", self.bytes.len()),
            )
            .finish()
    }
}

/// One-shot secret erased when its owner is dropped.
#[derive(Eq, PartialEq)]
pub struct BootstrapSecret(Vec<u8>);

impl BootstrapSecret {
    /// Creates a bounded non-empty bootstrap secret.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRange` when the secret is empty or oversized.
    pub fn new(bytes: Vec<u8>) -> Result<Self, ErrorReport> {
        if bytes.is_empty() || bytes.len() > MAX_SECRET_LENGTH {
            return Err(schema_error(
                ErrorCode::InvalidRange,
                "bootstrap secret length",
            ));
        }
        Ok(Self(bytes))
    }

    /// Borrows the secret only at an authentication boundary.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for BootstrapSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BootstrapSecret(<redacted>)")
    }
}

impl Drop for BootstrapSecret {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

/// Complete one-shot bootstrap envelope.
#[derive(Eq, PartialEq)]
pub struct BootstrapEnvelope {
    schema_version: u16,
    session_id: SessionId,
    generation: Generation,
    protocols: ProtocolRange,
    role: EndpointRole,
    memory: OpaqueDescriptor,
    signal: OpaqueDescriptor,
    secret: BootstrapSecret,
}

impl BootstrapEnvelope {
    /// Creates and validates an envelope using the current schema.
    ///
    /// # Errors
    ///
    /// Returns a bootstrap validation error for an invalid provider combination.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: SessionId,
        generation: Generation,
        protocols: ProtocolRange,
        role: EndpointRole,
        memory: OpaqueDescriptor,
        signal: OpaqueDescriptor,
        secret: BootstrapSecret,
    ) -> Result<Self, ErrorReport> {
        let envelope = Self {
            schema_version: SCHEMA_VERSION,
            session_id,
            generation,
            protocols,
            role,
            memory,
            signal,
            secret,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    /// Schema version carried by the envelope.
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }
    /// Session identity bound to the resources.
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }
    /// Resource generation bound to the resources.
    pub const fn generation(&self) -> Generation {
        self.generation
    }
    /// Offered protocol interval.
    pub const fn protocols(&self) -> ProtocolRange {
        self.protocols
    }
    /// Intended consumer role.
    pub const fn role(&self) -> EndpointRole {
        self.role
    }
    /// Shared-memory provider descriptor.
    pub const fn memory(&self) -> &OpaqueDescriptor {
        &self.memory
    }
    /// Notification provider descriptor.
    pub const fn signal(&self) -> &OpaqueDescriptor {
        &self.signal
    }
    /// One-shot authentication material.
    pub const fn secret(&self) -> &BootstrapSecret {
        &self.secret
    }

    /// Validates schema-level invariants.
    ///
    /// # Errors
    ///
    /// Returns a schema or provider mismatch.
    pub fn validate(&self) -> Result<(), ErrorReport> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(schema_error(
                ErrorCode::LayoutVersionMismatch,
                "bootstrap schema version",
            ));
        }
        if !matches!(
            self.memory.provider(),
            ProviderKind::ProcessTest | ProviderKind::IoSurface
        ) {
            return Err(schema_error(
                ErrorCode::ProtocolViolation,
                "bootstrap memory provider",
            ));
        }
        if matches!(self.signal.provider(), ProviderKind::IoSurface) {
            return Err(schema_error(
                ErrorCode::ProtocolViolation,
                "bootstrap signal provider",
            ));
        }
        Ok(())
    }

    /// Validates consumer identity before any provider is attached.
    ///
    /// # Errors
    ///
    /// Returns a role, session, generation, protocol, or provider mismatch.
    pub fn validate_for(
        &self,
        role: EndpointRole,
        session_id: SessionId,
        generation: Generation,
        protocol: u16,
    ) -> Result<(), ErrorReport> {
        self.validate()?;
        if self.role != role || self.session_id != session_id {
            return Err(schema_error(
                ErrorCode::ProtocolViolation,
                "bootstrap endpoint identity",
            ));
        }
        if self.generation != generation {
            return Err(schema_error(
                ErrorCode::StaleGeneration,
                "bootstrap generation",
            ));
        }
        if !self.protocols.contains(protocol) {
            return Err(schema_error(
                ErrorCode::LayoutVersionMismatch,
                "bootstrap protocol version",
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for BootstrapEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BootstrapEnvelope")
            .field("schema_version", &self.schema_version)
            .field("session_id", &self.session_id)
            .field("generation", &self.generation)
            .field("protocols", &self.protocols)
            .field("role", &self.role)
            .field("memory", &self.memory)
            .field("signal", &self.signal)
            .field("secret", &self.secret)
            .finish()
    }
}

fn schema_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Bootstrap,
        code,
        Recoverability::ReplaceEndpoint,
        operation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(generation: u64) -> BootstrapEnvelope {
        BootstrapEnvelope::new(
            SessionId::from_u128(7).unwrap(),
            Generation::new(generation).unwrap(),
            ProtocolRange::new(1, 2).unwrap(),
            EndpointRole::Peer,
            OpaqueDescriptor::new(ProviderKind::ProcessTest, vec![1]).unwrap(),
            OpaqueDescriptor::new(ProviderKind::ProcessTest, vec![2]).unwrap(),
            BootstrapSecret::new(vec![3; 16]).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn rejects_stale_generation_before_attach() {
        let error = envelope(1)
            .validate_for(
                EndpointRole::Peer,
                SessionId::from_u128(7).unwrap(),
                Generation::new(2).unwrap(),
                1,
            )
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::StaleGeneration);
    }

    #[test]
    fn debug_output_redacts_descriptors_and_secret() {
        let output = format!("{:?}", envelope(1));
        assert!(output.contains("redacted"));
        assert!(!output.contains("[3, 3"));
    }
}
