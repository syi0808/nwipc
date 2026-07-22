//! macOS `IOSurface` shared-memory provider.
//!
//! The initial provider uses a redacted `IOSurface` global ID descriptor. Mapping operations copy
//! while the surface is locked, so safe APIs never expose a pointer whose bytes another process
//! may mutate.

use std::fmt;

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_memory_api::{MappedRegion, MappingAccess, RegionDescriptor, SharedMemoryProvider};
use nwipc_types::Generation;

const DESCRIPTOR_LENGTH: usize = 20;

/// Transferable `IOSurface` ID, logical length, and generation.
#[derive(Clone, Eq, PartialEq)]
pub struct IoSurfaceDescriptor {
    surface_id: u32,
    byte_len: usize,
    generation: Generation,
}

impl IoSurfaceDescriptor {
    /// Encodes the fixed-width provider descriptor for bootstrap transport.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRange` if the platform length cannot fit the wire format.
    pub fn encode(&self) -> Result<[u8; DESCRIPTOR_LENGTH], ErrorReport> {
        let length = u64::try_from(self.byte_len)
            .map_err(|_| memory_error(ErrorCode::InvalidRange, "encode IOSurface descriptor"))?;
        let mut output = [0; DESCRIPTOR_LENGTH];
        output[..4].copy_from_slice(&self.surface_id.to_le_bytes());
        output[4..12].copy_from_slice(&length.to_le_bytes());
        output[12..20].copy_from_slice(&self.generation.get().to_le_bytes());
        Ok(output)
    }

    /// Decodes and validates a fixed-width provider descriptor.
    ///
    /// # Errors
    ///
    /// Rejects zero IDs, lengths, generations, and non-canonical lengths.
    pub fn decode(input: &[u8]) -> Result<Self, ErrorReport> {
        if input.len() != DESCRIPTOR_LENGTH {
            return Err(memory_error(
                ErrorCode::Truncated,
                "decode IOSurface descriptor",
            ));
        }
        let surface_id = u32::from_le_bytes(
            input[..4]
                .try_into()
                .map_err(|_| memory_error(ErrorCode::Truncated, "decode IOSurface descriptor"))?,
        );
        let wire_length = u64::from_le_bytes(
            input[4..12]
                .try_into()
                .map_err(|_| memory_error(ErrorCode::Truncated, "decode IOSurface descriptor"))?,
        );
        let byte_len = usize::try_from(wire_length)
            .map_err(|_| memory_error(ErrorCode::InvalidRange, "decode IOSurface descriptor"))?;
        let generation_value = u64::from_le_bytes(
            input[12..20]
                .try_into()
                .map_err(|_| memory_error(ErrorCode::Truncated, "decode IOSurface descriptor"))?,
        );
        let generation = Generation::new(generation_value).ok_or_else(|| {
            memory_error(ErrorCode::StaleGeneration, "decode IOSurface descriptor")
        })?;
        if surface_id == 0 || byte_len == 0 {
            return Err(memory_error(
                ErrorCode::InvalidRange,
                "decode IOSurface descriptor",
            ));
        }
        Ok(Self {
            surface_id,
            byte_len,
            generation,
        })
    }
}

impl fmt::Debug for IoSurfaceDescriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IoSurfaceDescriptor")
            .field("surface_id", &"<redacted>")
            .field("byte_len", &self.byte_len)
            .field("generation", &self.generation)
            .finish()
    }
}

impl RegionDescriptor for IoSurfaceDescriptor {
    fn byte_len(&self) -> usize {
        self.byte_len
    }

    fn generation(&self) -> Generation {
        self.generation
    }
}

/// `IOSurface` provider capability.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IoSurfaceProvider;

/// Redacted provider capabilities for support and sandbox diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IoSurfaceProviderDiagnostics {
    /// Descriptor transport selected by this implementation.
    pub descriptor_transport: IoSurfaceDescriptorTransport,
    /// Whether process-boundary visibility is supported.
    pub cross_process: bool,
    /// Whether read-only access is enforced by the native mapping rather than the safe API.
    pub native_read_only_enforced: bool,
}

/// Native representation transferred during bootstrap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IoSurfaceDescriptorTransport {
    /// `IOSurface` global ID; the native ID itself remains redacted.
    GlobalId,
}

impl IoSurfaceProvider {
    /// Initializes the provider on macOS.
    ///
    /// # Errors
    ///
    /// Returns explicit `Unsupported` on other platforms.
    pub fn initialize() -> Result<Self, ErrorReport> {
        #[cfg(target_os = "macos")]
        {
            Ok(Self)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(ErrorReport::unsupported("initialize IOSurface provider"))
        }
    }

