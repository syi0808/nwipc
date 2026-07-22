//! Platform-independent identity and sequence value types.

use std::fmt;
use std::num::{NonZeroU64, NonZeroU128};

/// A process-independent session identity.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct SessionId(NonZeroU128);

impl SessionId {
    /// Constructs a non-zero session identity.
    pub const fn new(value: u128) -> Option<Self> {
        match NonZeroU128::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Returns the wire representation at an explicit serialization boundary.
    pub const fn get(self) -> u128 {
        self.0.get()
    }
}

impl fmt::Debug for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SessionId(<redacted>)")
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted-session>")
    }
}

macro_rules! monotonic_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[repr(transparent)]
        pub struct $name(NonZeroU64);

        impl $name {
            /// Constructs a non-zero identifier.
            pub const fn new(value: u64) -> Option<Self> {
                match NonZeroU64::new(value) {
                    Some(value) => Some(Self(value)),
                    None => None,
                }
            }

            /// Returns the fixed-width representation.
            pub const fn get(self) -> u64 {
                self.0.get()
            }

            /// Advances the identifier, returning `None` at the monotonic boundary.
            pub fn checked_next(self) -> Option<Self> {
                self.get().checked_add(1).and_then(Self::new)
            }
        }
    };
}

monotonic_id!(
    Generation,
    "A monotonically increasing session resource generation."
);
monotonic_id!(
    DocumentGeneration,
    "A monotonically increasing renderer document generation."
);
monotonic_id!(MessageId, "A non-zero message identity.");
monotonic_id!(PortId, "A non-zero logical port identity.");

/// A wrapping ring sequence number.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct Sequence(u32);

impl Sequence {
    /// Creates a sequence at any wrapping value, including zero.
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the fixed-width representation.
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Advances with the wire-defined wrapping behavior.
    #[must_use]
    pub const fn wrapping_next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_and_wrapping_boundaries_differ() {
        assert!(Generation::new(0).is_none());
        assert!(Generation::new(u64::MAX).unwrap().checked_next().is_none());
        assert_eq!(Sequence::new(u32::MAX).wrapping_next().get(), 0);
    }

    #[test]
    fn identity_layouts_are_fixed() {
        assert_eq!(size_of::<SessionId>(), 16);
        assert_eq!(size_of::<Generation>(), 8);
        assert_eq!(size_of::<Sequence>(), 4);
    }
}
