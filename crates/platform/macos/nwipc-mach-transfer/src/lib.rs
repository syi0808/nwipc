//! One-shot, identity-bound Mach capability transfer.
//!
//! This crate deliberately does not discover a control port through the global bootstrap
//! namespace. A platform adapter must first deliver one end of [`AuthenticatedControlEndpoint`]
//! over an already authenticated control plane. The host then moves exactly two memory-entry send
//! rights, one signal send right, and one signal receive right in a single Mach message.
//!
//! Task-local port names never enter an encoded descriptor, property list, diagnostic, or debug
//! representation. Raw-name constructors exist only to bridge native code that has already moved a
//! right into the current task.

use core::fmt;

use nwipc_bootstrap_schema::EndpointRole;
use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_types::{Generation, SessionId};

const CAPABILITY_COUNT: usize = 4;
#[cfg(target_os = "macos")]
const CAPABILITY_COUNT_WIRE: u8 = 4;
#[cfg(target_os = "macos")]
const CONTROL_VERSION: u16 = 1;
#[cfg(target_os = "macos")]
const CONTROL_MAGIC: [u8; 8] = *b"NWIPCMX1";

/// Session identity, generation, and endpoint role authenticated by the control plane.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct TransferIdentity {
    session_id: SessionId,
    generation: Generation,
    role: EndpointRole,
}

impl TransferIdentity {
    /// Creates the identity expected by exactly one endpoint attach.
    pub const fn new(session_id: SessionId, generation: Generation, role: EndpointRole) -> Self {
        Self {
            session_id,
            generation,
            role,
        }
    }

    /// Session bound to the transferred rights.
    pub const fn session_id(self) -> SessionId {
        self.session_id
    }

    /// Resource generation bound to the transferred rights.
    pub const fn generation(self) -> Generation {
        self.generation
    }

    /// Only endpoint role allowed to consume the transfer.
    pub const fn role(self) -> EndpointRole {
        self.role
    }
}

impl fmt::Debug for TransferIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransferIdentity")
            .field("session_id", &self.session_id)
            .field("generation", &self.generation)
            .field("role", &self.role)
            .finish()
    }
}

/// Fixed semantic slot for one transferred Mach right.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CapabilityKind {
    /// Writable memory entry for frames produced by this endpoint.
    OutboundMemory = 1,
    /// Readable memory entry for frames consumed by this endpoint.
    InboundMemory = 2,
    /// Send right used to hint the remote listener.
    OutboundSignal = 3,
    /// Receive right moved to the sole local listener.
    InboundSignal = 4,
}

impl CapabilityKind {
    #[cfg(target_os = "macos")]
    const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::OutboundMemory),
            2 => Some(Self::InboundMemory),
            3 => Some(Self::OutboundSignal),
            4 => Some(Self::InboundSignal),
            _ => None,
        }
    }
}

const CANONICAL_KINDS: [CapabilityKind; CAPABILITY_COUNT] = [
    CapabilityKind::OutboundMemory,
    CapabilityKind::InboundMemory,
    CapabilityKind::OutboundSignal,
    CapabilityKind::InboundSignal,
];

/// Redacted, fixed-shape metadata accompanying a capability message.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct TransferManifest {
    identity: TransferIdentity,
    kinds: [CapabilityKind; CAPABILITY_COUNT],
}

impl TransferManifest {
    /// Creates the canonical four-right manifest for one endpoint.
    pub const fn new(identity: TransferIdentity) -> Self {
        Self {
            identity,
            kinds: CANONICAL_KINDS,
        }
    }

    /// Identity authenticated before provider attachment.
    pub const fn identity(self) -> TransferIdentity {
        self.identity
    }

    /// Ordered descriptor meanings.
    pub const fn kinds(self) -> [CapabilityKind; CAPABILITY_COUNT] {
        self.kinds
    }