    /// Returns non-secret provider capabilities.
    pub const fn diagnostics(self) -> IoSurfaceProviderDiagnostics {
        IoSurfaceProviderDiagnostics {
            descriptor_transport: IoSurfaceDescriptorTransport::GlobalId,
            cross_process: cfg!(target_os = "macos"),
            native_read_only_enforced: false,
        }
    }
}

impl SharedMemoryProvider for IoSurfaceProvider {
    type Descriptor = IoSurfaceDescriptor;
    type Mapping = IoSurfaceMapping;

    fn create(
        &self,
        byte_len: usize,
        generation: Generation,
    ) -> Result<(Self::Mapping, Self::Descriptor), ErrorReport> {
        platform::create(byte_len, generation)
    }

    fn attach(
        &self,
        descriptor: &Self::Descriptor,
        expected_generation: Generation,
        access: MappingAccess,
    ) -> Result<Self::Mapping, ErrorReport> {
        if descriptor.generation != expected_generation {
            return Err(memory_error(ErrorCode::StaleGeneration, "attach IOSurface"));
        }
        platform::attach(descriptor, access)
    }
}

/// Owned `IOSurface` mapping.
pub struct IoSurfaceMapping {
    inner: platform::Mapping,
    byte_len: usize,
    access: MappingAccess,
}

impl fmt::Debug for IoSurfaceMapping {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IoSurfaceMapping")
            .field("native", &"<redacted>")
            .field("byte_len", &self.byte_len)
            .field("access", &self.access)
            .finish_non_exhaustive()
    }
}

/// Non-secret state for one owned mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IoSurfaceMappingDiagnostics {
    /// Logical bytes promised by the bootstrap descriptor.
    pub logical_byte_len: usize,
    /// Native allocation bytes, which may include platform rounding.
    pub allocated_byte_len: usize,
    /// Granted logical access.
    pub access: MappingAccess,
}

impl IoSurfaceMapping {
    /// Returns redacted mapping diagnostics without exposing a native ID or address.
    pub fn diagnostics(&self) -> IoSurfaceMappingDiagnostics {
        IoSurfaceMappingDiagnostics {
            logical_byte_len: self.byte_len,
            allocated_byte_len: platform::allocated_len(&self.inner),
            access: self.access,
        }
    }
}

impl MappedRegion for IoSurfaceMapping {
    fn len(&self) -> usize {
        self.byte_len
    }

    fn access(&self) -> MappingAccess {
        self.access
    }

    fn read(&self, offset: usize, output: &mut [u8]) -> Result<(), ErrorReport> {
        checked_end(offset, output.len(), self.byte_len, "read IOSurface")?;
        platform::read(&self.inner, offset, output)
    }

    fn write(&mut self, offset: usize, input: &[u8]) -> Result<(), ErrorReport> {
        if self.access != MappingAccess::ReadWrite {
            return Err(memory_error(
                ErrorCode::RequiredCapabilityMissing,
                "write read-only IOSurface",
            ));
        }
        checked_end(offset, input.len(), self.byte_len, "write IOSurface")?;
        platform::write(&self.inner, offset, input)
    }

    fn load_u32_acquire(&self, offset: usize) -> Result<u32, ErrorReport> {
        checked_atomic_end(offset, self.byte_len, "load IOSurface atomic")?;
        platform::load_u32_acquire(&self.inner, offset)
    }

    fn store_u32_release(&mut self, offset: usize, value: u32) -> Result<(), ErrorReport> {
        if self.access != MappingAccess::ReadWrite {
            return Err(memory_error(
                ErrorCode::RequiredCapabilityMissing,
                "store read-only IOSurface atomic",
            ));
        }
        checked_atomic_end(offset, self.byte_len, "store IOSurface atomic")?;
        platform::store_u32_release(&self.inner, offset, value)
    }
}

fn checked_end(
    offset: usize,
    length: usize,
    byte_len: usize,
    operation: &'static str,
) -> Result<usize, ErrorReport> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| memory_error(ErrorCode::InvalidRange, operation))?;
    if end > byte_len {
        return Err(memory_error(ErrorCode::InvalidRange, operation));
    }
    Ok(end)
}

fn checked_atomic_end(
    offset: usize,
    byte_len: usize,
    operation: &'static str,
) -> Result<usize, ErrorReport> {
    if offset % align_of::<u32>() != 0 {
        return Err(memory_error(ErrorCode::InvalidAlignment, operation));
    }
    checked_end(offset, size_of::<u32>(), byte_len, operation)
}

