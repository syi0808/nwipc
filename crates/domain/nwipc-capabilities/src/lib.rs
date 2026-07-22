//! Capability and topology negotiation without platform dependencies.

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};

/// A forward-compatible capability bit set.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct TransportCapabilities(u64);

impl TransportCapabilities {
    /// No optional capabilities.
    pub const NONE: Self = Self(0);
    /// Safety polling is available.
    pub const POLL_SAFETY: Self = Self(1 << 0);
    /// Darwin notification hints are available.
    pub const DARWIN_NOTIFY: Self = Self(1 << 1);
    /// `IOSurface` memory is available.
    pub const MACOS_IOSURFACE: Self = Self(1 << 2);

    /// Preserves known and unknown bits from the wire.
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// Returns all preserved bits.
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Returns whether every requested bit is present.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Returns the supported intersection, preserving mutually understood unknown bits.
    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }
}

/// Describes whether the host can observe payload or signal traffic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransportTopology {
    /// Must remain false for the NWIPC data path.
    pub host_in_payload_path: bool,
    /// Whether signal hints are relayed through the host.
    pub host_in_signal_path: bool,
}

impl TransportTopology {
    /// Creates the required direct renderer-to-peer topology.
    pub const fn direct() -> Self {
        Self {
            host_in_payload_path: false,
            host_in_signal_path: false,
        }
    }
}

/// Negotiates optional bits and rejects missing required bits.
///
/// # Errors
///
/// Returns a stable protocol error when any required bit is unsupported.
pub fn negotiate(
    supported: TransportCapabilities,
    requested: TransportCapabilities,
    required: TransportCapabilities,
) -> Result<TransportCapabilities, ErrorReport> {
    if !supported.contains(required) {
        return Err(ErrorReport::new(
            ErrorCategory::Protocol,
            ErrorCode::RequiredCapabilityMissing,
            Recoverability::Terminal,
            "required capability missing",
        ));
    }
    Ok(supported.intersection(requested))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_missing_required_capability() {
        let result = negotiate(
            TransportCapabilities::POLL_SAFETY,
            TransportCapabilities::DARWIN_NOTIFY,
            TransportCapabilities::DARWIN_NOTIFY,
        );
        assert_eq!(
            result.unwrap_err().code(),
            ErrorCode::RequiredCapabilityMissing
        );
    }

    #[test]
    fn direct_topology_excludes_host() {
        assert!(!TransportTopology::direct().host_in_payload_path);
    }
}
