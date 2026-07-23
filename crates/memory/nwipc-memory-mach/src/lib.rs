//! Mach memory-entry shared-memory provider.
//!
//! A descriptor contains only an opaque, redacted bootstrap service name and fixed-width metadata. The
//! service transfers a Mach memory-entry send right on demand, so port names are never serialized
//! across task namespaces.

use std::fmt;

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_mach_transfer::OwnedMachSendRight;
use nwipc_memory_api::{MappedRegion, MappingAccess, RegionDescriptor, SharedMemoryProvider};
use nwipc_types::Generation;

const PREFIX: &str = "com.nwipc.memory-mach.v1";
const METADATA_LENGTH: usize = 16;
const MAXIMUM_NAME_LENGTH: usize = 127;

/// Transferable rendezvous descriptor for one Mach memory entry.
#[derive(Clone, Eq, PartialEq)]
pub struct MachMemoryDescriptor {
    service_name: String,
    byte_len: usize,
    generation: Generation,
}

impl MachMemoryDescriptor {
    /// Encodes generation, logical length, and the opaque rendezvous name.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRange` when the native length cannot fit the wire representation.
    pub fn encode(&self) -> Result<Vec<u8>, ErrorReport> {
        let byte_len = u64::try_from(self.byte_len)
            .map_err(|_| memory_error(ErrorCode::InvalidRange, "encode Mach memory descriptor"))?;
        let mut output = Vec::with_capacity(METADATA_LENGTH + self.service_name.len());
        output.extend_from_slice(&self.generation.get().to_le_bytes());
        output.extend_from_slice(&byte_len.to_le_bytes());
        output.extend_from_slice(self.service_name.as_bytes());
        Ok(output)
    }

    /// Decodes bounded descriptor metadata.
    ///
    /// # Errors
    ///
    /// Rejects zero generation/length, malformed names, and non-canonical lengths.
    pub fn decode(input: &[u8]) -> Result<Self, ErrorReport> {
        if input.len() <= METADATA_LENGTH {
            return Err(memory_error(
                ErrorCode::Truncated,
                "decode Mach memory descriptor",
            ));
        }
        let generation =
            Generation::new(u64::from_le_bytes(input[..8].try_into().map_err(|_| {
                memory_error(ErrorCode::Truncated, "decode Mach memory descriptor")
            })?))
            .ok_or_else(|| {
                memory_error(ErrorCode::StaleGeneration, "decode Mach memory descriptor")
            })?;
        let wire_len =
            u64::from_le_bytes(input[8..16].try_into().map_err(|_| {
                memory_error(ErrorCode::Truncated, "decode Mach memory descriptor")
            })?);
        let byte_len = usize::try_from(wire_len)
            .map_err(|_| memory_error(ErrorCode::InvalidRange, "decode Mach memory descriptor"))?;
        let service_name = std::str::from_utf8(&input[16..]).map_err(|_| {
            memory_error(
                ErrorCode::ProtocolViolation,
                "decode Mach memory descriptor",
            )
        })?;
        if byte_len == 0
            || service_name.len() > MAXIMUM_NAME_LENGTH
            || !service_name.starts_with(PREFIX)
            || service_name.bytes().any(|byte| byte == 0)
        {
            return Err(memory_error(
                ErrorCode::ProtocolViolation,
                "decode Mach memory descriptor",
            ));
        }
        Ok(Self {
            service_name: service_name.to_owned(),
            byte_len,
            generation,
        })
    }
}

impl fmt::Debug for MachMemoryDescriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MachMemoryDescriptor")
            .field("service_name", &"<redacted>")
            .field("byte_len", &self.byte_len)
            .field("generation", &self.generation)
            .finish()
    }
}

impl RegionDescriptor for MachMemoryDescriptor {
    fn byte_len(&self) -> usize {
        self.byte_len
    }

    fn generation(&self) -> Generation {
        self.generation
    }
}

/// Mach memory-entry provider capability.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MachMemoryProvider;

/// Non-secret Mach provider characteristics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MachMemoryProviderDiagnostics {
    /// Memory entries are transferred as Mach send rights, not numeric port names.
    pub capability_transfer: bool,
    /// Native VM protections enforce read-only attachments.
    pub native_read_only_enforced: bool,
    /// Process-boundary mappings are supported.
    pub cross_process: bool,
}

impl MachMemoryProvider {
    /// Initializes the provider on macOS.
    ///
    /// # Errors
    ///
    /// Returns explicit `Unsupported` on other platforms.
    pub fn initialize() -> Result<Self, ErrorReport> {
        if cfg!(target_os = "macos") {
            Ok(Self)
        } else {
            Err(ErrorReport::unsupported("initialize Mach memory provider"))
        }
    }