fn memory_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Memory,
        code,
        match code {
            ErrorCode::StaleGeneration => Recoverability::ReplaceEndpoint,
            _ => Recoverability::Terminal,
        },
        operation,
    )
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{c_int, c_void};
    use std::ptr;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::{
        ErrorCode, ErrorReport, Generation, IoSurfaceDescriptor, IoSurfaceMapping, MappingAccess,
        memory_error,
    };

    type CFTypeRef = *const c_void;
    type IOSurfaceRef = *mut c_void;

    const CF_NUMBER_SINT64_TYPE: isize = 4;
    const IO_SURFACE_LOCK_READ_ONLY: u32 = 1;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFBooleanTrue: CFTypeRef;
        fn CFNumberCreate(
            allocator: CFTypeRef,
            number_type: isize,
            value: *const c_void,
        ) -> CFTypeRef;
        fn CFDictionaryCreate(
            allocator: CFTypeRef,
            keys: *const CFTypeRef,
            values: *const CFTypeRef,
            count: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> CFTypeRef;
        fn CFRelease(value: CFTypeRef);
    }

    #[link(name = "IOSurface", kind = "framework")]
    unsafe extern "C" {
        static kIOSurfaceAllocSize: CFTypeRef;
        static kIOSurfaceIsGlobal: CFTypeRef;
        fn IOSurfaceCreate(properties: CFTypeRef) -> IOSurfaceRef;
        fn IOSurfaceLookup(surface_id: u32) -> IOSurfaceRef;
        fn IOSurfaceGetID(surface: IOSurfaceRef) -> u32;
        fn IOSurfaceGetAllocSize(surface: IOSurfaceRef) -> usize;
        fn IOSurfaceGetBaseAddress(surface: IOSurfaceRef) -> *mut c_void;
        fn IOSurfaceLock(surface: IOSurfaceRef, options: u32, seed: *mut u32) -> c_int;
        fn IOSurfaceUnlock(surface: IOSurfaceRef, options: u32, seed: *mut u32) -> c_int;
    }

    pub(super) struct Mapping(IOSurfaceRef);

    unsafe impl Send for Mapping {}

    impl Drop for Mapping {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0.cast_const()) };
        }
    }

    pub(super) fn create(
        byte_len: usize,
        generation: Generation,
    ) -> Result<(IoSurfaceMapping, IoSurfaceDescriptor), ErrorReport> {
        if byte_len == 0 {
            return Err(memory_error(ErrorCode::InvalidRange, "create IOSurface"));
        }
        let signed_length = i64::try_from(byte_len)
            .map_err(|_| memory_error(ErrorCode::InvalidRange, "create IOSurface"))?;
        let number = unsafe {
            CFNumberCreate(
                ptr::null(),
                CF_NUMBER_SINT64_TYPE,
                ptr::from_ref(&signed_length).cast(),
            )
        };
        if number.is_null() {
            return Err(memory_error(ErrorCode::Internal, "create IOSurface size"));
        }
        let keys = unsafe { [kIOSurfaceAllocSize, kIOSurfaceIsGlobal] };
        let values = unsafe { [number, kCFBooleanTrue] };
        let dictionary = unsafe {
            CFDictionaryCreate(
                ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                2,
                ptr::null(),
                ptr::null(),
            )
        };
        unsafe { CFRelease(number) };
        if dictionary.is_null() {
            return Err(memory_error(
                ErrorCode::Internal,
                "create IOSurface properties",
            ));
        }
        let surface = unsafe { IOSurfaceCreate(dictionary) };
        unsafe { CFRelease(dictionary) };
        if surface.is_null() {
            return Err(memory_error(ErrorCode::Internal, "create IOSurface"));
        }
        let surface_id = unsafe { IOSurfaceGetID(surface) };
        if surface_id == 0 || unsafe { IOSurfaceGetAllocSize(surface) } < byte_len {
            unsafe { CFRelease(surface.cast_const()) };
            return Err(memory_error(
                ErrorCode::InvalidRange,
                "validate IOSurface allocation",
            ));
        }
        Ok((
            IoSurfaceMapping {
                inner: Mapping(surface),
                byte_len,
                access: MappingAccess::ReadWrite,
            },
            IoSurfaceDescriptor {
                surface_id,
                byte_len,
                generation,
            },
        ))
    }

    pub(super) fn attach(
        descriptor: &IoSurfaceDescriptor,
        access: MappingAccess,
    ) -> Result<IoSurfaceMapping, ErrorReport> {
        let surface = unsafe { IOSurfaceLookup(descriptor.surface_id) };
        if surface.is_null() {
            return Err(memory_error(ErrorCode::Closed, "lookup IOSurface"));
        }
        if unsafe { IOSurfaceGetAllocSize(surface) } < descriptor.byte_len {
            unsafe { CFRelease(surface.cast_const()) };
            return Err(memory_error(
                ErrorCode::InvalidRange,
                "validate attached IOSurface",
            ));
        }
        Ok(IoSurfaceMapping {
            inner: Mapping(surface),
            byte_len: descriptor.byte_len,
            access,
        })
    }

    pub(super) fn read(
        mapping: &Mapping,
        offset: usize,
        output: &mut [u8],
    ) -> Result<(), ErrorReport> {
        with_locked(
            mapping,
            IO_SURFACE_LOCK_READ_ONLY,
            "read IOSurface",
            |base| unsafe {
                ptr::copy_nonoverlapping(
                    base.add(offset).cast_const(),
                    output.as_mut_ptr(),
                    output.len(),
                );
            },
        )
    }

    pub(super) fn write(mapping: &Mapping, offset: usize, input: &[u8]) -> Result<(), ErrorReport> {
        with_locked(mapping, 0, "write IOSurface", |base| unsafe {
            ptr::copy_nonoverlapping(input.as_ptr(), base.add(offset), input.len());
        })
    }

    pub(super) fn load_u32_acquire(mapping: &Mapping, offset: usize) -> Result<u32, ErrorReport> {
        with_locked(
            mapping,
            IO_SURFACE_LOCK_READ_ONLY,
            "load IOSurface atomic",
            |base| {
                if base
                    .wrapping_add(offset)
                    .align_offset(align_of::<AtomicU32>())
                    != 0
                {
                    return Err(memory_error(
                        ErrorCode::InvalidAlignment,
                        "load IOSurface atomic",
                    ));
                }
                #[allow(clippy::cast_ptr_alignment)]
                let pointer = unsafe { base.add(offset).cast::<AtomicU32>() };
                Ok(unsafe { &*pointer }.load(Ordering::Acquire))
            },
        )?
    }

    pub(super) fn store_u32_release(
        mapping: &Mapping,
        offset: usize,
        value: u32,
    ) -> Result<(), ErrorReport> {
        with_locked(mapping, 0, "store IOSurface atomic", |base| {
            if base
                .wrapping_add(offset)
                .align_offset(align_of::<AtomicU32>())
                != 0
            {
                return Err(memory_error(
                    ErrorCode::InvalidAlignment,
                    "store IOSurface atomic",
                ));
            }
            #[allow(clippy::cast_ptr_alignment)]
            let pointer = unsafe { base.add(offset).cast::<AtomicU32>() };
            unsafe { &*pointer }.store(value, Ordering::Release);
            Ok(())
        })?
    }

    pub(super) fn allocated_len(mapping: &Mapping) -> usize {
        unsafe { IOSurfaceGetAllocSize(mapping.0) }
    }

    fn with_locked<Output>(
        mapping: &Mapping,
        options: u32,
        operation: &'static str,
        body: impl FnOnce(*mut u8) -> Output,
    ) -> Result<Output, ErrorReport> {
        if unsafe { IOSurfaceLock(mapping.0, options, ptr::null_mut()) } != 0 {
            return Err(memory_error(ErrorCode::Internal, operation));
        }
        let base = unsafe { IOSurfaceGetBaseAddress(mapping.0) }.cast::<u8>();
        if base.is_null() {
            let _ = unsafe { IOSurfaceUnlock(mapping.0, options, ptr::null_mut()) };
            return Err(memory_error(ErrorCode::Internal, operation));
        }
        let output = body(base);
        if unsafe { IOSurfaceUnlock(mapping.0, options, ptr::null_mut()) } != 0 {
            return Err(memory_error(ErrorCode::Internal, operation));
        }
        Ok(output)
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::{ErrorReport, Generation, IoSurfaceDescriptor, IoSurfaceMapping, MappingAccess};

    pub(super) struct Mapping;

    pub(super) fn create(
        _: usize,
        _: Generation,
    ) -> Result<(IoSurfaceMapping, IoSurfaceDescriptor), ErrorReport> {
        Err(ErrorReport::unsupported("create IOSurface"))
    }

    pub(super) fn attach(
        _: &IoSurfaceDescriptor,
        _: MappingAccess,
    ) -> Result<IoSurfaceMapping, ErrorReport> {
        Err(ErrorReport::unsupported("attach IOSurface"))
    }

    pub(super) fn read(_: &Mapping, _: usize, _: &mut [u8]) -> Result<(), ErrorReport> {
        Err(ErrorReport::unsupported("read IOSurface"))
    }

    pub(super) fn write(_: &Mapping, _: usize, _: &[u8]) -> Result<(), ErrorReport> {
        Err(ErrorReport::unsupported("write IOSurface"))
    }

    pub(super) fn load_u32_acquire(_: &Mapping, _: usize) -> Result<u32, ErrorReport> {
        Err(ErrorReport::unsupported("load IOSurface atomic"))
    }

    pub(super) fn store_u32_release(_: &Mapping, _: usize, _: u32) -> Result<(), ErrorReport> {
        Err(ErrorReport::unsupported("store IOSurface atomic"))
    }

    pub(super) const fn allocated_len(_: &Mapping) -> usize {
        0
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use std::process::Command;

    use super::*;

    #[test]
    fn descriptor_round_trip_is_fixed_width_and_redacted() {
        let descriptor = IoSurfaceDescriptor {
            surface_id: 7,
            byte_len: 4096,
            generation: Generation::new(2).unwrap(),
        };
        assert_eq!(
            IoSurfaceDescriptor::decode(&descriptor.encode().unwrap()).unwrap(),
            descriptor
        );
        assert!(!format!("{descriptor:?}").contains('7'));
        assert_eq!(
            IoSurfaceDescriptor::decode(&[0; DESCRIPTOR_LENGTH])
                .unwrap_err()
                .code(),
            ErrorCode::StaleGeneration
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn create_attach_and_access_contract() {
        let provider = IoSurfaceProvider::initialize().unwrap();
        let generation = Generation::new(1).unwrap();
        let (mut owner, descriptor) = provider.create(4096, generation).unwrap();
        owner.write(17, b"nwipc").unwrap();
        let attached = provider
            .attach(&descriptor, generation, MappingAccess::ReadOnly)
            .unwrap();
        let mut output = [0; 5];
        attached.read(17, &mut output).unwrap();
        assert_eq!(&output, b"nwipc");
        assert_eq!(
            provider
                .attach(
                    &descriptor,
                    Generation::new(2).unwrap(),
                    MappingAccess::ReadOnly,
                )
                .unwrap_err()
                .code(),
            ErrorCode::StaleGeneration
        );
        assert_eq!(
            owner.read(4095, &mut [0; 2]).unwrap_err().code(),
            ErrorCode::InvalidRange
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn two_process_raw_byte_visibility() {
        const ENVIRONMENT: &str = "NWIPC_IOSURFACE_PROCESS_DESCRIPTOR";
        if std::env::var_os(ENVIRONMENT).is_some() {
            return;
        }
        let provider = IoSurfaceProvider::initialize().unwrap();
        let generation = Generation::new(11).unwrap();
        let (mut mapping, descriptor) = provider.create(4096, generation).unwrap();
        mapping.write(31, b"parent").unwrap();
        let encoded = descriptor.encode().unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::iosurface_process_child", "--nocapture"])
            .env(ENVIRONMENT, encode_hex(&encoded))
            .status()
            .unwrap();
        assert!(status.success());
        let mut output = [0; 5];
        mapping.read(31, &mut output).unwrap();
        assert_eq!(&output, b"child");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn iosurface_process_child() {
        let Ok(encoded) = std::env::var("NWIPC_IOSURFACE_PROCESS_DESCRIPTOR") else {
            return;
        };
        let descriptor = IoSurfaceDescriptor::decode(&decode_hex(&encoded)).unwrap();
        let generation = descriptor.generation();
        let provider = IoSurfaceProvider::initialize().unwrap();
        let mut mapping = provider
            .attach(&descriptor, generation, MappingAccess::ReadWrite)
            .unwrap();
        let mut input = [0; 6];
        mapping.read(31, &mut input).unwrap();
        assert_eq!(&input, b"parent");
        mapping.write(31, b"child").unwrap();
    }

    #[cfg(target_os = "macos")]
    fn encode_hex(bytes: &[u8]) -> String {
        bytes.iter().fold(String::new(), |mut output, byte| {
            use std::fmt::Write as _;
            write!(output, "{byte:02x}").unwrap();
            output
        })
    }

    #[cfg(target_os = "macos")]
    fn decode_hex(value: &str) -> Vec<u8> {
        value
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let pair = std::str::from_utf8(pair).unwrap();
                u8::from_str_radix(pair, 16).unwrap()
            })
            .collect()
    }
}
