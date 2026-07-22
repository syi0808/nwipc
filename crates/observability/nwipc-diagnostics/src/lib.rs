//! Redacted operational snapshots shared by public adapters.

use nwipc_capabilities::{TransportCapabilities, TransportTopology};
use nwipc_error::ErrorCode;
use nwipc_metrics::MetricsSnapshot;
use nwipc_state::SessionState;
use nwipc_types::{Generation, SessionId};

/// Stable memory backend name without a native descriptor or handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryBackend {
    /// Deterministic process-test memory.
    ProcessTest,
    /// macOS `IOSurface` memory.
    IoSurface,
}

/// Stable signal backend name without a notification name or token.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignalBackend {
    /// Poll-only correctness path.
    Poll,
    /// Darwin notifications plus correctness polling.
    Hybrid,
}

/// Redacted state of one public session generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionDiagnostics {
    /// Stable logical session identity.
    pub session_id: SessionId,
    /// Active resource generation.
    pub generation: Generation,
    /// Canonical lifecycle state.
    pub state: SessionState,
    /// Direct renderer-to-peer topology.
    pub topology: TransportTopology,
    /// Negotiated or configured transport capabilities.
    pub capabilities: TransportCapabilities,
    /// Selected memory backend.
    pub memory_backend: MemoryBackend,
    /// Selected notification backend.
    pub signal_backend: SignalBackend,
    /// Most recent stable failure code, if any.
    pub last_error: Option<ErrorCode>,
    /// Whether generation resources completed cleanup.
    pub resources_cleaned: bool,
}

/// Complete public diagnostics snapshot. Payloads, secrets, and native handles are absent by type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticsSnapshot {
    /// Schema version for forward-compatible consumers.
    pub schema_version: u16,
    /// Runtime-wide monotonic counters.
    pub metrics: MetricsSnapshot,
    /// Redacted active session entries.
    pub sessions: Vec<SessionDiagnostics>,
}

impl DiagnosticsSnapshot {
    /// Current diagnostics schema version.
    pub const SCHEMA_VERSION: u16 = 1;

    /// Creates a snapshot and sorts sessions by stable identity bytes.
    pub fn new(metrics: MetricsSnapshot, mut sessions: Vec<SessionDiagnostics>) -> Self {
        sessions.sort_by_key(|session| session.session_id.to_bytes());
        Self {
            schema_version: Self::SCHEMA_VERSION,
            metrics,
            sessions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_schema_contains_no_provider_secret_fields() {
        let snapshot = DiagnosticsSnapshot::new(MetricsSnapshot::default(), Vec::new());
        let debug = format!("{snapshot:?}");
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("descriptor"));
        assert!(!debug.contains("handle"));
    }
}
