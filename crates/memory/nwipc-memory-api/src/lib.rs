//! Provider-neutral shared-memory ownership and mapping contracts.

use std::fmt::Debug;

use nwipc_error::ErrorReport;
use nwipc_types::Generation;

/// Access granted to a mapped region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MappingAccess {
    /// Bytes may only be copied out of the mapping.
    ReadOnly,
    /// Bytes may be copied in and out of the mapping.
    ReadWrite,
}

/// A transferable, provider-specific region descriptor.
pub trait RegionDescriptor: Clone + Debug + Send + Sync + 'static {
    /// Logical byte length promised by the descriptor.
    fn byte_len(&self) -> usize;

    /// Resource generation to which this descriptor is bound.
    fn generation(&self) -> Generation;
}

/// A live mapping which owns the native resource for its entire lifetime.
///
/// Copy operations are used instead of exposing references into memory that another process can
/// mutate concurrently.
pub trait MappedRegion: Send + 'static {
    /// Logical mapped length.
    fn len(&self) -> usize;

    /// Whether the logical mapping is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Granted mapping access.
    fn access(&self) -> MappingAccess;

    /// Copies bytes out of the mapping.
    ///
    /// # Errors
    ///
    /// Returns a typed range or platform error.
    fn read(&self, offset: usize, output: &mut [u8]) -> Result<(), ErrorReport>;

    /// Copies bytes into a read-write mapping.
    ///
    /// # Errors
    ///
    /// Returns a typed access, range, or platform error.
    fn write(&mut self, offset: usize, input: &[u8]) -> Result<(), ErrorReport>;

    /// Acquire-loads an aligned shared little-endian `u32`.
    ///
    /// Implementations must make this operation atomic with every load/store of the same mapping
    /// location in every attached process.
    ///
    /// # Errors
    ///
    /// Returns a typed alignment, range, or platform error.
    fn load_u32_acquire(&self, offset: usize) -> Result<u32, ErrorReport>;

    /// Release-stores an aligned shared little-endian `u32`.
    ///
    /// Implementations must make this operation atomic with every load/store of the same mapping
    /// location in every attached process.
    ///
    /// # Errors
    ///
    /// Returns a typed access, alignment, range, or platform error.
    fn store_u32_release(&mut self, offset: usize, value: u32) -> Result<(), ErrorReport>;
}

/// Provider lifecycle contract shared by fake and platform implementations.
pub trait SharedMemoryProvider {
    /// Transferable descriptor type.
    type Descriptor: RegionDescriptor;
    /// Owned mapping type.
    type Mapping: MappedRegion;

    /// Creates a zero-filled, read-write region and its transferable descriptor.
    ///
    /// # Errors
    ///
    /// Returns a typed configuration, resource, or platform error.
    fn create(
        &self,
        byte_len: usize,
        generation: Generation,
    ) -> Result<(Self::Mapping, Self::Descriptor), ErrorReport>;

    /// Attaches a descriptor after validating its active generation.
    ///
    /// # Errors
    ///
    /// Returns a typed descriptor, generation, access, or platform error.
    fn attach(
        &self,
        descriptor: &Self::Descriptor,
        expected_generation: Generation,
        access: MappingAccess,
    ) -> Result<Self::Mapping, ErrorReport>;
}
