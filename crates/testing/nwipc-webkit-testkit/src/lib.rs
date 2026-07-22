//! Stable output contract for the real `WKWebView` process harness.

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};

/// Successful observations emitted by the native `AppKit` harness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WebKitE2eReport {
    /// The first `WebContent` process invoked `WKBundleInitialize`.
    pub initial_bundle_loaded: bool,
    /// A different `WebContent` process completed navigation after forced termination.
    pub replacement_process_observed: bool,
    /// The harness ran only after artifact runtime-flag validation.
    pub hardened_process: bool,
}

impl WebKitE2eReport {
    /// Parses the bounded one-line native harness contract.
    ///
    /// # Errors
    ///
    /// Rejects missing, unknown, or unsuccessful observations.
    pub fn parse(output: &str) -> Result<Self, ErrorReport> {
        let line = output
            .lines()
            .find(|line| line.starts_with("webkit-e2e: "))
            .ok_or_else(report_error)?;
        let report = Self {
            initial_bundle_loaded: line.contains("initial-load=ok"),
            replacement_process_observed: line.contains("replacement-process=ok"),
            hardened_process: line.contains("hardened-process=ok"),
        };
        if !report.initial_bundle_loaded
            || !report.replacement_process_observed
            || !report.hardened_process
        {
            return Err(report_error());
        }
        Ok(report)
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
    fn accepts_only_a_complete_process_report() {
        let report = WebKitE2eReport::parse(
            "webkit-e2e: initial-load=ok replacement-process=ok hardened-process=ok\n",
        )
        .unwrap();
        assert!(report.initial_bundle_loaded);
        assert!(WebKitE2eReport::parse("webkit-e2e: initial-load=ok").is_err());
    }
}
