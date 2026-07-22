//! Safe logical model for the two unidirectional regions in an NWIPC session.

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_memory_api::RegionDescriptor;
use nwipc_types::Generation;

/// Process role that owns writes to a region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegionOwner {
    /// `WebKit` renderer endpoint.
    Renderer,
    /// Native peer endpoint.
    Peer,
}

/// Logical traffic direction and its single writer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegionDirection {
    /// Renderer writes and peer reads.
    RendererToPeer,
    /// Peer writes and renderer reads.
    PeerToRenderer,
}

impl RegionDirection {
    /// Single writer for this direction.
    pub const fn owner(self) -> RegionOwner {
        match self {
            Self::RendererToPeer => RegionOwner::Renderer,
            Self::PeerToRenderer => RegionOwner::Peer,
        }
    }

    /// Stable descriptor value.
    pub const fn wire_value(self) -> u8 {
        match self {
            Self::RendererToPeer => 1,
            Self::PeerToRenderer => 2,
        }
    }
}

/// Descriptors for the two regions of one generation.
#[derive(Clone, Debug)]
pub struct RegionPair<Descriptor> {
    generation: Generation,
    renderer_to_peer: Descriptor,
    peer_to_renderer: Descriptor,
}

impl<Descriptor: RegionDescriptor> RegionPair<Descriptor> {
    /// Validates and groups descriptors without coupling their mapping lifetimes.
    ///
    /// # Errors
    ///
    /// Rejects empty, unequal, or stale-generation descriptors.
    pub fn new(
        generation: Generation,
        renderer_to_peer: Descriptor,
        peer_to_renderer: Descriptor,
    ) -> Result<Self, ErrorReport> {
        let length = renderer_to_peer.byte_len();
        if length == 0 || peer_to_renderer.byte_len() != length {
            return Err(region_error(ErrorCode::InvalidRange, "create region pair"));
        }
        if renderer_to_peer.generation() != generation
            || peer_to_renderer.generation() != generation
        {
            return Err(region_error(
                ErrorCode::StaleGeneration,
                "create region pair",
            ));
        }
        Ok(Self {
            generation,
            renderer_to_peer,
            peer_to_renderer,
        })
    }

    /// Pair generation.
    pub const fn generation(&self) -> Generation {
        self.generation
    }

    /// Descriptor for a logical direction.
    pub const fn descriptor(&self, direction: RegionDirection) -> &Descriptor {
        match direction {
            RegionDirection::RendererToPeer => &self.renderer_to_peer,
            RegionDirection::PeerToRenderer => &self.peer_to_renderer,
        }
    }

    /// Logical length shared by both directions.
    pub fn byte_len(&self) -> usize {
        self.renderer_to_peer.byte_len()
    }
}

fn region_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Memory,
        code,
        Recoverability::ReplaceEndpoint,
        operation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug)]
    struct Descriptor {
        length: usize,
        generation: Generation,
    }

    impl RegionDescriptor for Descriptor {
        fn byte_len(&self) -> usize {
            self.length
        }

        fn generation(&self) -> Generation {
            self.generation
        }
    }

    #[test]
    fn pair_requires_matching_lengths_and_generation() {
        let generation = Generation::new(3).unwrap();
        let first = Descriptor {
            length: 4096,
            generation,
        };
        assert!(RegionPair::new(generation, first.clone(), first).is_ok());
        let stale = Descriptor {
            length: 4096,
            generation: Generation::new(2).unwrap(),
        };
        let current = Descriptor {
            length: 4096,
            generation,
        };
        assert_eq!(
            RegionPair::new(generation, stale, current)
                .unwrap_err()
                .code(),
            ErrorCode::StaleGeneration
        );
    }
}
