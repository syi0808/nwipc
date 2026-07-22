//! Stable, allocation-free, redacted errors exposed across NWIPC boundaries.

use std::fmt;

/// Broad error category suitable for policy decisions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorCategory {
    /// The requested operation is unavailable.
    Unsupported,
    /// Configuration is missing or inconsistent.
    Configuration,
    /// Endpoint bootstrap failed.
    Bootstrap,
    /// Shared memory acquisition or mapping failed.
    Memory,
    /// Signal delivery or observation failed.
    Signal,
    /// Input violated a wire contract.
    Protocol,
    /// A lifecycle transition was rejected.
    Lifecycle,
    /// Authentication or trust validation failed.
    Security,
    /// A platform operation failed.
    Platform,
    /// A resource could not be acquired or retained.
    Resource,
    /// An operation exceeded its bounded deadline.
    Timeout,
    /// The endpoint has closed.
    Closed,
    /// An internal invariant failed.
    Internal,
}

/// Stable machine-readable error codes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u16)]
pub enum ErrorCode {
    /// The requested operation is unsupported.
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
    /// A fixed wire buffer is truncated.
    Truncated = 8,
    /// A wire length or offset is outside the permitted range.
    InvalidRange = 9,
    /// A wire value is not aligned as required.
    InvalidAlignment = 10,
    /// Region magic does not identify NWIPC.
    InvalidMagic = 11,
    /// A layout version cannot be interpreted.
    LayoutVersionMismatch = 12,
    /// A byte-order marker cannot be interpreted.
    ByteOrderMismatch = 13,
    /// A generation does not identify the active resource.
    StaleGeneration = 14,
    /// A required record flag is unknown.
    UnknownRequiredFlag = 15,
    /// A message exceeds the configured inline limit.
    MessageTooLarge = 16,
    /// A bounded buffer cannot currently accept the operation.
    Backpressured = 17,
    /// Shared state contains an impossible cursor distance.
    InvalidCursor = 18,
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

/// Endpoint associated with a public failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Endpoint {
    /// The application host process.
    Host,
    /// The `WebKit` renderer process.
    Renderer,
    /// The native peer process.
    Peer,
}

/// Bounded, non-secret context safe for diagnostics and wire conversion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ErrorContext {
    operation: &'static str,
    endpoint: Option<Endpoint>,
}

impl ErrorContext {
    /// Creates context without an endpoint association.
    pub const fn operation(operation: &'static str) -> Self {
        Self {
            operation,
            endpoint: None,
        }
    }

    /// Associates the context with an endpoint role.
    #[must_use]
    pub const fn at_endpoint(mut self, endpoint: Endpoint) -> Self {
        self.endpoint = Some(endpoint);
        self
    }

    /// Returns the stable operation label.
    pub const fn operation_name(self) -> &'static str {
        self.operation
    }

    /// Returns the endpoint association, when present.
    pub const fn endpoint(self) -> Option<Endpoint> {
        self.endpoint
    }
}

/// A redacted error value safe to expose at public API boundaries.
#[derive(Clone, Eq, PartialEq)]
pub struct ErrorReport {
    category: ErrorCategory,
    code: ErrorCode,
    recoverability: Recoverability,
    context: ErrorContext,
}

impl ErrorReport {
    /// Creates a report using a stable operation label.
    pub const fn new(
        category: ErrorCategory,
        code: ErrorCode,
        recoverability: Recoverability,
        operation: &'static str,
    ) -> Self {
        Self::with_context(
            category,
            code,
            recoverability,
            ErrorContext::operation(operation),
        )
    }

    /// Creates a report from explicit public context.
    pub const fn with_context(
        category: ErrorCategory,
        code: ErrorCode,
        recoverability: Recoverability,
        context: ErrorContext,
    ) -> Self {
        Self {
            category,
            code,
            recoverability,
            context,
        }
    }

    /// Creates an explicit unsupported-operation report.
    pub const fn unsupported(operation: &'static str) -> Self {
        Self::new(
            ErrorCategory::Unsupported,
            ErrorCode::Unsupported,
            Recoverability::Terminal,
            operation,
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

    /// Returns bounded public context.
    pub const fn context(&self) -> ErrorContext {
        self.context
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
        write!(formatter, "{} ({:?})", self.context.operation, self.code)
    }
}

impl std::error::Error for ErrorReport {}

/// A domain-specific source retained locally alongside its redacted public report.
///
/// `Debug` and `Display` intentionally omit the source. Callers may inspect it explicitly while
/// public API boundaries consume the value with [`Self::into_report`].
pub struct ErrorWithSource<Source> {
    report: ErrorReport,
    source: Source,
}

impl<Source> ErrorWithSource<Source> {
    /// Retains a domain-specific source without adding it to public diagnostics.
    pub const fn new(report: ErrorReport, source: Source) -> Self {
        Self { report, source }
    }

    /// Returns the redacted public report.
    pub const fn report(&self) -> &ErrorReport {
        &self.report
    }

    /// Returns the domain-specific source for local recovery or inspection.
    pub const fn source_error(&self) -> &Source {
        &self.source
    }

    /// Drops the local source at a public API or wire boundary.
    pub fn into_report(self) -> ErrorReport {
        self.report
    }
}

impl<Source> fmt::Debug for ErrorWithSource<Source> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ErrorWithSource")
            .field("report", &self.report)
            .field("source", &"<redacted>")
            .finish()
    }
}

impl<Source> fmt::Display for ErrorWithSource<Source> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.report.fmt(formatter)
    }
}

const _: () = {
    assert!(ErrorCode::Unsupported as u16 != ErrorCode::RequiredCapabilityMissing as u16);
    assert!(ErrorCode::InvalidStateTransition as u16 != ErrorCode::ProtocolViolation as u16);
    assert!(ErrorCode::Truncated as u16 != ErrorCode::InvalidRange as u16);
    assert!(ErrorCode::InvalidMagic as u16 != ErrorCode::LayoutVersionMismatch as u16);
    assert!(ErrorCode::ByteOrderMismatch as u16 != ErrorCode::StaleGeneration as u16);
    assert!(ErrorCode::UnknownRequiredFlag as u16 != ErrorCode::MessageTooLarge as u16);
    assert!(ErrorCode::MessageTooLarge as u16 != ErrorCode::Backpressured as u16);
    assert!(ErrorCode::Backpressured as u16 != ErrorCode::InvalidCursor as u16);
};

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

    #[test]
    fn context_contains_only_bounded_public_fields() {
        let context = ErrorContext::operation("attach").at_endpoint(Endpoint::Renderer);
        let report = ErrorReport::with_context(
            ErrorCategory::Bootstrap,
            ErrorCode::ProtocolViolation,
            Recoverability::ReplaceEndpoint,
            context,
        );
        assert_eq!(report.context().operation_name(), "attach");
        assert_eq!(report.context().endpoint(), Some(Endpoint::Renderer));
        assert!(!format!("{report:?}").contains("descriptor"));
    }

    #[test]
    fn local_source_is_preserved_but_never_formatted() {
        let error = ErrorWithSource::new(ErrorReport::unsupported("mapping"), "descriptor=42");
        assert_eq!(error.source_error(), &"descriptor=42");
        assert!(!format!("{error:?}").contains("descriptor=42"));
        assert_eq!(error.into_report().code(), ErrorCode::Unsupported);
    }
}
