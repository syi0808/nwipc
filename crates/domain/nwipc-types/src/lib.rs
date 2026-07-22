//! Platform-independent identities and counters used across NWIPC.

use std::fmt;
use std::num::{NonZeroU32, NonZeroU64};

/// A process-independent, non-zero 128-bit session identity.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct SessionId([u8; 16]);

impl SessionId {
    /// Constructs an identity from its canonical bytes, rejecting the all-zero value.
    pub const fn from_bytes(bytes: [u8; 16]) -> Option<Self> {
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] != 0 {
                return Some(Self(bytes));
            }
            index += 1;
        }
        None
    }

    /// Constructs an identity from a numeric value at an explicit conversion boundary.
    pub const fn from_u128(value: u128) -> Option<Self> {
        Self::from_bytes(value.to_le_bytes())
    }

    /// Returns the canonical wire bytes.
    pub const fn to_bytes(self) -> [u8; 16] {
        self.0
    }

    /// Returns the numeric value represented by the canonical bytes.
    pub const fn to_u128(self) -> u128 {
        u128::from_le_bytes(self.0)
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

macro_rules! monotonic_u64 {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[repr(transparent)]
        pub struct $name(NonZeroU64);

        impl $name {
            /// Constructs a non-zero value.
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

            /// Advances monotonically, returning `None` rather than wrapping.
            pub fn checked_next(self) -> Option<Self> {
                self.get().checked_add(1).and_then(Self::new)
            }
        }
    };
}

macro_rules! nonzero_u32 {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[repr(transparent)]
        pub struct $name(NonZeroU32);

        impl $name {
            /// Constructs a non-zero value.
            pub const fn new(value: u32) -> Option<Self> {
                match NonZeroU32::new(value) {
                    Some(value) => Some(Self(value)),
                    None => None,
                }
            }

            /// Returns the fixed-width representation.
            pub const fn get(self) -> u32 {
                self.0.get()
            }

            /// Advances monotonically, returning `None` rather than wrapping.
            pub fn checked_next(self) -> Option<Self> {
                self.get().checked_add(1).and_then(Self::new)
            }
        }
    };
}

monotonic_u64!(
    Generation,
    "A monotonically increasing session resource generation."
);
monotonic_u64!(
    DocumentGeneration,
    "A monotonically increasing renderer document generation."
);
nonzero_u32!(MessageId, "A non-zero message identity.");
nonzero_u32!(PortId, "A non-zero logical port identity.");

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

    /// Returns the forward wrapping distance when it is unambiguous.
    pub const fn forward_distance(self, newer: Self) -> Option<u32> {
        let distance = newer.0.wrapping_sub(self.0);
        if distance <= i32::MAX as u32 {
            Some(distance)
        } else {
            None
        }
    }
}

const _: () = assert!(size_of::<SessionId>() == 16);
const _: () = assert!(align_of::<SessionId>() == 1);
const _: () = assert!(size_of::<Generation>() == 8);
const _: () = assert!(size_of::<DocumentGeneration>() == 8);
const _: () = assert!(size_of::<MessageId>() == 4);
const _: () = assert!(size_of::<Sequence>() == 4);
const _: () = assert!(size_of::<PortId>() == 4);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_and_wrapping_boundaries_differ() {
        assert!(Generation::new(0).is_none());
        assert!(Generation::new(u64::MAX).unwrap().checked_next().is_none());
        assert!(MessageId::new(u32::MAX).unwrap().checked_next().is_none());
        assert_eq!(Sequence::new(u32::MAX).wrapping_next().get(), 0);
    }

    #[test]
    fn session_identity_has_canonical_bytes_and_redacted_output() {
        let bytes = [1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let session = SessionId::from_bytes(bytes).unwrap();
        assert_eq!(session.to_bytes(), bytes);
        assert_eq!(SessionId::from_u128(session.to_u128()), Some(session));
        assert_eq!(format!("{session:?}"), "SessionId(<redacted>)");
        assert_eq!(session.to_string(), "<redacted-session>");
        assert!(SessionId::from_bytes([0; 16]).is_none());
    }

    #[test]
    fn wrapping_distance_rejects_ambiguous_ordering() {
        let before_wrap = Sequence::new(u32::MAX - 2);
        assert_eq!(before_wrap.forward_distance(Sequence::new(1)), Some(4));
        assert_eq!(Sequence::new(1).forward_distance(before_wrap), None);
    }
}