    fn validate_for(self, expected: TransferIdentity) -> Result<(), ErrorReport> {
        if self.identity.session_id != expected.session_id || self.identity.role != expected.role {
            return Err(transfer_error(
                ErrorCategory::Security,
                ErrorCode::AuthenticationFailed,
                "authenticate Mach capability endpoint",
            ));
        }
        if self.identity.generation != expected.generation {
            return Err(transfer_error(
                ErrorCategory::Bootstrap,
                ErrorCode::StaleGeneration,
                "validate Mach capability generation",
            ));
        }
        if self.kinds != CANONICAL_KINDS {
            return Err(transfer_error(
                ErrorCategory::Protocol,
                ErrorCode::ProtocolViolation,
                "validate Mach capability metadata",
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for TransferManifest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransferManifest")
            .field("identity", &self.identity)
            .field("kinds", &self.kinds)
            .finish()
    }
}

/// Owned Mach send right.
///
/// The raw constructor is a native ownership boundary, not a serialized handle.
pub struct OwnedMachSendRight {
    raw: platform::MachPort,
}

impl OwnedMachSendRight {
    /// Takes ownership of one send-right reference already valid in the current task.
    ///
    /// # Errors
    ///
    /// Rejects a null right or returns `Unsupported` outside macOS.
    ///
    /// # Safety
    ///
    /// `raw` must name one owned send-right reference in the current task. The caller must not
    /// independently deallocate that reference after this call.
    pub unsafe fn from_raw(raw: u32) -> Result<Self, ErrorReport> {
        platform::validate_raw(raw, "adopt Mach send right")?;
        Ok(Self { raw })
    }

    /// Releases the wrapper without deallocating the native right.
    ///
    /// This is only for a provider adapter that immediately consumes the right in the current
    /// task. The returned value must not be serialized, logged, or placed in a property list.
    pub fn into_raw(mut self) -> u32 {
        let raw = self.raw;
        self.raw = platform::MACH_PORT_NULL;
        raw
    }
}

impl fmt::Debug for OwnedMachSendRight {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OwnedMachSendRight(<redacted>)")
    }
}

impl Drop for OwnedMachSendRight {
    fn drop(&mut self) {
        if self.raw != platform::MACH_PORT_NULL {
            platform::drop_send(self.raw);
        }
    }
}

/// Owned Mach receive right.
///
/// The raw constructor is a native ownership boundary, not a serialized handle.
pub struct OwnedMachReceiveRight {
    raw: platform::MachPort,
}

impl OwnedMachReceiveRight {
    /// Takes ownership of a receive right already valid in the current task.
    ///
    /// # Errors
    ///
    /// Rejects a null right or returns `Unsupported` outside macOS.
    ///
    /// # Safety
    ///
    /// `raw` must name an owned receive right in the current task. No other owner may destroy it.
    pub unsafe fn from_raw(raw: u32) -> Result<Self, ErrorReport> {
        platform::validate_raw(raw, "adopt Mach receive right")?;
        Ok(Self { raw })
    }

    /// Releases the wrapper without destroying the native right.
    ///
    /// This is only for an authenticated native bridge that immediately moves the right to another
    /// task. The returned value must not be serialized, logged, or placed in a property list.
    pub fn into_raw(mut self) -> u32 {
        let raw = self.raw;
        self.raw = platform::MACH_PORT_NULL;
        raw
    }
}

impl fmt::Debug for OwnedMachReceiveRight {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OwnedMachReceiveRight(<redacted>)")
    }
}

impl Drop for OwnedMachReceiveRight {
    fn drop(&mut self) {
        if self.raw != platform::MACH_PORT_NULL {
            platform::drop_receive(self.raw);
        }
    }
}

/// Host-owned rights waiting for a single atomic transfer.
pub struct MachCapabilityBundle {
    manifest: TransferManifest,
    outbound_memory: Option<OwnedMachSendRight>,
    inbound_memory: Option<OwnedMachSendRight>,
    outbound_signal: Option<OwnedMachSendRight>,
    inbound_signal: Option<OwnedMachReceiveRight>,
}

impl MachCapabilityBundle {
    /// Binds exactly four provider rights to their endpoint metadata.
    pub const fn new(
        identity: TransferIdentity,
        outbound_memory: OwnedMachSendRight,
        inbound_memory: OwnedMachSendRight,
        outbound_signal: OwnedMachSendRight,
        inbound_signal: OwnedMachReceiveRight,
    ) -> Self {
        Self {
            manifest: TransferManifest::new(identity),
            outbound_memory: Some(outbound_memory),
            inbound_memory: Some(inbound_memory),
            outbound_signal: Some(outbound_signal),
            inbound_signal: Some(inbound_signal),
        }
    }

    /// Redacted descriptor metadata.
    pub const fn manifest(&self) -> TransferManifest {
        self.manifest
    }

    fn take_raw(&mut self) -> [platform::MachPort; CAPABILITY_COUNT] {
        [
            self.outbound_memory
                .take()
                .expect("complete capability bundle")
                .into_raw(),
            self.inbound_memory
                .take()
                .expect("complete capability bundle")
                .into_raw(),
            self.outbound_signal
                .take()
                .expect("complete capability bundle")
                .into_raw(),
            self.inbound_signal
                .take()
                .expect("complete capability bundle")
                .into_raw(),
        ]
    }
}

impl fmt::Debug for MachCapabilityBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MachCapabilityBundle")
            .field("manifest", &self.manifest)
            .field("rights", &"<redacted:4>")
            .finish_non_exhaustive()
    }
}

