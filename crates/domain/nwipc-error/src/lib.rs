//! Stable, redacted errors exposed across NWIPC public boundaries.

use std::fmt;

/// Broad error category suitable for policy decisions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorCategory {
    /// The requested provider or operation is unavailable.
    Unsupported,
    /// Input violated a protocol or state contract.
    Protocol,
    /// A resource could not be acquired or retained.
    Resource,
    /// An operation exceeded its bounded deadline.
    Timeout,
    /// The endpoint has closed.
    Closed,
    /// An internal invariant failed without exposing implementation data.
    Internal,
}

/// Stable machine-readable error codes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum ErrorCode {
    /// The requested capability is not implemented on this build or platform.
    Unsupported = 1,
    /// A required capability was not negotiated.
    RequiredCapabilityMissing = 2,
    /// A state-machine transition was rejected.
    InvalidStateTransition = 3,
    /// A protocol value was malformed or inconsistent.
    ProtocolViolation = 4,
    /// A bounded operation timed out.
    Timeout = 5,
    /// The endpoint is closed.
    Closed = 6,
    /// An internal invariant failed.
    Internal = 7,
}

/// Whether retrying or replacing the endpoint can recover an operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Recoverability {
    /// Retrying the same operation may succeed.
    Retryable,
    /// A new endpoint generation is required.
    ReplaceEndpoint,
    /// The failure is terminal for the current request.
    Terminal,
}

/// A redacted error value safe to expose at public API boundaries.
#[derive(Clone, Eq, PartialEq)]
pub struct ErrorReport {
    category: ErrorCategory,
    code: ErrorCode,
    recoverability: Recoverability,
    context: &'static str,
}

impl ErrorReport {
    /// Creates a report from stable public fields.
    pub const fn new(
        category: ErrorCategory,
        code: ErrorCode,
        recoverability: Recoverability,
        context: &'static str,
    ) -> Self {
        Self {
            category,
            code,
            recoverability,
            context,
        }
    }

    /// Creates an explicit unsupported-operation report.
    pub const fn unsupported(context: &'static str) -> Self {
        Self::new(
            ErrorCategory::Unsupported,
            ErrorCode::Unsupported,
            Recoverability::Terminal,
            context,
        )
    }

    /// Returns the broad category.
    pub const fn category(&self) -> ErrorCategory {
        self.category
    }

    /// Returns the stable machine-readable code.
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    /// Returns recovery guidance.
    pub const fn recoverability(&self) -> Recoverability {
        self.recoverability
    }
}

impl fmt::Debug for ErrorReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ErrorReport")
            .field("category", &self.category)
            .field("code", &self.code)
            .field("recoverability", &self.recoverability)
            .field("context", &self.context)
            .finish()
    }
}

impl fmt::Display for ErrorReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({:?})", self.context, self.code)
    }
}

impl std::error::Error for ErrorReport {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_is_stable_and_typed() {
        let report = ErrorReport::unsupported("provider");
        assert_eq!(report.category(), ErrorCategory::Unsupported);
        assert_eq!(report.code(), ErrorCode::Unsupported);
        assert_eq!(report.to_string(), "provider (Unsupported)");
    }
}