    /// Returns redacted provider capabilities.
    pub const fn diagnostics(self) -> MachMemoryProviderDiagnostics {
        MachMemoryProviderDiagnostics {
            capability_transfer: cfg!(target_os = "macos"),
            native_read_only_enforced: cfg!(target_os = "macos"),
            cross_process: cfg!(target_os = "macos"),
        }
    }

    /// Creates a host mapping whose memory-entry right is exported only through authenticated
    /// native capability transfer.
    ///
    /// # Errors
    ///
    /// Returns typed allocation and memory-entry creation failures.
    pub fn create_transfer_mapping(
        self,
        byte_len: usize,
    ) -> Result<MachMemoryMapping, ErrorReport> {
        platform::create_transfer(byte_len)
    }
}

impl SharedMemoryProvider for MachMemoryProvider {
    type Descriptor = MachMemoryDescriptor;
    type Mapping = MachMemoryMapping;

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
            return Err(memory_error(
                ErrorCode::StaleGeneration,
                "attach Mach memory",
            ));
        }
        platform::attach(descriptor, access)
    }
}

/// Owned native VM mapping.
pub struct MachMemoryMapping {
    inner: platform::Mapping,
    byte_len: usize,
    access: MappingAccess,
}

impl MachMemoryMapping {
    /// Duplicates the backing memory-entry send right for an authenticated native transfer.
    ///
    /// The returned task-local name owns one send-right reference. It must be adopted immediately
    /// by a Mach message wrapper and must never be serialized or logged.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the mapping has no transferable entry or duplication fails.
    pub fn duplicate_memory_entry_right(&self) -> Result<OwnedMachSendRight, ErrorReport> {
        let raw = platform::duplicate_entry(&self.inner)?;
        unsafe { OwnedMachSendRight::from_raw(raw) }
    }

    /// Maps a memory-entry send right already transferred into the current task.
    ///
    /// Ownership is represented by `OwnedMachSendRight`; the right is consumed on both success and
    /// failure.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid lengths, missing rights, or native mapping failure.
    pub fn attach_memory_entry_right(
        right: OwnedMachSendRight,
        byte_len: usize,
        access: MappingAccess,
    ) -> Result<Self, ErrorReport> {
        unsafe { platform::attach_entry(right.into_raw(), byte_len, access) }
    }
}

impl fmt::Debug for MachMemoryMapping {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MachMemoryMapping")
            .field("native", &"<redacted>")
            .field("byte_len", &self.byte_len)
            .field("access", &self.access)
            .finish_non_exhaustive()
    }
}

impl MappedRegion for MachMemoryMapping {
    fn len(&self) -> usize {
        self.byte_len
    }

    fn access(&self) -> MappingAccess {
        self.access
    }

    fn read(&self, offset: usize, output: &mut [u8]) -> Result<(), ErrorReport> {
        checked_end(offset, output.len(), self.byte_len, "read Mach memory")?;
        platform::read(&self.inner, offset, output);
        Ok(())
    }

    fn write(&mut self, offset: usize, input: &[u8]) -> Result<(), ErrorReport> {
        if self.access != MappingAccess::ReadWrite {
            return Err(memory_error(
                ErrorCode::RequiredCapabilityMissing,
                "write read-only Mach memory",
            ));
        }
        checked_end(offset, input.len(), self.byte_len, "write Mach memory")?;
        platform::write(&self.inner, offset, input);
        Ok(())
    }

    fn load_u32_acquire(&self, offset: usize) -> Result<u32, ErrorReport> {
        checked_atomic_end(offset, self.byte_len, "load Mach memory atomic")?;
        platform::load_u32_acquire(&self.inner, offset)
    }

    fn store_u32_release(&mut self, offset: usize, value: u32) -> Result<(), ErrorReport> {
        if self.access != MappingAccess::ReadWrite {
            return Err(memory_error(
                ErrorCode::RequiredCapabilityMissing,
                "store read-only Mach memory atomic",
            ));
        }
        checked_atomic_end(offset, self.byte_len, "store Mach memory atomic")?;
        platform::store_u32_release(&self.inner, offset, value)
    }
}

fn checked_end(
    offset: usize,
    length: usize,
    byte_len: usize,
    operation: &'static str,
) -> Result<(), ErrorReport> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| memory_error(ErrorCode::InvalidRange, operation))?;
    if end > byte_len {
        return Err(memory_error(ErrorCode::InvalidRange, operation));
    }
    Ok(())
}