impl Drop for MachCapabilityBundle {
    fn drop(&mut self) {
        self.inbound_signal.take();
        self.outbound_signal.take();
        self.inbound_memory.take();
        self.outbound_memory.take();
    }
}

/// Host side of an authenticated, one-shot control endpoint.
pub struct AuthenticatedControlHost {
    identity: TransferIdentity,
    send: Option<OwnedMachSendRight>,
    attempted: bool,
}

impl AuthenticatedControlHost {
    /// Reconstructs the host side after an authenticated native control-plane handoff.
    ///
    /// # Errors
    ///
    /// Rejects a null right or returns `Unsupported` outside macOS.
    ///
    /// # Safety
    ///
    /// `raw` must be an owned send right for a control receive right held only by the endpoint
    /// identified by `identity`.
    pub unsafe fn from_raw_send_right(
        identity: TransferIdentity,
        raw: u32,
    ) -> Result<Self, ErrorReport> {
        Ok(Self {
            identity,
            // SAFETY: Forwarded native ownership contract is identical.
            send: Some(unsafe { OwnedMachSendRight::from_raw(raw)? }),
            attempted: false,
        })
    }

    /// Atomically moves all four rights to the bound endpoint.
    ///
    /// The endpoint is one-shot even when validation or native send fails. All untransferred rights
    /// are then released in reverse acquisition order.
    ///
    /// # Errors
    ///
    /// Rejects replay, wrong session/role, stale generation, and native send failure.
    pub fn transfer(&mut self, mut bundle: MachCapabilityBundle) -> Result<(), ErrorReport> {
        if self.attempted {
            return Err(transfer_error(
                ErrorCategory::Security,
                ErrorCode::ReplayDetected,
                "replay Mach capability transfer",
            ));
        }
        self.attempted = true;
        bundle.manifest.validate_for(self.identity)?;
        let control = self.send.take().ok_or_else(|| {
            transfer_error(
                ErrorCategory::Closed,
                ErrorCode::Closed,
                "closed Mach capability endpoint",
            )
        })?;
        let rights = bundle.take_raw();
        platform::send(control, bundle.manifest, rights)
    }
}

impl fmt::Debug for AuthenticatedControlHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatedControlHost")
            .field("identity", &self.identity)
            .field("native", &"<redacted>")
            .field("attempted", &self.attempted)
            .finish_non_exhaustive()
    }
}

/// Endpoint side of an authenticated, one-shot control endpoint.
pub struct AuthenticatedControlEndpoint {
    identity: TransferIdentity,
    receive: Option<OwnedMachReceiveRight>,
    attempted: bool,
}

impl AuthenticatedControlEndpoint {
    /// Reconstructs an endpoint after its receive right was moved by a trusted native bridge.
    ///
    /// # Errors
    ///
    /// Rejects a null right or returns `Unsupported` outside macOS.
    ///
    /// # Safety
    ///
    /// `raw` must be the uniquely owned receive right for this identity's authenticated control
    /// endpoint.
    pub unsafe fn from_raw_receive_right(
        identity: TransferIdentity,
        raw: u32,
    ) -> Result<Self, ErrorReport> {
        Ok(Self {
            identity,
            // SAFETY: Forwarded native ownership contract is identical.
            receive: Some(unsafe { OwnedMachReceiveRight::from_raw(raw)? }),
            attempted: false,
        })
    }

    /// Moves the control receive right into a trusted native bridge.
    ///
    /// The returned task-local name must be transferred as a Mach descriptor, never serialized.
    ///
    /// # Errors
    ///
    /// Returns `Closed` if the endpoint no longer owns its receive right.
    pub fn into_raw_receive_right(mut self) -> Result<u32, ErrorReport> {
        self.receive
            .take()
            .map(OwnedMachReceiveRight::into_raw)
            .ok_or_else(|| {
                transfer_error(
                    ErrorCategory::Closed,
                    ErrorCode::Closed,
                    "move Mach control endpoint",
                )
            })
    }

    /// Receives and authenticates one complete capability set before provider attachment.
    ///
    /// # Errors
    ///
    /// Rejects replay, malformed metadata, wrong endpoint identity, and stale generation. Rights
    /// received with an invalid message are cleaned in reverse order before returning.
    pub fn receive(&mut self) -> Result<EndpointCapabilities, ErrorReport> {
        if self.attempted {
            return Err(transfer_error(
                ErrorCategory::Security,
                ErrorCode::ReplayDetected,
                "replay Mach capability receive",
            ));
        }
        self.attempted = true;
        let receive = self.receive.take().ok_or_else(|| {
            transfer_error(
                ErrorCategory::Closed,
                ErrorCode::Closed,
                "closed Mach capability endpoint",
            )
        })?;
        platform::receive(receive, self.identity)
    }
}

