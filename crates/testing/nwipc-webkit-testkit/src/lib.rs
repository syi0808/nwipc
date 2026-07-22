//! Stable orchestration contract for the real `WKWebView` process harness.

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};

/// Canonical renderer bootstrap passed through the host process without decoding.
pub const RENDERER_BOOTSTRAP_ENV: &str = "NWIPC_WEBKIT_E2E_RENDERER_BOOTSTRAP";
/// Per-run Darwin notification posted after the production transport matrix succeeds.
pub const TRANSPORT_NOTIFICATION_ENV: &str = "NWIPC_WEBKIT_E2E_TRANSPORT_NOTIFICATION";
/// Exact production inline-message boundary exercised by the signed harness.
pub const EXACT_INLINE_LENGTH: usize = 16 * 1024;
/// Maximum negotiated logical message exercised by the signed harness.
pub const MAXIMUM_MESSAGE_LENGTH: usize = 1024 * 1024;
/// First payload length that requires production fragmentation.
pub const FRAGMENTED_MESSAGE_LENGTH: usize = EXACT_INLINE_LENGTH + 1;

/// Successful observations emitted by the native `AppKit` harness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WebKitE2eReport {
    observations: u8,
}

impl WebKitE2eReport {
    const INITIAL_BUNDLE: u8 = 1 << 0;
    const REPLACEMENT_PROCESS: u8 = 1 << 1;
    const HARDENED_PROCESS: u8 = 1 << 2;
    const PRODUCTION_TRANSPORT: u8 = 1 << 3;
    const BOUNDARIES: u8 = 1 << 4;
    const BACKPRESSURE: u8 = 1 << 5;
    const COMPLETE: u8 = Self::INITIAL_BUNDLE
        | Self::REPLACEMENT_PROCESS
        | Self::HARDENED_PROCESS
        | Self::PRODUCTION_TRANSPORT
        | Self::BOUNDARIES
        | Self::BACKPRESSURE;

    /// Parses the bounded one-line native harness contract.
    ///
    /// # Errors
    ///
    /// Rejects missing or unsuccessful observations.
    pub fn parse(output: &str) -> Result<Self, ErrorReport> {
        let line = output
            .lines()
            .find(|line| line.starts_with("webkit-e2e: "))
            .ok_or_else(report_error)?;
        let observations = [
            ("initial-load=ok", Self::INITIAL_BUNDLE),
            ("replacement-process=ok", Self::REPLACEMENT_PROCESS),
            ("hardened-process=ok", Self::HARDENED_PROCESS),
            ("production-transport=ok", Self::PRODUCTION_TRANSPORT),
            ("boundaries=ok", Self::BOUNDARIES),
            ("backpressure=ok", Self::BACKPRESSURE),
        ]
        .into_iter()
        .filter_map(|(marker, bit)| line.contains(marker).then_some(bit))
        .fold(0, |observations, bit| observations | bit);
        if observations != Self::COMPLETE {
            return Err(report_error());
        }
        Ok(Self { observations })
    }

    /// Whether the first `WebContent` process invoked `WKBundleInitialize`.
    pub const fn initial_bundle_loaded(self) -> bool {
        self.observations & Self::INITIAL_BUNDLE != 0
    }

    /// Whether a different `WebContent` process completed replacement navigation.
    pub const fn replacement_process_observed(self) -> bool {
        self.observations & Self::REPLACEMENT_PROCESS != 0
    }

    /// Whether hardened artifact inspection preceded process execution.
    pub const fn hardened_process(self) -> bool {
        self.observations & Self::HARDENED_PROCESS != 0
    }

    /// Whether the renderer and peer used the public production channel and handshake.
    pub const fn production_transport(self) -> bool {
        self.observations & Self::PRODUCTION_TRANSPORT != 0
    }

    /// Whether zero, exact-inline, fragmented, and maximum payloads round-tripped.
    pub const fn boundaries(self) -> bool {
        self.observations & Self::BOUNDARIES != 0
    }

    /// Whether saturation crossed backpressure and recovered a writable edge.
    pub const fn backpressure(self) -> bool {
        self.observations & Self::BACKPRESSURE != 0
    }
}

fn report_error() -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Platform,
        ErrorCode::ProtocolViolation,
        Recoverability::Terminal,
        "webkit e2e harness report",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_a_complete_production_report() {
        let report = WebKitE2eReport::parse(
            "webkit-e2e: initial-load=ok production-transport=ok boundaries=ok backpressure=ok replacement-process=ok hardened-process=ok\n",
        )
        .unwrap();
        assert!(report.initial_bundle_loaded());
        assert!(report.production_transport());
        assert!(report.boundaries());
        assert!(report.backpressure());
        assert!(WebKitE2eReport::parse("webkit-e2e: initial-load=ok").is_err());
    }
}
