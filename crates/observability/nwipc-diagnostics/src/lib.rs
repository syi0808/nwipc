//! Redacted operational snapshots shared by public adapters.

use nwipc_capabilities::{TransportCapabilities, TransportTopology};
use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
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

/// Stable public operation stage associated with a failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureStage {
    /// Public configuration or provider selection.
    Configuration,
    /// Generation resource preparation.
    Preparation,
    /// Bootstrap transfer or decoding.
    Bootstrap,
    /// Protocol negotiation and identity validation.
    Handshake,
    /// Application data-plane operation.
    Transport,
    /// Lifecycle transition or generation routing.
    Lifecycle,
    /// Generation resource cleanup.
    Cleanup,
}

/// Redacted stable failure fields. Provider messages and source errors are intentionally absent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FailureDiagnostics {
    /// Boundary at which the failure was observed.
    pub stage: FailureStage,
    /// Broad stable policy category.
    pub category: ErrorCategory,
    /// Stable machine-readable failure code.
    pub code: ErrorCode,
    /// Stable recovery guidance.
    pub recoverability: Recoverability,
}

impl FailureDiagnostics {
    /// Redacts a public error report into the versioned diagnostics schema.
    pub const fn from_report(stage: FailureStage, report: &ErrorReport) -> Self {
        Self {
            stage,
            category: report.category(),
            code: report.code(),
            recoverability: report.recoverability(),
        }
    }
}

/// Observable cleanup result for one generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanupStatus {
    /// Resources are still owned by an active generation.
    Pending,
    /// Cleanup completed successfully and idempotently.
    Complete,
    /// Cleanup returned a stable failure.
    Failed,
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
    /// Full redacted failure fields for release diagnostics consumers.
    pub last_failure: Option<FailureDiagnostics>,
    /// Whether generation resources completed cleanup.
    pub resources_cleaned: bool,
    /// Unambiguous cleanup outcome for active, closed, and failed generations.
    pub cleanup: CleanupStatus,
}

/// Complete public diagnostics snapshot. Payloads, secrets, and native handles are absent by type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticsSnapshot {
    /// Schema version for forward-compatible consumers.
    pub schema_version: u16,
    /// Oldest reader schema which can safely consume this snapshot.
    pub minimum_compatible_schema_version: u16,
    /// Runtime-wide monotonic counters.
    pub metrics: MetricsSnapshot,
    /// Redacted active session entries.
    pub sessions: Vec<SessionDiagnostics>,
}

impl DiagnosticsSnapshot {
    /// Current diagnostics schema version.
    pub const SCHEMA_VERSION: u16 = 2;
    /// Oldest compatible reader for this schema. Version 2 is intentionally breaking from v1.
    pub const MINIMUM_COMPATIBLE_SCHEMA_VERSION: u16 = 2;

    /// Creates a snapshot and sorts sessions by stable identity bytes.
    pub fn new(metrics: MetricsSnapshot, mut sessions: Vec<SessionDiagnostics>) -> Self {
        sessions.sort_by_key(|session| (session.session_id.to_bytes(), session.generation.get()));
        Self {
            schema_version: Self::SCHEMA_VERSION,
            minimum_compatible_schema_version: Self::MINIMUM_COMPATIBLE_SCHEMA_VERSION,
            metrics,
            sessions,
        }
    }

    /// Returns whether a reader schema can consume this snapshot without guessing missing fields.
    pub const fn is_compatible_with(&self, reader_schema_version: u16) -> bool {
        reader_schema_version >= self.minimum_compatible_schema_version
            && reader_schema_version >= self.schema_version
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(session: u128, generation: u64) -> SessionDiagnostics {
        let error = ErrorReport::new(
            ErrorCategory::Protocol,
            ErrorCode::ProtocolViolation,
            Recoverability::ReplaceEndpoint,
            "provider secret must not be projected",
        );
        SessionDiagnostics {
            session_id: SessionId::from_u128(session).unwrap(),
            generation: Generation::new(generation).unwrap(),
            state: SessionState::Failed,
            topology: TransportTopology::direct(),
            capabilities: TransportCapabilities::NONE,
            memory_backend: MemoryBackend::IoSurface,
            signal_backend: SignalBackend::Hybrid,
            last_error: Some(error.code()),
            last_failure: Some(FailureDiagnostics::from_report(
                FailureStage::Handshake,
                &error,
            )),
            resources_cleaned: true,
            cleanup: CleanupStatus::Complete,
        }
    }

    #[test]
    fn debug_schema_contains_no_provider_secret_fields() {
        let snapshot = DiagnosticsSnapshot::new(MetricsSnapshot::default(), vec![session(1, 1)]);
        let debug = format!("{snapshot:?}");
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("descriptor"));
        assert!(!debug.contains("handle"));
        assert!(!debug.contains("provider secret must not be projected"));
    }

    #[test]
    fn schema_v2_rejects_legacy_readers() {
        let snapshot = DiagnosticsSnapshot::new(MetricsSnapshot::default(), Vec::new());
        assert!(!snapshot.is_compatible_with(1));
        assert!(snapshot.is_compatible_with(2));
        assert!(snapshot.is_compatible_with(3));
    }

    #[test]
    fn session_order_is_identity_then_generation() {
        let snapshot = DiagnosticsSnapshot::new(
            MetricsSnapshot::default(),
            vec![session(2, 1), session(1, 2), session(1, 1)],
        );
        let keys = snapshot
            .sessions
            .iter()
            .map(|entry| (entry.session_id.to_u128(), entry.generation.get()))
            .collect::<Vec<_>>();
        assert_eq!(keys, vec![(1, 1), (1, 2), (2, 1)]);
    }
}
