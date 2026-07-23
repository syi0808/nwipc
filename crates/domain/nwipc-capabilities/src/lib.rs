//! Capability and topology negotiation without platform dependencies.

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};

/// A forward-compatible capability bit set.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct TransportCapabilities(u64);

impl TransportCapabilities {
    /// No capabilities.
    pub const NONE: Self = Self(0);
    /// A shared-memory payload plane is available.
    pub const SHARED_MEMORY_DATA_PLANE: Self = Self(1 << 0);
    /// Binary messages are available.
    pub const BINARY_MESSAGES: Self = Self(1 << 1);
    /// Bounded backpressure is enforced.
    pub const BOUNDED_BACKPRESSURE: Self = Self(1 << 2);
    /// Renderer-to-peer signal hints are direct.
    pub const DIRECT_SIGNAL: Self = Self(1 << 3);
    /// Signal hints may be relayed by the host.
    pub const HOST_RELAYED_SIGNAL: Self = Self(1 << 4);
    /// Generation-bound authenticated payload encryption is available.
    pub const AUTHENTICATED_ENCRYPTION: Self = Self(1 << 5);
    /// Compatibility name for authenticated payload encryption.
    pub const ENCRYPTED_REGION: Self = Self::AUTHENTICATED_ENCRYPTION;
    /// Borrowed send buffers are available.
    pub const BORROWED_SEND: Self = Self(1 << 6);
    /// Borrowed receive buffers are available.
    pub const BORROWED_RECEIVE: Self = Self(1 << 7);
    /// Logical messages may span multiple data records.
    pub const FRAGMENTATION: Self = Self(1 << 8);

    /// Mask of capabilities understood by this version.
    pub const KNOWN: Self = Self((1 << 9) - 1);

    /// Preserves known and unknown bits received from a peer.
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// Returns all preserved bits.
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Returns bits unknown to this version.
    pub const fn unknown_bits(self) -> u64 {
        self.0 & !Self::KNOWN.0
    }

    /// Returns whether every bit in `other` is present.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Returns the mutually supported intersection, including mutually understood future bits.
    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Returns the union of two sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

macro_rules! capability_role {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
        #[repr(transparent)]
        pub struct $name(TransportCapabilities);

        impl $name {
            /// Creates the role-specific capability set.
            pub const fn new(capabilities: TransportCapabilities) -> Self {
                Self(capabilities)
            }

            /// Returns the preserved capability bits.
            pub const fn capabilities(self) -> TransportCapabilities {
                self.0
            }
        }
    };
}

capability_role!(
    SupportedCapabilities,
    "Capabilities that the accepting endpoint can provide."
);
capability_role!(
    RequestedCapabilities,
    "Capabilities that the initiating endpoint would like to use."
);
capability_role!(
    RequiredCapabilities,
    "Requested capabilities without which the session must fail."
);
capability_role!(
    NegotiatedCapabilities,
    "Capabilities selected for one session generation."
);

/// Describes whether infrastructure processes can observe traffic.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransportTopology {
    /// Must remain false for the NWIPC payload path.
    pub host_in_payload_path: bool,
    /// Whether signal hints are relayed through the host.
    pub host_in_signal_path: bool,
    /// Whether `WebKit`'s browser process is in the payload path.
    pub browser_process_in_payload_path: bool,
    /// Whether the renderer contains the native NWIPC module.
    pub renderer_native_module: bool,
}

impl TransportTopology {
    /// Creates the required direct renderer-to-peer topology.
    pub const fn direct() -> Self {
        Self {
            host_in_payload_path: false,
            host_in_signal_path: false,
            browser_process_in_payload_path: false,
            renderer_native_module: true,
        }
    }

    /// Validates the non-relayed payload invariant.
    ///
    /// # Errors
    ///
    /// Returns a stable configuration error when an infrastructure process is in the payload path.
    pub fn validate(self) -> Result<Self, ErrorReport> {
        if self.host_in_payload_path || self.browser_process_in_payload_path {
            return Err(ErrorReport::new(
                ErrorCategory::Configuration,
                ErrorCode::ProtocolViolation,
                Recoverability::Terminal,
                "host-relayed payload topology",
            ));
        }
        Ok(self)
    }
}

/// Negotiates requested bits and rejects unsupported or unrequested required bits.
///
/// Unknown optional bits are preserved only when both endpoints advertise them. Unknown required
/// bits therefore work with a newer peer when supported and fail closed otherwise.
///
/// # Errors
///
/// Returns a stable protocol error when any required bit is not both requested and supported.
pub fn negotiate(
    supported: SupportedCapabilities,
    requested: RequestedCapabilities,
    required: RequiredCapabilities,
) -> Result<NegotiatedCapabilities, ErrorReport> {
    let supported = supported.capabilities();
    let requested = requested.capabilities();
    let required = required.capabilities();
    if !requested.contains(required) || !supported.contains(required) {
        return Err(ErrorReport::new(
            ErrorCategory::Protocol,
            ErrorCode::RequiredCapabilityMissing,
            Recoverability::Terminal,
            "required capability missing",
        ));
    }
    Ok(NegotiatedCapabilities::new(
        supported.intersection(requested),
    ))
}

const _: () = assert!(size_of::<TransportCapabilities>() == 8);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiates_supported_requested_bits() {
        let common = TransportCapabilities::SHARED_MEMORY_DATA_PLANE
            .union(TransportCapabilities::BINARY_MESSAGES);
        let negotiated = negotiate(
            SupportedCapabilities::new(common),
            RequestedCapabilities::new(common.union(TransportCapabilities::DIRECT_SIGNAL)),
            RequiredCapabilities::new(TransportCapabilities::SHARED_MEMORY_DATA_PLANE),
        )
        .unwrap();
        assert_eq!(negotiated.capabilities(), common);
    }

    #[test]
    fn rejects_a_missing_required_capability() {
        let result = negotiate(
            SupportedCapabilities::new(TransportCapabilities::BINARY_MESSAGES),
            RequestedCapabilities::new(TransportCapabilities::DIRECT_SIGNAL),
            RequiredCapabilities::new(TransportCapabilities::DIRECT_SIGNAL),
        );
        assert_eq!(
            result.unwrap_err().code(),
            ErrorCode::RequiredCapabilityMissing
        );
    }

    #[test]
    fn preserves_mutually_supported_unknown_optional_bits() {
        let future = TransportCapabilities::from_bits(1 << 48);
        let negotiated = negotiate(
            SupportedCapabilities::new(future),
            RequestedCapabilities::new(future),
            RequiredCapabilities::new(TransportCapabilities::NONE),
        )
        .unwrap();
        assert_eq!(negotiated.capabilities().unknown_bits(), 1 << 48);
    }

    #[test]
    fn negotiates_fragmentation_only_when_both_endpoints_enable_it() {
        let negotiated = negotiate(
            SupportedCapabilities::new(TransportCapabilities::FRAGMENTATION),
            RequestedCapabilities::new(TransportCapabilities::FRAGMENTATION),
            RequiredCapabilities::new(TransportCapabilities::FRAGMENTATION),
        )
        .unwrap();
        assert!(
            negotiated
                .capabilities()
                .contains(TransportCapabilities::FRAGMENTATION)
        );
    }

    #[test]
    fn topology_excludes_infrastructure_from_payload() {
        assert_eq!(
            TransportTopology::direct().validate(),
            Ok(TransportTopology::direct())
        );
        let invalid = TransportTopology {
            host_in_payload_path: true,
            ..TransportTopology::direct()
        };
        assert!(invalid.validate().is_err());
    }
}