fn checked_atomic_end(
    offset: usize,
    byte_len: usize,
    operation: &'static str,
) -> Result<(), ErrorReport> {
    if offset % align_of::<u32>() != 0 {
        return Err(memory_error(ErrorCode::InvalidAlignment, operation));
    }
    checked_end(offset, size_of::<u32>(), byte_len, operation)
}

fn memory_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Memory,
        code,
        if code == ErrorCode::StaleGeneration {
            Recoverability::ReplaceEndpoint
        } else {
            Recoverability::Terminal
        },
        operation,
    )
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{CString, c_char};
    use std::fs::File;
    use std::io::Read;
    use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};
    use std::thread::JoinHandle;

    use super::{
        ErrorCode, ErrorReport, Generation, MachMemoryDescriptor, MachMemoryMapping, MappingAccess,
        PREFIX, memory_error,
    };

    type MachPort = u32;
    type KernReturn = i32;
    type MachVmAddress = u64;
    type MachVmSize = u64;

    const KERN_SUCCESS: KernReturn = 0;
    const MACH_PORT_NULL: MachPort = 0;
    const MACH_PORT_RIGHT_RECEIVE: i32 = 1;
    const MACH_MSG_TYPE_MOVE_SEND_ONCE: u8 = 18;
    const MACH_MSG_TYPE_COPY_SEND: u8 = 19;
    const MACH_MSG_TYPE_MAKE_SEND: u32 = 20;
    const MACH_MSG_TYPE_MAKE_SEND_ONCE: u32 = 21;
    const MACH_MSG_PORT_DESCRIPTOR: u8 = 0;
    const MACH_MSGH_BITS_COMPLEX: u32 = 0x8000_0000;
    const MACH_SEND_MSG: u32 = 1;
    const MACH_RCV_MSG: u32 = 2;
    const VM_FLAGS_ANYWHERE: i32 = 1;
    const VM_PROT_READ: i32 = 1;
    const VM_PROT_WRITE: i32 = 2;
    const VM_INHERIT_NONE: i32 = 2;

    static NEXT_SERVICE: AtomicU64 = AtomicU64::new(1);

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct MessageHeader {
        bits: u32,
        size: u32,
        remote_port: MachPort,
        local_port: MachPort,
        voucher_port: MachPort,
        id: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct MessageBody {
        descriptor_count: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct PortDescriptor {
        name: MachPort,
        pad1: u32,
        pad2: u16,
        disposition: u8,
        descriptor_type: u8,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct PortMessage {
        header: MessageHeader,
        body: MessageBody,
        port: PortDescriptor,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RequestBuffer {
        header: MessageHeader,
        trailer: [u8; 32],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct PortMessageBuffer {
        message: PortMessage,
        trailer: [u8; 32],
    }

    #[link(name = "System")]
    unsafe extern "C" {
        static mach_task_self_: MachPort;
        static bootstrap_port: MachPort;
        fn mach_vm_allocate(
            task: MachPort,
            address: *mut MachVmAddress,
            size: MachVmSize,
            flags: i32,
        ) -> KernReturn;
        fn mach_vm_deallocate(
            task: MachPort,
            address: MachVmAddress,
            size: MachVmSize,
        ) -> KernReturn;
        fn mach_make_memory_entry_64(
            task: MachPort,
            size: *mut MachVmSize,
            offset: MachVmAddress,
            permission: i32,
            object: *mut MachPort,
            parent: MachPort,
        ) -> KernReturn;
        fn mach_vm_map(
            task: MachPort,
            address: *mut MachVmAddress,
            size: MachVmSize,
            mask: MachVmAddress,
            flags: i32,
            object: MachPort,
            offset: MachVmAddress,
            copy: i32,
            current_protection: i32,
            maximum_protection: i32,
            inheritance: i32,
        ) -> KernReturn;
        fn mach_port_allocate(task: MachPort, right: i32, name: *mut MachPort) -> KernReturn;
        fn mach_port_insert_right(
            task: MachPort,
            name: MachPort,
            poly: MachPort,
            poly_poly: u32,
        ) -> KernReturn;
        fn mach_port_deallocate(task: MachPort, name: MachPort) -> KernReturn;
        fn mach_port_destroy(task: MachPort, name: MachPort) -> KernReturn;
        fn mach_port_mod_refs(task: MachPort, name: MachPort, right: i32, delta: i32)
        -> KernReturn;
        fn bootstrap_register(
            bootstrap: MachPort,
            service_name: *const c_char,
            service: MachPort,
        ) -> KernReturn;
        fn bootstrap_look_up(
            bootstrap: MachPort,
            service_name: *const c_char,
            service: *mut MachPort,
        ) -> KernReturn;
        fn mach_msg(
            message: *mut MessageHeader,
            option: u32,
            send_size: u32,
            receive_limit: u32,
            receive_name: MachPort,
            timeout: u32,
            notify: MachPort,
        ) -> KernReturn;
    }

    pub(super) struct Mapping {
        address: MachVmAddress,
        allocated_len: MachVmSize,
        service: Option<Service>,
        entry: MachPort,
    }

    unsafe impl Send for Mapping {}

    impl Drop for Mapping {
        fn drop(&mut self) {
            self.service.take();
            let task = unsafe { mach_task_self_ };
            if self.entry != MACH_PORT_NULL {
                let _ = unsafe { mach_port_deallocate(task, self.entry) };
            }
            let _ = unsafe { mach_vm_deallocate(task, self.address, self.allocated_len) };
        }
    }

    struct Service {
        control: MachPort,
        worker: Option<JoinHandle<()>>,
    }

    impl Drop for Service {
        fn drop(&mut self) {
            let task = unsafe { mach_task_self_ };
            let _ = unsafe { mach_port_destroy(task, self.control) };
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    pub(super) fn create(
        byte_len: usize,
        generation: Generation,
    ) -> Result<(MachMemoryMapping, MachMemoryDescriptor), ErrorReport> {
        if byte_len == 0 {
            return Err(memory_error(ErrorCode::InvalidRange, "create Mach memory"));
        }
        let allocated_len = MachVmSize::try_from(byte_len)
            .map_err(|_| memory_error(ErrorCode::InvalidRange, "create Mach memory"))?;
        let task = unsafe { mach_task_self_ };
        let mut address = 0;
        check(
            unsafe { mach_vm_allocate(task, &raw mut address, allocated_len, VM_FLAGS_ANYWHERE) },
            "allocate Mach memory",
        )?;
        let mut entry_size = allocated_len;
        let mut entry = MACH_PORT_NULL;
        if let Err(error) = check(
            unsafe {
                mach_make_memory_entry_64(
                    task,
                    &raw mut entry_size,
                    address,
                    VM_PROT_READ | VM_PROT_WRITE,
                    &raw mut entry,
                    MACH_PORT_NULL,
                )
            },
            "create Mach memory entry",
        ) {
            let _ = unsafe { mach_vm_deallocate(task, address, allocated_len) };
            return Err(error);
        }
        if entry_size < allocated_len {
            let _ = unsafe { mach_port_deallocate(task, entry) };
            let _ = unsafe { mach_vm_deallocate(task, address, allocated_len) };
            return Err(memory_error(
                ErrorCode::InvalidRange,
                "validate Mach memory entry",
            ));
        }
        let (service_name, service) = match create_service(entry, generation) {
            Ok(output) => output,
            Err(error) => {
                let _ = unsafe { mach_port_deallocate(task, entry) };
                let _ = unsafe { mach_vm_deallocate(task, address, allocated_len) };
                return Err(error);
            }
        };
        if let Err(error) = copy_send_right(entry, "retain Mach memory entry") {
            drop(service);
            let _ = unsafe { mach_vm_deallocate(task, address, allocated_len) };
            return Err(error);
        }
        Ok((
            MachMemoryMapping {
                inner: Mapping {
                    address,
                    allocated_len,
                    service: Some(service),
                    entry,
                },
                byte_len,
                access: MappingAccess::ReadWrite,
            },
            MachMemoryDescriptor {
                service_name,
                byte_len,
                generation,
            },
        ))
    }

    pub(super) fn create_transfer(byte_len: usize) -> Result<MachMemoryMapping, ErrorReport> {
        if byte_len == 0 {
            return Err(memory_error(
                ErrorCode::InvalidRange,
                "create transferable Mach memory",
            ));
        }
        let allocated_len = MachVmSize::try_from(byte_len).map_err(|_| {
            memory_error(ErrorCode::InvalidRange, "create transferable Mach memory")
        })?;
        let task = unsafe { mach_task_self_ };
        let mut address = 0;
        check(
            unsafe { mach_vm_allocate(task, &raw mut address, allocated_len, VM_FLAGS_ANYWHERE) },
            "allocate transferable Mach memory",
        )?;
        let mut entry_size = allocated_len;
        let mut entry = MACH_PORT_NULL;
        if let Err(error) = check(
            unsafe {
                mach_make_memory_entry_64(
                    task,
                    &raw mut entry_size,
                    address,
                    VM_PROT_READ | VM_PROT_WRITE,
                    &raw mut entry,
                    MACH_PORT_NULL,
                )
            },
            "create transferable Mach memory entry",
        ) {
            let _ = unsafe { mach_vm_deallocate(task, address, allocated_len) };
            return Err(error);
        }
        if entry_size < allocated_len {
            let _ = unsafe { mach_port_deallocate(task, entry) };
            let _ = unsafe { mach_vm_deallocate(task, address, allocated_len) };
            return Err(memory_error(
                ErrorCode::InvalidRange,
                "validate transferable Mach memory entry",
            ));
        }
        Ok(MachMemoryMapping {
            inner: Mapping {
                address,
                allocated_len,
                service: None,
                entry,
            },
            byte_len,
            access: MappingAccess::ReadWrite,
        })
    }

    pub(super) fn attach(
        descriptor: &MachMemoryDescriptor,
        access: MappingAccess,
    ) -> Result<MachMemoryMapping, ErrorReport> {
        let entry = request_entry(&descriptor.service_name)?;
        let size = MachVmSize::try_from(descriptor.byte_len)
            .map_err(|_| memory_error(ErrorCode::InvalidRange, "attach Mach memory"))?;
        let protections = match access {
            MappingAccess::ReadOnly => VM_PROT_READ,
            MappingAccess::ReadWrite => VM_PROT_READ | VM_PROT_WRITE,
        };
        let task = unsafe { mach_task_self_ };
        let mut address = 0;
        let result = check(
            unsafe {
                mach_vm_map(
                    task,
                    &raw mut address,
                    size,
                    0,
                    VM_FLAGS_ANYWHERE,
                    entry,
                    0,
                    0,
                    protections,
                    protections,
                    VM_INHERIT_NONE,
                )
            },
            "map Mach memory entry",
        );
        let _ = unsafe { mach_port_deallocate(task, entry) };
        result?;
        Ok(MachMemoryMapping {
            inner: Mapping {
                address,
                allocated_len: size,
                service: None,
                entry: MACH_PORT_NULL,
            },
            byte_len: descriptor.byte_len,
            access,
        })
    }

    pub(super) fn duplicate_entry(mapping: &Mapping) -> Result<MachPort, ErrorReport> {
        if mapping.entry == MACH_PORT_NULL {
            return Err(memory_error(
                ErrorCode::RequiredCapabilityMissing,
                "duplicate Mach memory entry",
            ));
        }
        copy_send_right(mapping.entry, "duplicate Mach memory entry")?;
        Ok(mapping.entry)
    }

    pub(super) unsafe fn attach_entry(
        entry: MachPort,
        byte_len: usize,
        access: MappingAccess,
    ) -> Result<MachMemoryMapping, ErrorReport> {
        if entry == MACH_PORT_NULL || byte_len == 0 {
            if entry != MACH_PORT_NULL {
                let _ = unsafe { mach_port_deallocate(mach_task_self_, entry) };
            }
            return Err(memory_error(
                ErrorCode::RequiredCapabilityMissing,
                "adopt Mach memory entry",
            ));
        }
        let Ok(size) = MachVmSize::try_from(byte_len) else {
            let _ = unsafe { mach_port_deallocate(mach_task_self_, entry) };
            return Err(memory_error(
                ErrorCode::InvalidRange,
                "attach Mach memory entry",
            ));
        };
        let protections = match access {
            MappingAccess::ReadOnly => VM_PROT_READ,
            MappingAccess::ReadWrite => VM_PROT_READ | VM_PROT_WRITE,
        };
        let mut address = 0;
        let result = check(
            unsafe {
                mach_vm_map(
                    mach_task_self_,
                    &raw mut address,
                    size,
                    0,
                    VM_FLAGS_ANYWHERE,
                    entry,
                    0,
                    0,
                    protections,
                    protections,
                    VM_INHERIT_NONE,
                )
            },
            "map transferred Mach memory entry",
        );
        let _ = unsafe { mach_port_deallocate(mach_task_self_, entry) };
        result?;
        Ok(MachMemoryMapping {
            inner: Mapping {
                address,
                allocated_len: size,
                service: None,
                entry: MACH_PORT_NULL,
            },
            byte_len,
            access,
        })
    }

    fn copy_send_right(port: MachPort, operation: &'static str) -> Result<(), ErrorReport> {
        const MACH_PORT_RIGHT_SEND: i32 = 0;
        check(
            unsafe { mach_port_mod_refs(mach_task_self_, port, MACH_PORT_RIGHT_SEND, 1) },
            operation,
        )
    }

    pub(super) fn read(mapping: &Mapping, offset: usize, output: &mut [u8]) {
        let base = usize::try_from(mapping.address).expect("Mach address fits usize");
        for (index, byte) in output.iter_mut().enumerate() {
            let pointer = (base + offset + index) as *const AtomicU8;
            *byte = unsafe { &*pointer }.load(Ordering::Relaxed);
        }
    }

    pub(super) fn write(mapping: &Mapping, offset: usize, input: &[u8]) {
        let base = usize::try_from(mapping.address).expect("Mach address fits usize");
        for (index, byte) in input.iter().copied().enumerate() {
            let pointer = (base + offset + index) as *const AtomicU8;
            unsafe { &*pointer }.store(byte, Ordering::Relaxed);
        }
    }

    pub(super) fn load_u32_acquire(mapping: &Mapping, offset: usize) -> Result<u32, ErrorReport> {
        let base = usize::try_from(mapping.address)
            .map_err(|_| memory_error(ErrorCode::InvalidRange, "load Mach memory atomic"))?;
        let pointer = (base + offset) as *const AtomicU32;
        if pointer.align_offset(align_of::<AtomicU32>()) != 0 {
            return Err(memory_error(
                ErrorCode::InvalidAlignment,
                "load Mach memory atomic",
            ));
        }
        Ok(unsafe { &*pointer }.load(Ordering::Acquire))
    }

    pub(super) fn store_u32_release(
        mapping: &Mapping,
        offset: usize,
        value: u32,
    ) -> Result<(), ErrorReport> {
        let base = usize::try_from(mapping.address)
            .map_err(|_| memory_error(ErrorCode::InvalidRange, "store Mach memory atomic"))?;
        let pointer = (base + offset) as *const AtomicU32;
        if pointer.align_offset(align_of::<AtomicU32>()) != 0 {
            return Err(memory_error(
                ErrorCode::InvalidAlignment,
                "store Mach memory atomic",
            ));
        }
        unsafe { &*pointer }.store(value, Ordering::Release);
        Ok(())
    }

    fn create_service(
        entry: MachPort,
        generation: Generation,
    ) -> Result<(String, Service), ErrorReport> {
        let nonce = service_nonce()?;
        let task = unsafe { mach_task_self_ };
        let mut control = MACH_PORT_NULL;
        check(
            unsafe { mach_port_allocate(task, MACH_PORT_RIGHT_RECEIVE, &raw mut control) },
            "allocate Mach memory transfer port",
        )?;
        if let Err(error) = check(
            unsafe { mach_port_insert_right(task, control, control, MACH_MSG_TYPE_MAKE_SEND) },
            "insert Mach memory transfer right",
        ) {
            let _ = unsafe { mach_port_destroy(task, control) };
            return Err(error);
        }
        let sequence = NEXT_SERVICE.fetch_add(1, Ordering::Relaxed);
        let service_name = format!(
            "{PREFIX}.{}.{}.{}.{nonce}",
            std::process::id(),
            generation.get(),
            sequence
        );
        let name = CString::new(service_name.as_bytes())
            .map_err(|_| memory_error(ErrorCode::ProtocolViolation, "name Mach memory service"))?;
        let bootstrap = unsafe { bootstrap_port };
        if let Err(error) = check(
            unsafe { bootstrap_register(bootstrap, name.as_ptr(), control) },
            "register Mach memory service",
        ) {
            let _ = unsafe { mach_port_destroy(task, control) };
            return Err(error);
        }
        let worker = std::thread::Builder::new()
            .name("nwipc-mach-memory".into())
            .spawn(move || serve(control, entry));
        let Ok(worker) = worker else {
            let _ = unsafe { mach_port_destroy(task, control) };
            return Err(memory_error(
                ErrorCode::Internal,
                "spawn Mach memory service",
            ));
        };
        Ok((
            service_name,
            Service {
                control,
                worker: Some(worker),
            },
        ))
    }

    fn serve(control: MachPort, entry: MachPort) {
        loop {
            let mut request = RequestBuffer::default();
            let status = unsafe {
                mach_msg(
                    &raw mut request.header,
                    MACH_RCV_MSG,
                    0,
                    u32::try_from(size_of::<RequestBuffer>()).unwrap(),
                    control,
                    0,
                    MACH_PORT_NULL,
                )
            };
            if status != KERN_SUCCESS {
                break;
            }
            let mut response = PortMessage {
                header: MessageHeader {
                    bits: MACH_MSGH_BITS_COMPLEX | u32::from(MACH_MSG_TYPE_MOVE_SEND_ONCE),
                    size: u32::try_from(size_of::<PortMessage>()).unwrap(),
                    remote_port: request.header.remote_port,
                    local_port: MACH_PORT_NULL,
                    voucher_port: MACH_PORT_NULL,
                    id: request.header.id,
                },
                body: MessageBody {
                    descriptor_count: 1,
                },
                port: PortDescriptor {
                    name: entry,
                    disposition: MACH_MSG_TYPE_COPY_SEND,
                    descriptor_type: MACH_MSG_PORT_DESCRIPTOR,
                    ..PortDescriptor::default()
                },
            };
            let _ = unsafe {
                mach_msg(
                    &raw mut response.header,
                    MACH_SEND_MSG,
                    response.header.size,
                    0,
                    MACH_PORT_NULL,
                    0,
                    MACH_PORT_NULL,
                )
            };
        }
        let task = unsafe { mach_task_self_ };
        let _ = unsafe { mach_port_deallocate(task, entry) };
    }

    fn request_entry(service_name: &str) -> Result<MachPort, ErrorReport> {
        let name = CString::new(service_name.as_bytes()).map_err(|_| {
            memory_error(ErrorCode::ProtocolViolation, "lookup Mach memory service")
        })?;
        let task = unsafe { mach_task_self_ };
        let mut service = MACH_PORT_NULL;
        let bootstrap = unsafe { bootstrap_port };
        check(
            unsafe { bootstrap_look_up(bootstrap, name.as_ptr(), &raw mut service) },
            "lookup Mach memory service",
        )?;
        let mut reply = MACH_PORT_NULL;
        if let Err(error) = check(
            unsafe { mach_port_allocate(task, MACH_PORT_RIGHT_RECEIVE, &raw mut reply) },
            "allocate Mach memory reply port",
        ) {
            let _ = unsafe { mach_port_deallocate(task, service) };
            return Err(error);
        }
        let mut request = MessageHeader {
            bits: u32::from(MACH_MSG_TYPE_COPY_SEND) | (MACH_MSG_TYPE_MAKE_SEND_ONCE << 8),
            size: u32::try_from(size_of::<MessageHeader>()).unwrap(),
            remote_port: service,
            local_port: reply,
            voucher_port: MACH_PORT_NULL,
            id: 1,
        };
        let sent = check(
            unsafe {
                mach_msg(
                    &raw mut request,
                    MACH_SEND_MSG,
                    request.size,
                    0,
                    MACH_PORT_NULL,
                    0,
                    MACH_PORT_NULL,
                )
            },
            "request Mach memory entry",
        );
        let _ = unsafe { mach_port_deallocate(task, service) };
        if let Err(error) = sent {
            let _ = unsafe { mach_port_destroy(task, reply) };
            return Err(error);
        }
        let mut response = PortMessageBuffer::default();
        let received = check(
            unsafe {
                mach_msg(
                    &raw mut response.message.header,
                    MACH_RCV_MSG,
                    0,
                    u32::try_from(size_of::<PortMessageBuffer>()).unwrap(),
                    reply,
                    0,
                    MACH_PORT_NULL,
                )
            },
            "receive Mach memory entry",
        );
        let _ = unsafe { mach_port_destroy(task, reply) };
        received?;
        if response.message.body.descriptor_count != 1
            || response.message.port.descriptor_type != MACH_MSG_PORT_DESCRIPTOR
            || response.message.port.name == MACH_PORT_NULL
        {
            return Err(memory_error(
                ErrorCode::ProtocolViolation,
                "validate Mach memory entry response",
            ));
        }
        Ok(response.message.port.name)
    }

    fn check(status: KernReturn, operation: &'static str) -> Result<(), ErrorReport> {
        if status == KERN_SUCCESS {
            Ok(())
        } else {
            Err(memory_error(
                ErrorCode::RequiredCapabilityMissing,
                operation,
            ))
        }
    }

    fn service_nonce() -> Result<String, ErrorReport> {
        let mut bytes = [0_u8; 16];
        File::open("/dev/urandom")
            .and_then(|mut source| source.read_exact(&mut bytes))
            .map_err(|_| memory_error(ErrorCode::Internal, "randomize Mach memory service"))?;
        Ok(bytes.iter().fold(String::new(), |mut output, byte| {
            use std::fmt::Write as _;
            write!(output, "{byte:02x}").expect("writing to String cannot fail");
            output
        }))
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::{ErrorReport, Generation, MachMemoryDescriptor, MachMemoryMapping, MappingAccess};

    pub(super) struct Mapping;

    pub(super) fn create(
        _: usize,
        _: Generation,
    ) -> Result<(MachMemoryMapping, MachMemoryDescriptor), ErrorReport> {
        Err(ErrorReport::unsupported("create Mach memory"))
    }

    pub(super) fn create_transfer(_: usize) -> Result<MachMemoryMapping, ErrorReport> {
        Err(ErrorReport::unsupported("create transferable Mach memory"))
    }

    pub(super) fn attach(
        _: &MachMemoryDescriptor,
        _: MappingAccess,
    ) -> Result<MachMemoryMapping, ErrorReport> {
        Err(ErrorReport::unsupported("attach Mach memory"))
    }

    pub(super) fn duplicate_entry(_: &Mapping) -> Result<u32, ErrorReport> {
        Err(ErrorReport::unsupported("duplicate Mach memory entry"))
    }

    pub(super) unsafe fn attach_entry(
        _: u32,
        _: usize,
        _: MappingAccess,
    ) -> Result<MachMemoryMapping, ErrorReport> {
        Err(ErrorReport::unsupported("attach Mach memory entry"))
    }

    pub(super) const fn read(_: &Mapping, _: usize, _: &mut [u8]) {}
    pub(super) const fn write(_: &Mapping, _: usize, _: &[u8]) {}

    pub(super) fn load_u32_acquire(_: &Mapping, _: usize) -> Result<u32, ErrorReport> {
        Err(ErrorReport::unsupported("load Mach memory atomic"))
    }

    pub(super) fn store_u32_release(_: &Mapping, _: usize, _: u32) -> Result<(), ErrorReport> {
        Err(ErrorReport::unsupported("store Mach memory atomic"))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use std::process::Command;

    use super::*;

    fn synthetic_descriptor() -> MachMemoryDescriptor {
        MachMemoryDescriptor {
            service_name: format!("{PREFIX}.synthetic"),
            byte_len: 4096,
            generation: Generation::new(3).unwrap(),
        }
    }

    #[test]
    fn descriptor_round_trip_is_redacted() {
        let descriptor = synthetic_descriptor();
        assert_eq!(
            MachMemoryDescriptor::decode(&descriptor.encode().unwrap()).unwrap(),
            descriptor
        );
        assert!(!format!("{descriptor:?}").contains("synthetic"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn create_attach_and_native_protection_contract() {
        let provider = MachMemoryProvider::initialize().unwrap();
        let generation = Generation::new(1).unwrap();
        let (mut owner, descriptor) = provider.create(4096, generation).unwrap();
        owner.write(19, b"nwipc").unwrap();
        let attached = provider
            .attach(&descriptor, generation, MappingAccess::ReadOnly)
            .unwrap();
        let mut output = [0; 5];
        attached.read(19, &mut output).unwrap();
        assert_eq!(&output, b"nwipc");
        assert_eq!(
            owner.read(4095, &mut [0; 2]).unwrap_err().code(),
            ErrorCode::InvalidRange
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn two_process_memory_entry_visibility() {
        const ENVIRONMENT: &str = "NWIPC_MACH_MEMORY_DESCRIPTOR";
        if std::env::var_os(ENVIRONMENT).is_some() {
            return;
        }
        let provider = MachMemoryProvider::initialize().unwrap();
        let generation = Generation::new(11).unwrap();
        let (mut owner, descriptor) = provider.create(4096, generation).unwrap();
        owner.write(31, b"parent").unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::mach_memory_process_child", "--nocapture"])
            .env(ENVIRONMENT, encode_hex(&descriptor.encode().unwrap()))
            .status()
            .unwrap();
        assert!(status.success());
        let mut output = [0; 5];
        owner.read(31, &mut output).unwrap();
        assert_eq!(&output, b"child");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mach_memory_process_child() {
        let Ok(encoded) = std::env::var("NWIPC_MACH_MEMORY_DESCRIPTOR") else {
            return;
        };
        let descriptor = MachMemoryDescriptor::decode(&decode_hex(&encoded)).unwrap();
        let provider = MachMemoryProvider::initialize().unwrap();
        let mut mapping = provider
            .attach(
                &descriptor,
                descriptor.generation(),
                MappingAccess::ReadWrite,
            )
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
