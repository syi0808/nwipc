//! Owns the session boundary for NWIPC.
//!
//! The contract is present, but operational behavior is implemented in a later vertical-slice
//! phase. Calling it now fails explicitly.

use nwipc_error::ErrorReport;

/// Compile-time marker for this component boundary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Session;

impl Session {
    /// Attempts to initialize the component.
    ///
    /// # Errors
    ///
    /// Returns an Unsupported error while the provider implementation is unavailable.
    pub fn initialize() -> Result<Self, ErrorReport> {
        Err(ErrorReport::unsupported("nwipc-session"))
    }
}