impl fmt::Debug for AuthenticatedControlEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatedControlEndpoint")
            .field("identity", &self.identity)
            .field("native", &"<redacted>")
            .field("attempted", &self.attempted)
            .finish_non_exhaustive()
    }
}

/// Validated endpoint ownership returned only after all metadata checks pass.
pub struct EndpointCapabilities {
    manifest: TransferManifest,
    outbound_memory: Option<OwnedMachSendRight>,
    inbound_memory: Option<OwnedMachSendRight>,
    outbound_signal: Option<OwnedMachSendRight>,
    inbound_signal: Option<OwnedMachReceiveRight>,
}

impl EndpointCapabilities {
    /// Authenticated metadata for this capability set.
    pub const fn manifest(&self) -> TransferManifest {
        self.manifest
    }

    /// Moves the outbound memory-entry send right into a provider adapter.
    pub fn take_outbound_memory(&mut self) -> Option<OwnedMachSendRight> {
        self.outbound_memory.take()
    }

    /// Moves the inbound memory-entry send right into a provider adapter.
    pub fn take_inbound_memory(&mut self) -> Option<OwnedMachSendRight> {
        self.inbound_memory.take()
    }

    /// Moves the outbound signal send right into a provider adapter.
    pub fn take_outbound_signal(&mut self) -> Option<OwnedMachSendRight> {
        self.outbound_signal.take()
    }

    /// Moves the sole inbound signal receive right into a provider adapter.
    pub fn take_inbound_signal(&mut self) -> Option<OwnedMachReceiveRight> {
        self.inbound_signal.take()
    }
}

impl fmt::Debug for EndpointCapabilities {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EndpointCapabilities")
            .field("manifest", &self.manifest)
            .field("rights", &"<redacted:4>")
            .finish_non_exhaustive()
    }
}

impl Drop for EndpointCapabilities {
    fn drop(&mut self) {
        self.inbound_signal.take();
        self.outbound_signal.take();
        self.inbound_memory.take();
        self.outbound_memory.take();
    }
}

/// Creates a fresh control endpoint without any global service registration.
///
/// The endpoint receive right still belongs to the current task. Production adapters must move it
/// through their already authenticated host-to-endpoint control plane before calling `receive`.
///
/// # Errors
///
/// Returns `Unsupported` outside macOS or a typed resource error when the port cannot be created.
pub fn authenticated_control_channel(
    identity: TransferIdentity,
) -> Result<(AuthenticatedControlHost, AuthenticatedControlEndpoint), ErrorReport> {
    let (send, receive) = platform::channel()?;
    Ok((
        AuthenticatedControlHost {
            identity,
            send: Some(send),
            attempted: false,
        },
        AuthenticatedControlEndpoint {
            identity,
            receive: Some(receive),
            attempted: false,
        },
    ))
}

fn transfer_error(
    category: ErrorCategory,
    code: ErrorCode,
    operation: &'static str,
) -> ErrorReport {
    ErrorReport::new(
        category,
        code,
        if matches!(code, ErrorCode::StaleGeneration | ErrorCode::ReplayDetected) {
            Recoverability::ReplaceEndpoint
        } else {
            Recoverability::Terminal
        },
        operation,
    )
}

#[cfg(target_os = "macos")]
mod platform {
    #[cfg(test)]
    use super::MachCapabilityBundle;
    use super::{
        CANONICAL_KINDS, CAPABILITY_COUNT, CAPABILITY_COUNT_WIRE, CONTROL_MAGIC, CONTROL_VERSION,
        CapabilityKind, EndpointCapabilities, EndpointRole, ErrorCategory, ErrorCode, ErrorReport,
        Generation, OwnedMachReceiveRight, OwnedMachSendRight, SessionId, TransferIdentity,
        TransferManifest, transfer_error,
    };

    pub(super) type MachPort = u32;
    type KernReturn = i32;

    pub(super) const MACH_PORT_NULL: MachPort = 0;
    const KERN_SUCCESS: KernReturn = 0;
    const MACH_PORT_RIGHT_RECEIVE: i32 = 1;
    const MACH_MSG_TYPE_MOVE_RECEIVE: u8 = 16;
    const MACH_MSG_TYPE_MOVE_SEND: u8 = 17;
    const MACH_MSG_TYPE_COPY_SEND: u8 = 19;
    const MACH_MSG_TYPE_MAKE_SEND: u32 = 20;
    const MACH_MSG_PORT_DESCRIPTOR: u8 = 0;
    const MACH_MSGH_BITS_COMPLEX: u32 = 0x8000_0000;
    const MACH_SEND_MSG: u32 = 1;
    const MACH_RCV_MSG: u32 = 2;
    const TRANSFER_MESSAGE_ID: i32 = 0x4e57_4d31;
    #[cfg(test)]
    const VM_FLAGS_ANYWHERE: i32 = 1;
    #[cfg(test)]
    const VM_PROT_READ: i32 = 1;
    #[cfg(test)]
    const VM_PROT_WRITE: i32 = 2;

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
    struct WireMetadata {
        magic: [u8; 8],
        version: u16,
        role: u8,
        descriptor_count: u8,
        session_id: [u8; 16],
        generation: u64,
        kinds: [u8; CAPABILITY_COUNT],
        reserved: [u8; 4],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct TransferMessage {
        header: MessageHeader,
        body: MessageBody,
        descriptors: [PortDescriptor; CAPABILITY_COUNT],
        metadata: WireMetadata,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct ReceiveBuffer {
        message: TransferMessage,
        trailer: [u8; 32],
    }

    #[link(name = "System")]
    unsafe extern "C" {
        static mach_task_self_: MachPort;
        fn mach_port_allocate(task: MachPort, right: i32, name: *mut MachPort) -> KernReturn;
        fn mach_port_insert_right(
            task: MachPort,
            name: MachPort,
            poly: MachPort,
            poly_poly: u32,
        ) -> KernReturn;
        fn mach_port_deallocate(task: MachPort, name: MachPort) -> KernReturn;
        fn mach_port_destroy(task: MachPort, name: MachPort) -> KernReturn;
        #[cfg(test)]
        fn mach_vm_allocate(task: MachPort, address: *mut u64, size: u64, flags: i32)
        -> KernReturn;
        #[cfg(test)]
        fn mach_vm_deallocate(task: MachPort, address: u64, size: u64) -> KernReturn;
        #[cfg(test)]
        fn mach_make_memory_entry_64(
            task: MachPort,
            size: *mut u64,
            offset: u64,
            permission: i32,
            object: *mut MachPort,
            parent: MachPort,
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

    pub(super) fn validate_raw(raw: MachPort, operation: &'static str) -> Result<(), ErrorReport> {
        if raw == MACH_PORT_NULL {
            Err(transfer_error(
                ErrorCategory::Resource,
                ErrorCode::RequiredCapabilityMissing,
                operation,
            ))
        } else {
            Ok(())
        }
    }

    pub(super) fn drop_send(raw: MachPort) {
        let _ = unsafe { mach_port_deallocate(mach_task_self_, raw) };
    }

    pub(super) fn drop_receive(raw: MachPort) {
        let _ = unsafe { mach_port_destroy(mach_task_self_, raw) };
    }

    pub(super) fn channel() -> Result<(OwnedMachSendRight, OwnedMachReceiveRight), ErrorReport> {
        let mut control = MACH_PORT_NULL;
        check(
            unsafe {
                mach_port_allocate(mach_task_self_, MACH_PORT_RIGHT_RECEIVE, &raw mut control)
            },
            "allocate Mach control endpoint",
        )?;
        if let Err(error) = check(
            unsafe {
                mach_port_insert_right(mach_task_self_, control, control, MACH_MSG_TYPE_MAKE_SEND)
            },
            "create Mach control send right",
        ) {
            drop_receive(control);
            return Err(error);
        }
        Ok((
            OwnedMachSendRight { raw: control },
            OwnedMachReceiveRight { raw: control },
        ))
    }

    pub(super) fn send(
        control: OwnedMachSendRight,
        manifest: TransferManifest,
        rights: [MachPort; CAPABILITY_COUNT],
    ) -> Result<(), ErrorReport> {
        let control = control.into_raw();
        let mut message = TransferMessage {
            header: MessageHeader {
                bits: MACH_MSGH_BITS_COMPLEX | u32::from(MACH_MSG_TYPE_COPY_SEND),
                size: u32::try_from(size_of::<TransferMessage>())
                    .expect("transfer message size fits Mach header"),
                remote_port: control,
                local_port: MACH_PORT_NULL,
                voucher_port: MACH_PORT_NULL,
                id: TRANSFER_MESSAGE_ID,
            },
            body: MessageBody {
                descriptor_count: u32::try_from(CAPABILITY_COUNT)
                    .expect("capability count fits u32"),
            },
            metadata: encode_manifest(manifest),
            ..TransferMessage::default()
        };
        for (index, raw) in rights.into_iter().enumerate() {
            message.descriptors[index] = PortDescriptor {
                name: raw,
                disposition: if index == CAPABILITY_COUNT - 1 {
                    MACH_MSG_TYPE_MOVE_RECEIVE
                } else {
                    MACH_MSG_TYPE_MOVE_SEND
                },
                descriptor_type: MACH_MSG_PORT_DESCRIPTOR,
                ..PortDescriptor::default()
            };
        }
        let status = unsafe {
            mach_msg(
                &raw mut message.header,
                MACH_SEND_MSG,
                message.header.size,
                0,
                MACH_PORT_NULL,
                0,
                MACH_PORT_NULL,
            )
        };
        drop_send(control);
        if status == KERN_SUCCESS {
            Ok(())
        } else {
            cleanup_descriptors(&mut message);
            Err(transfer_error(
                ErrorCategory::Platform,
                ErrorCode::RequiredCapabilityMissing,
                "send Mach capabilities",
            ))
        }
    }

    pub(super) fn receive(
        control: OwnedMachReceiveRight,
        expected: TransferIdentity,
    ) -> Result<EndpointCapabilities, ErrorReport> {
        let control = control.into_raw();
        let mut buffer = ReceiveBuffer::default();
        let status = unsafe {
            mach_msg(
                &raw mut buffer.message.header,
                MACH_RCV_MSG,
                0,
                u32::try_from(size_of::<ReceiveBuffer>())
                    .expect("receive buffer size fits Mach header"),
                control,
                0,
                MACH_PORT_NULL,
            )
        };
        drop_receive(control);
        if status != KERN_SUCCESS {
            return Err(transfer_error(
                ErrorCategory::Platform,
                ErrorCode::RequiredCapabilityMissing,
                "receive Mach capabilities",
            ));
        }
        let result = validate_message(&buffer.message, expected);
        if let Err(error) = result {
            cleanup_descriptors(&mut buffer.message);
            return Err(error);
        }
        let mut raw = [MACH_PORT_NULL; CAPABILITY_COUNT];
        for (target, descriptor) in raw.iter_mut().zip(&mut buffer.message.descriptors) {
            *target = descriptor.name;
            descriptor.name = MACH_PORT_NULL;
        }
        Ok(EndpointCapabilities {
            manifest: decode_manifest(&buffer.message.metadata)?,
            outbound_memory: Some(OwnedMachSendRight { raw: raw[0] }),
            inbound_memory: Some(OwnedMachSendRight { raw: raw[1] }),
            outbound_signal: Some(OwnedMachSendRight { raw: raw[2] }),
            inbound_signal: Some(OwnedMachReceiveRight { raw: raw[3] }),
        })
    }

    fn encode_manifest(manifest: TransferManifest) -> WireMetadata {
        WireMetadata {
            magic: CONTROL_MAGIC,
            version: CONTROL_VERSION,
            role: manifest.identity.role as u8,
            descriptor_count: CAPABILITY_COUNT_WIRE,
            session_id: manifest.identity.session_id.to_bytes(),
            generation: manifest.identity.generation.get(),
            kinds: manifest.kinds.map(|kind| kind as u8),
            reserved: [0; 4],
        }
    }

    fn decode_manifest(metadata: &WireMetadata) -> Result<TransferManifest, ErrorReport> {
        if metadata.magic != CONTROL_MAGIC
            || metadata.version != CONTROL_VERSION
            || usize::from(metadata.descriptor_count) != CAPABILITY_COUNT
            || metadata.reserved != [0; 4]
        {
            return Err(protocol_error());
        }
        let session_id = SessionId::from_bytes(metadata.session_id).ok_or_else(protocol_error)?;
        let generation = Generation::new(metadata.generation).ok_or_else(|| {
            transfer_error(
                ErrorCategory::Bootstrap,
                ErrorCode::StaleGeneration,
                "validate Mach capability generation",
            )
        })?;
        let role = EndpointRole::from_wire(metadata.role).ok_or_else(protocol_error)?;
        let mut kinds = CANONICAL_KINDS;
        for (kind, value) in kinds.iter_mut().zip(metadata.kinds) {
            *kind = CapabilityKind::from_wire(value).ok_or_else(protocol_error)?;
        }
        Ok(TransferManifest {
            identity: TransferIdentity::new(session_id, generation, role),
            kinds,
        })
    }

    fn validate_message(
        message: &TransferMessage,
        expected: TransferIdentity,
    ) -> Result<(), ErrorReport> {
        if message.header.id != TRANSFER_MESSAGE_ID
            || usize::try_from(message.header.size).ok() != Some(size_of::<TransferMessage>())
            || usize::try_from(message.body.descriptor_count).ok() != Some(CAPABILITY_COUNT)
        {
            return Err(protocol_error());
        }
        for (index, descriptor) in message.descriptors.iter().enumerate() {
            let expected_disposition = if index == CAPABILITY_COUNT - 1 {
                MACH_MSG_TYPE_MOVE_RECEIVE
            } else {
                MACH_MSG_TYPE_MOVE_SEND
            };
            if descriptor.name == MACH_PORT_NULL
                || descriptor.descriptor_type != MACH_MSG_PORT_DESCRIPTOR
                || descriptor.disposition != expected_disposition
            {
                return Err(protocol_error());
            }
        }
        decode_manifest(&message.metadata)?.validate_for(expected)
    }

    fn cleanup_descriptors(message: &mut TransferMessage) {
        for descriptor in message.descriptors.iter_mut().rev() {
            if descriptor.name == MACH_PORT_NULL {
                continue;
            }
            if descriptor.disposition == MACH_MSG_TYPE_MOVE_RECEIVE {
                drop_receive(descriptor.name);
            } else {
                drop_send(descriptor.name);
            }
            descriptor.name = MACH_PORT_NULL;
        }
    }

    fn check(status: KernReturn, operation: &'static str) -> Result<(), ErrorReport> {
        if status == KERN_SUCCESS {
            Ok(())
        } else {
            Err(transfer_error(
                ErrorCategory::Resource,
                ErrorCode::RequiredCapabilityMissing,
                operation,
            ))
        }
    }

    fn protocol_error() -> ErrorReport {
        transfer_error(
            ErrorCategory::Protocol,
            ErrorCode::ProtocolViolation,
            "validate Mach capability message",
        )
    }

    #[cfg(test)]
    pub(super) struct TestResources {
        allocations: [(u64, u64); 2],
        signal_receive: OwnedMachReceiveRight,
    }

    #[cfg(test)]
    impl Drop for TestResources {
        fn drop(&mut self) {
            let _ = &self.signal_receive;
            for (address, size) in self.allocations {
                let _ = unsafe { mach_vm_deallocate(mach_task_self_, address, size) };
            }
        }
    }

    #[cfg(test)]
    pub(super) fn test_capabilities(
        identity: TransferIdentity,
    ) -> (MachCapabilityBundle, TestResources) {
        let mut memory = Vec::new();
        let mut allocations = Vec::new();
        for _ in 0..2 {
            let size = 4096_u64;
            let mut address = 0_u64;
            check(
                unsafe {
                    mach_vm_allocate(mach_task_self_, &raw mut address, size, VM_FLAGS_ANYWHERE)
                },
                "allocate test Mach memory",
            )
            .unwrap();
            let mut entry_size = size;
            let mut entry = MACH_PORT_NULL;
            check(
                unsafe {
                    mach_make_memory_entry_64(
                        mach_task_self_,
                        &raw mut entry_size,
                        address,
                        VM_PROT_READ | VM_PROT_WRITE,
                        &raw mut entry,
                        MACH_PORT_NULL,
                    )
                },
                "create test Mach memory entry",
            )
            .unwrap();
            memory.push(OwnedMachSendRight { raw: entry });
            allocations.push((address, size));
        }
        let (signal_send, signal_receive) = channel().unwrap();
        let (_listener_send, listener) = channel().unwrap();
        (
            MachCapabilityBundle::new(
                identity,
                memory.remove(0),
                memory.remove(0),
                signal_send,
                listener,
            ),
            TestResources {
                allocations: allocations.try_into().unwrap(),
                signal_receive,
            },
        )
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::{
        CAPABILITY_COUNT, EndpointCapabilities, ErrorReport, OwnedMachReceiveRight,
        OwnedMachSendRight, TransferIdentity, TransferManifest,
    };

    pub(super) type MachPort = u32;
    pub(super) const MACH_PORT_NULL: MachPort = 0;

    pub(super) fn validate_raw(_: MachPort, operation: &'static str) -> Result<(), ErrorReport> {
        Err(ErrorReport::unsupported(operation))
    }

    pub(super) fn drop_send(_: MachPort) {}

    pub(super) fn drop_receive(_: MachPort) {}

    pub(super) fn channel() -> Result<(OwnedMachSendRight, OwnedMachReceiveRight), ErrorReport> {
        Err(ErrorReport::unsupported("create Mach control endpoint"))
    }

    pub(super) fn send(
        _: OwnedMachSendRight,
        _: TransferManifest,
        _: [MachPort; CAPABILITY_COUNT],
    ) -> Result<(), ErrorReport> {
        Err(ErrorReport::unsupported("send Mach capabilities"))
    }

    pub(super) fn receive(
        _: OwnedMachReceiveRight,
        _: TransferIdentity,
    ) -> Result<EndpointCapabilities, ErrorReport> {
        Err(ErrorReport::unsupported("receive Mach capabilities"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(role: EndpointRole, generation: u64) -> TransferIdentity {
        TransferIdentity::new(
            SessionId::from_u128(0x1234).unwrap(),
            Generation::new(generation).unwrap(),
            role,
        )
    }

    #[test]
    fn manifest_rejects_wrong_role_session_generation_and_descriptor_order() {
        let expected = identity(EndpointRole::Renderer, 7);
        let manifest = TransferManifest::new(expected);
        assert_eq!(manifest.validate_for(expected), Ok(()));
        assert_eq!(
            manifest
                .validate_for(identity(EndpointRole::Peer, 7))
                .unwrap_err()
                .code(),
            ErrorCode::AuthenticationFailed
        );
        assert_eq!(
            manifest
                .validate_for(identity(EndpointRole::Renderer, 8))
                .unwrap_err()
                .code(),
            ErrorCode::StaleGeneration
        );
        let mut wrong_session = expected;
        wrong_session.session_id = SessionId::from_u128(9).unwrap();
        assert_eq!(
            manifest.validate_for(wrong_session).unwrap_err().code(),
            ErrorCode::AuthenticationFailed
        );
        let malformed = TransferManifest {
            identity: expected,
            kinds: [
                CapabilityKind::OutboundMemory,
                CapabilityKind::InboundMemory,
                CapabilityKind::InboundSignal,
                CapabilityKind::InboundSignal,
            ],
        };
        assert_eq!(
            malformed.validate_for(expected).unwrap_err().code(),
            ErrorCode::ProtocolViolation
        );
    }

    #[test]
    fn metadata_and_native_owners_are_redacted() {
        let identity = identity(EndpointRole::Peer, 3);
        let manifest = TransferManifest::new(identity);
        let debug = format!("{manifest:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("4660"));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn control_channel_is_explicitly_unsupported_off_macos() {
        assert_eq!(
            authenticated_control_channel(identity(EndpointRole::Peer, 1))
                .unwrap_err()
                .code(),
            ErrorCode::Unsupported
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn transfers_exact_capabilities_once_without_bootstrap_lookup() {
        let identity = identity(EndpointRole::Renderer, 11);
        let (mut host, mut endpoint) = authenticated_control_channel(identity).unwrap();
        let (bundle, _live_receivers) = platform::test_capabilities(identity);
        host.transfer(bundle).unwrap();
        let mut received = endpoint.receive().unwrap();
        assert_eq!(received.manifest(), TransferManifest::new(identity));
        assert!(received.take_outbound_memory().is_some());
        assert!(received.take_inbound_memory().is_some());
        assert!(received.take_outbound_signal().is_some());
        assert!(received.take_inbound_signal().is_some());
        assert_eq!(
            endpoint.receive().unwrap_err().code(),
            ErrorCode::ReplayDetected
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn wrong_role_and_stale_generation_fail_before_transfer() {
        for (bundle_identity, code) in [
            (
                identity(EndpointRole::Peer, 5),
                ErrorCode::AuthenticationFailed,
            ),
            (
                identity(EndpointRole::Renderer, 6),
                ErrorCode::StaleGeneration,
            ),
        ] {
            let expected = identity(EndpointRole::Renderer, 5);
            let (mut host, _endpoint) = authenticated_control_channel(expected).unwrap();
            let (bundle, _live_receivers) = platform::test_capabilities(bundle_identity);
            assert_eq!(host.transfer(bundle).unwrap_err().code(), code);
            assert_eq!(
                host.transfer(platform::test_capabilities(expected).0)
                    .unwrap_err()
                    .code(),
                ErrorCode::ReplayDetected
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn failed_native_send_cleans_capabilities_and_closes_one_shot_endpoint() {
        let identity = identity(EndpointRole::Peer, 13);
        let (mut host, endpoint) = authenticated_control_channel(identity).unwrap();
        drop(endpoint);
        let (bundle, _resources) = platform::test_capabilities(identity);
        assert_eq!(
            host.transfer(bundle).unwrap_err().code(),
            ErrorCode::RequiredCapabilityMissing
        );
        assert_eq!(
            host.transfer(platform::test_capabilities(identity).0)
                .unwrap_err()
                .code(),
            ErrorCode::ReplayDetected
        );
    }
}
