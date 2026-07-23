//! Sandbox-authorized, one-shot rendezvous for a Mach capability control endpoint.
//!
//! System `WebKit` serializes injected-bundle parameters as secure-coded bytes and cannot carry a
//! Mach message attachment. This narrow bootstrap publishes a random per-generation service,
//! grants lookup through a sandbox extension, authenticates the canonical endpoint identity and a
//! 256-bit nonce, moves only the control receive right, transfers the four production rights
//! through `nwipc-mach-transfer`, waits for an acknowledgement, and permanently closes.
//!
//! This crate is an experimental feasibility artifact. System `WebKit` does not authorize
//! arbitrary Mach service names in the `WebContent` sandbox, so production transport must not
//! depend on it.

use core::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nwipc_bootstrap_schema::EndpointRole;
use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_mach_transfer::{MachCapabilityBundle, TransferIdentity, authenticated_control_channel};
use nwipc_types::{Generation, SessionId};

const MAGIC: [u8; 8] = *b"NWIPCRV1";
const VERSION: u16 = 1;
const NONCE_LENGTH: usize = 32;
const HEADER_LENGTH: usize = 80;
const MAXIMUM_SERVICE_LENGTH: usize = 127;
const MAXIMUM_EXTENSION_LENGTH: usize = 2048;

/// Property-list-safe metadata needed to consume one rendezvous.
#[derive(Clone, Eq, PartialEq)]
pub struct RendezvousDescriptor {
    identity: TransferIdentity,
    nonce: [u8; NONCE_LENGTH],
    publisher_pid: u32,
    service_name: String,
    sandbox_extension: String,
}

impl RendezvousDescriptor {
    /// Encodes bounded metadata without a task-local port name.
    ///
    /// # Errors
    ///
    /// Rejects metadata that cannot fit the fixed bootstrap limits.
    pub fn encode(&self) -> Result<Vec<u8>, ErrorReport> {
        let service_length = u16::try_from(self.service_name.len())
            .map_err(|_| rendezvous_error(ErrorCode::InvalidRange, "encode rendezvous service"))?;
        let extension_length = u16::try_from(self.sandbox_extension.len()).map_err(|_| {
            rendezvous_error(ErrorCode::InvalidRange, "encode rendezvous extension")
        })?;
        let mut output = Vec::with_capacity(
            HEADER_LENGTH + self.service_name.len() + self.sandbox_extension.len(),
        );
        output.extend_from_slice(&MAGIC);
        output.extend_from_slice(&VERSION.to_le_bytes());
        output.push(self.identity.role() as u8);
        output.push(0);
        output.extend_from_slice(&self.identity.session_id().to_bytes());
        output.extend_from_slice(&self.identity.generation().get().to_le_bytes());
        output.extend_from_slice(&self.nonce);
        output.extend_from_slice(&self.publisher_pid.to_le_bytes());
        output.extend_from_slice(&service_length.to_le_bytes());
        output.extend_from_slice(&extension_length.to_le_bytes());
        output.extend_from_slice(&[0; 4]);
        output.extend_from_slice(self.service_name.as_bytes());
        output.extend_from_slice(self.sandbox_extension.as_bytes());
        Ok(output)
    }

    /// Decodes and validates bounded rendezvous metadata.
    ///
    /// # Errors
    ///
    /// Rejects malformed identity, lengths, service names, extensions, and reserved fields.
    pub fn decode(input: &[u8]) -> Result<Self, ErrorReport> {
        if input.len() < HEADER_LENGTH || input[..8] != MAGIC {
            return Err(rendezvous_error(
                ErrorCode::Truncated,
                "decode Mach rendezvous",
            ));
        }
        let version = u16::from_le_bytes([input[8], input[9]]);
        let role = EndpointRole::from_wire(input[10]).ok_or_else(protocol_error)?;
        if version != VERSION || input[11] != 0 {
            return Err(protocol_error());
        }
        let session_id =
            SessionId::from_bytes(input[12..28].try_into().map_err(|_| protocol_error())?)
                .ok_or_else(protocol_error)?;
        let generation = Generation::new(u64::from_le_bytes(
            input[28..36].try_into().map_err(|_| protocol_error())?,
        ))
        .ok_or_else(|| {
            rendezvous_error(ErrorCode::StaleGeneration, "decode rendezvous generation")
        })?;
        let nonce = input[36..68].try_into().map_err(|_| protocol_error())?;
        let publisher_pid =
            u32::from_le_bytes(input[68..72].try_into().map_err(|_| protocol_error())?);
        let service_length = usize::from(u16::from_le_bytes([input[72], input[73]]));
        let extension_length = usize::from(u16::from_le_bytes([input[74], input[75]]));
        let service_end = HEADER_LENGTH
            .checked_add(service_length)
            .ok_or_else(protocol_error)?;
        let extension_end = service_end
            .checked_add(extension_length)
            .ok_or_else(protocol_error)?;
        if extension_end != input.len()
            || service_length == 0
            || service_length > MAXIMUM_SERVICE_LENGTH
            || extension_length == 0
            || extension_length > MAXIMUM_EXTENSION_LENGTH
            || publisher_pid == 0
            || input[76..80] != [0; 4]
        {
            return Err(protocol_error());
        }
        let service_name = std::str::from_utf8(&input[HEADER_LENGTH..service_end])
            .map_err(|_| protocol_error())?;
        let sandbox_extension =
            std::str::from_utf8(&input[service_end..]).map_err(|_| protocol_error())?;
        if !service_name.starts_with("com.nwipc.rendezvous.v1.")
            || service_name.bytes().any(|byte| byte == 0)
            || sandbox_extension.bytes().any(|byte| byte == 0)
        {
            return Err(protocol_error());
        }
        Ok(Self {
            identity: TransferIdentity::new(session_id, generation, role),
            nonce,
            publisher_pid,
            service_name: service_name.to_owned(),
            sandbox_extension: sandbox_extension.to_owned(),
        })
    }

    /// Identity authenticated by this rendezvous.
    pub const fn identity(&self) -> TransferIdentity {
        self.identity
    }
}

impl fmt::Debug for RendezvousDescriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RendezvousDescriptor")
            .field("identity", &self.identity)
            .field("service_name", &"<redacted>")
            .field("sandbox_extension", &"<redacted>")
            .field("nonce", &"<redacted>")
            .field("publisher_pid", &"<redacted>")
            .finish()
    }
}

/// Host lifetime for one published endpoint rendezvous.
pub struct RendezvousHost {
    _inner: platform::Host,
    completed: Arc<AtomicBool>,
}

impl RendezvousHost {
    /// Whether a valid endpoint acknowledged the complete capability set.
    pub fn completed(&self) -> bool {
        self.completed.load(Ordering::Acquire)
    }
}

impl fmt::Debug for RendezvousHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RendezvousHost")
            .field("native", &"<redacted>")
            .field("completed", &self.completed())
            .finish()
    }
}

/// Publishes one authenticated endpoint and starts the bounded transfer worker.
///
/// Publication finishes before the descriptor is returned, preventing launch-before-publish races.
///
/// # Errors
///
/// Returns typed allocation, publication, sandbox-extension, or worker errors.
pub fn publish(
    identity: TransferIdentity,
    capabilities: MachCapabilityBundle,
) -> Result<(RendezvousHost, RendezvousDescriptor), ErrorReport> {
    let (control_host, control_endpoint) = authenticated_control_channel(identity)?;
    let raw_endpoint = control_endpoint.into_raw_receive_right()?;
    let completed = Arc::new(AtomicBool::new(false));
    let (inner, metadata) = platform::publish(
        identity,
        control_host,
        raw_endpoint,
        capabilities,
        Arc::clone(&completed),
    )?;
    Ok((
        RendezvousHost {
            _inner: inner,
            completed,
        },
        RendezvousDescriptor {
            identity,
            nonce: metadata.nonce,
            service_name: metadata.service_name,
            sandbox_extension: metadata.sandbox_extension,
            publisher_pid: metadata.publisher_pid,
        },
    ))
}

/// Consumes one descriptor, receives the canonical capability set, and acknowledges completion.
///
/// # Errors
///
/// Rejects wrong identity, replay, sandbox denial, lookup failure, malformed control handoff, and
/// capability-transfer validation failure.
pub fn consume(
    descriptor: &RendezvousDescriptor,
    expected: TransferIdentity,
) -> Result<nwipc_mach_transfer::EndpointCapabilities, ErrorReport> {
    if descriptor.identity != expected {
        return Err(
            if descriptor.identity.generation() == expected.generation() {
                rendezvous_error(
                    ErrorCode::AuthenticationFailed,
                    "authenticate rendezvous endpoint",
                )
            } else {
                rendezvous_error(
                    ErrorCode::StaleGeneration,
                    "authenticate rendezvous generation",
                )
            },
        );
    }
    platform::consume(descriptor)
}

struct PublishedMetadata {
    nonce: [u8; NONCE_LENGTH],
    service_name: String,
    sandbox_extension: String,
    publisher_pid: u32,
}

fn rendezvous_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        match code {
            ErrorCode::AuthenticationFailed | ErrorCode::ReplayDetected => ErrorCategory::Security,
            ErrorCode::StaleGeneration => ErrorCategory::Bootstrap,
            ErrorCode::ProtocolViolation | ErrorCode::Truncated => ErrorCategory::Protocol,
            ErrorCode::Timeout => ErrorCategory::Timeout,
            ErrorCode::Closed => ErrorCategory::Closed,
            _ => ErrorCategory::Platform,
        },
        code,
        if matches!(code, ErrorCode::StaleGeneration | ErrorCode::Timeout) {
            Recoverability::ReplaceEndpoint
        } else {
            Recoverability::Terminal
        },
        operation,
    )
}

fn protocol_error() -> ErrorReport {
    rendezvous_error(
        ErrorCode::ProtocolViolation,
        "validate Mach rendezvous metadata",
    )
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{CStr, CString, c_char, c_void};
    use std::fs::File;
    use std::io::Read;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;

    use nwipc_mach_transfer::{
        AuthenticatedControlEndpoint, AuthenticatedControlHost, MachCapabilityBundle,
        TransferIdentity,
    };

    use super::{NONCE_LENGTH, PublishedMetadata, RendezvousDescriptor, rendezvous_error};
    use nwipc_error::{ErrorCode, ErrorReport};

    type MachPort = u32;
    type KernReturn = i32;

    const KERN_SUCCESS: KernReturn = 0;
    const MACH_PORT_NULL: MachPort = 0;
    const MACH_PORT_RIGHT_RECEIVE: i32 = 1;
    const MACH_MSG_TYPE_MOVE_RECEIVE: u8 = 16;
    const MACH_MSG_TYPE_COPY_SEND: u32 = 19;
    const MACH_MSG_TYPE_MAKE_SEND: u32 = 20;
    const MACH_MSG_TYPE_MAKE_SEND_ONCE: u32 = 21;
    const MACH_MSG_TYPE_MOVE_SEND_ONCE: u32 = 18;
    const MACH_MSG_PORT_DESCRIPTOR: u8 = 0;
    const MACH_MSGH_BITS_COMPLEX: u32 = 0x8000_0000;
    const MACH_SEND_MSG: u32 = 1;
    const MACH_RCV_MSG: u32 = 2;
    const MACH_RCV_TIMEOUT: u32 = 0x100;
    const MACH_RCV_TIMED_OUT: KernReturn = 0x1000_4003;
    const CONNECT_MESSAGE: i32 = 0x4e57_5201;
    const CONTROL_MESSAGE: i32 = 0x4e57_5202;
    const ACK_MESSAGE: i32 = 0x4e57_5203;
    const RECEIVE_TIMEOUT_MS: u32 = 30_000;
    const BOOTSTRAP_PER_PID_SERVICE: u64 = 1;

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
    #[derive(Clone, Copy, Default, Eq, PartialEq)]
    struct Authentication {
        session_id: [u8; 16],
        generation: u64,
        role: u8,
        reserved: [u8; 7],
        nonce: [u8; NONCE_LENGTH],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Request {
        header: MessageHeader,
        authentication: Authentication,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RequestBuffer {
        request: Request,
        trailer: [u8; 32],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct ControlResponse {
        header: MessageHeader,
        body: MessageBody,
        control: PortDescriptor,
        authentication: Authentication,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct ControlBuffer {
        response: ControlResponse,
        trailer: [u8; 32],
    }

    #[link(name = "System")]
    unsafe extern "C" {
        static mach_task_self_: MachPort;
        static bootstrap_port: MachPort;
        fn mach_port_allocate(task: MachPort, right: i32, name: *mut MachPort) -> KernReturn;
        fn mach_port_insert_right(
            task: MachPort,
            name: MachPort,
            poly: MachPort,
            poly_poly: u32,
        ) -> KernReturn;
        fn mach_port_deallocate(task: MachPort, name: MachPort) -> KernReturn;
        fn mach_port_destroy(task: MachPort, name: MachPort) -> KernReturn;
        fn bootstrap_register2(
            bootstrap: MachPort,
            service_name: *const c_char,
            service: MachPort,
            flags: u64,
        ) -> KernReturn;
        fn bootstrap_look_up3(
            bootstrap: MachPort,
            service_name: *const c_char,
            service: *mut MachPort,
            target_pid: i32,
            instance_uuid: *const u8,
            flags: u64,
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
        fn sandbox_extension_issue_mach(
            extension_class: *const c_char,
            service_name: *const c_char,
            flags: u64,
        ) -> *mut c_char;
        fn sandbox_extension_consume(token: *const c_char) -> i64;
        fn sandbox_extension_release(handle: i64) -> i32;
        fn free(pointer: *mut c_void);
    }

    pub(super) struct Host {
        service: MachPort,
        worker: Option<JoinHandle<()>>,
    }

    impl Drop for Host {
        fn drop(&mut self) {
            let _ = unsafe { mach_port_destroy(mach_task_self_, self.service) };
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    struct SandboxGrant(i64);

    impl Drop for SandboxGrant {
        fn drop(&mut self) {
            if self.0 > 0 {
                let _ = unsafe { sandbox_extension_release(self.0) };
            }
        }
    }

    struct LookupRight(MachPort);

    impl Drop for LookupRight {
        fn drop(&mut self) {
            let _ = unsafe { mach_port_deallocate(mach_task_self_, self.0) };
        }
    }

    pub(super) fn publish(
        identity: TransferIdentity,
        transfer: AuthenticatedControlHost,
        endpoint: MachPort,
        capabilities: MachCapabilityBundle,
        completed: Arc<AtomicBool>,
    ) -> Result<(Host, PublishedMetadata), ErrorReport> {
        let nonce = random_nonce()?;
        let service_name = random_service_name(identity)?;
        let service_name_c = CString::new(service_name.as_bytes())
            .map_err(|_| rendezvous_error(ErrorCode::ProtocolViolation, "name Mach rendezvous"))?;
        let service = allocate_port("allocate Mach rendezvous")?;
        if unsafe {
            bootstrap_register2(
                bootstrap_port,
                service_name_c.as_ptr(),
                service,
                BOOTSTRAP_PER_PID_SERVICE,
            )
        } != KERN_SUCCESS
        {
            let _ = unsafe { mach_port_destroy(mach_task_self_, service) };
            return Err(rendezvous_error(
                ErrorCode::RequiredCapabilityMissing,
                "publish Mach rendezvous",
            ));
        }
        let extension_class = c"com.apple.webkit.extension.mach";
        let token = unsafe {
            sandbox_extension_issue_mach(extension_class.as_ptr(), service_name_c.as_ptr(), 0)
        };
        if token.is_null() {
            let _ = unsafe { mach_port_destroy(mach_task_self_, service) };
            return Err(rendezvous_error(
                ErrorCode::RequiredCapabilityMissing,
                "issue Mach lookup sandbox extension",
            ));
        }
        let sandbox_extension = unsafe { CStr::from_ptr(token) }
            .to_str()
            .map_err(|_| {
                unsafe { free(token.cast()) };
                rendezvous_error(
                    ErrorCode::ProtocolViolation,
                    "encode Mach lookup sandbox extension",
                )
            })?
            .to_owned();
        unsafe { free(token.cast()) };
        let worker = std::thread::Builder::new()
            .name("nwipc-mach-rendezvous".into())
            .spawn(move || {
                serve(
                    service,
                    identity,
                    nonce,
                    endpoint,
                    transfer,
                    capabilities,
                    &completed,
                );
            })
            .map_err(|_| {
                let _ = unsafe { mach_port_destroy(mach_task_self_, service) };
                rendezvous_error(ErrorCode::Internal, "spawn Mach rendezvous")
            })?;
        Ok((
            Host {
                service,
                worker: Some(worker),
            },
            PublishedMetadata {
                nonce,
                service_name,
                sandbox_extension,
                publisher_pid: std::process::id(),
            },
        ))
    }

    fn serve(
        service: MachPort,
        identity: TransferIdentity,
        nonce: [u8; NONCE_LENGTH],
        endpoint: MachPort,
        mut transfer: AuthenticatedControlHost,
        capabilities: MachCapabilityBundle,
        completed: &AtomicBool,
    ) {
        let expected = authentication(identity, nonce);
        let mut endpoint = endpoint;
        loop {
            let mut request = RequestBuffer::default();
            let status = unsafe {
                mach_msg(
                    &raw mut request.request.header,
                    MACH_RCV_MSG | MACH_RCV_TIMEOUT,
                    0,
                    u32::try_from(size_of::<RequestBuffer>()).unwrap(),
                    service,
                    RECEIVE_TIMEOUT_MS,
                    MACH_PORT_NULL,
                )
            };
            if status != KERN_SUCCESS {
                break;
            }
            if request.request.header.id != CONNECT_MESSAGE
                || request.request.authentication != expected
                || request.request.header.remote_port == MACH_PORT_NULL
            {
                if request.request.header.remote_port != MACH_PORT_NULL {
                    let _ = unsafe {
                        mach_port_deallocate(mach_task_self_, request.request.header.remote_port)
                    };
                }
                continue;
            }
            let mut response = ControlResponse {
                header: MessageHeader {
                    bits: MACH_MSGH_BITS_COMPLEX | MACH_MSG_TYPE_MOVE_SEND_ONCE,
                    size: u32::try_from(size_of::<ControlResponse>()).unwrap(),
                    remote_port: request.request.header.remote_port,
                    id: CONTROL_MESSAGE,
                    ..MessageHeader::default()
                },
                body: MessageBody {
                    descriptor_count: 1,
                },
                control: PortDescriptor {
                    name: endpoint,
                    disposition: MACH_MSG_TYPE_MOVE_RECEIVE,
                    descriptor_type: MACH_MSG_PORT_DESCRIPTOR,
                    ..PortDescriptor::default()
                },
                authentication: expected,
            };
            if unsafe {
                mach_msg(
                    &raw mut response.header,
                    MACH_SEND_MSG,
                    response.header.size,
                    0,
                    MACH_PORT_NULL,
                    0,
                    MACH_PORT_NULL,
                )
            } != KERN_SUCCESS
            {
                break;
            }
            endpoint = MACH_PORT_NULL;
            if transfer.transfer(capabilities).is_err() {
                break;
            }
            let mut acknowledgement = RequestBuffer::default();
            if unsafe {
                mach_msg(
                    &raw mut acknowledgement.request.header,
                    MACH_RCV_MSG | MACH_RCV_TIMEOUT,
                    0,
                    u32::try_from(size_of::<RequestBuffer>()).unwrap(),
                    service,
                    RECEIVE_TIMEOUT_MS,
                    MACH_PORT_NULL,
                )
            } == KERN_SUCCESS
                && acknowledgement.request.header.id == ACK_MESSAGE
                && acknowledgement.request.authentication == expected
            {
                completed.store(true, Ordering::Release);
            }
            break;
        }
        if endpoint != MACH_PORT_NULL {
            let _ = unsafe { mach_port_destroy(mach_task_self_, endpoint) };
        }
    }

    pub(super) fn consume(
        descriptor: &RendezvousDescriptor,
    ) -> Result<nwipc_mach_transfer::EndpointCapabilities, ErrorReport> {
        let extension = CString::new(descriptor.sandbox_extension.as_bytes()).map_err(|_| {
            rendezvous_error(
                ErrorCode::ProtocolViolation,
                "consume Mach lookup sandbox extension",
            )
        })?;
        let grant = SandboxGrant(unsafe { sandbox_extension_consume(extension.as_ptr()) });
        if grant.0 < 0 {
            return Err(rendezvous_error(
                ErrorCode::RequiredCapabilityMissing,
                "consume Mach lookup sandbox extension",
            ));
        }
        let service_name = CString::new(descriptor.service_name.as_bytes()).map_err(|_| {
            rendezvous_error(ErrorCode::ProtocolViolation, "lookup Mach rendezvous")
        })?;
        let service = LookupRight(lookup_service(&service_name, descriptor.publisher_pid)?);
        let authentication = authentication(descriptor.identity, descriptor.nonce);
        let mut endpoint = connect_control(descriptor.identity, service.0, authentication)?;
        let capabilities = endpoint.receive()?;
        let mut acknowledgement = Request {
            header: MessageHeader {
                bits: MACH_MSG_TYPE_COPY_SEND,
                size: u32::try_from(size_of::<Request>()).unwrap(),
                remote_port: service.0,
                id: ACK_MESSAGE,
                ..MessageHeader::default()
            },
            authentication,
        };
        let acknowledged = check(
            unsafe {
                mach_msg(
                    &raw mut acknowledgement.header,
                    MACH_SEND_MSG,
                    acknowledgement.header.size,
                    0,
                    MACH_PORT_NULL,
                    0,
                    MACH_PORT_NULL,
                )
            },
            "acknowledge Mach capabilities",
        );
        acknowledged?;
        drop(grant);
        Ok(capabilities)
    }

    fn connect_control(
        identity: TransferIdentity,
        service: MachPort,
        authentication: Authentication,
    ) -> Result<AuthenticatedControlEndpoint, ErrorReport> {
        let reply = allocate_receive("allocate Mach rendezvous reply")?;
        let mut request = Request {
            header: MessageHeader {
                bits: MACH_MSG_TYPE_COPY_SEND | (MACH_MSG_TYPE_MAKE_SEND_ONCE << 8),
                size: u32::try_from(size_of::<Request>()).unwrap(),
                remote_port: service,
                local_port: reply,
                id: CONNECT_MESSAGE,
                ..MessageHeader::default()
            },
            authentication,
        };
        if let Err(error) = check(
            unsafe {
                mach_msg(
                    &raw mut request.header,
                    MACH_SEND_MSG,
                    request.header.size,
                    0,
                    MACH_PORT_NULL,
                    0,
                    MACH_PORT_NULL,
                )
            },
            "connect Mach rendezvous",
        ) {
            let _ = unsafe { mach_port_destroy(mach_task_self_, reply) };
            return Err(error);
        }
        let mut response = ControlBuffer::default();
        let received = check(
            unsafe {
                mach_msg(
                    &raw mut response.response.header,
                    MACH_RCV_MSG | MACH_RCV_TIMEOUT,
                    0,
                    u32::try_from(size_of::<ControlBuffer>()).unwrap(),
                    reply,
                    RECEIVE_TIMEOUT_MS,
                    MACH_PORT_NULL,
                )
            },
            "receive Mach rendezvous control",
        );
        let _ = unsafe { mach_port_destroy(mach_task_self_, reply) };
        received?;
        if response.response.header.id != CONTROL_MESSAGE
            || response.response.body.descriptor_count != 1
            || response.response.control.descriptor_type != MACH_MSG_PORT_DESCRIPTOR
            || response.response.control.name == MACH_PORT_NULL
            || response.response.authentication != authentication
        {
            if response.response.control.name != MACH_PORT_NULL {
                let _ =
                    unsafe { mach_port_destroy(mach_task_self_, response.response.control.name) };
            }
            return Err(rendezvous_error(
                ErrorCode::ProtocolViolation,
                "validate Mach rendezvous control",
            ));
        }
        unsafe {
            AuthenticatedControlEndpoint::from_raw_receive_right(
                identity,
                response.response.control.name,
            )
        }
    }

    fn authentication(identity: TransferIdentity, nonce: [u8; NONCE_LENGTH]) -> Authentication {
        Authentication {
            session_id: identity.session_id().to_bytes(),
            generation: identity.generation().get(),
            role: identity.role() as u8,
            nonce,
            ..Authentication::default()
        }
    }

    fn random_nonce() -> Result<[u8; NONCE_LENGTH], ErrorReport> {
        let mut nonce = [0; NONCE_LENGTH];
        File::open("/dev/urandom")
            .and_then(|mut source| source.read_exact(&mut nonce))
            .map_err(|_| rendezvous_error(ErrorCode::Internal, "randomize Mach rendezvous"))?;
        Ok(nonce)
    }

    fn random_service_name(identity: TransferIdentity) -> Result<String, ErrorReport> {
        let mut random = [0_u8; 16];
        File::open("/dev/urandom")
            .and_then(|mut source| source.read_exact(&mut random))
            .map_err(|_| rendezvous_error(ErrorCode::Internal, "name Mach rendezvous"))?;
        let suffix = random.iter().fold(String::new(), |mut output, byte| {
            use std::fmt::Write as _;
            write!(output, "{byte:02x}").expect("writing to String cannot fail");
            output
        });
        Ok(format!(
            "com.nwipc.rendezvous.v1.{}.{}.{}",
            identity.generation().get(),
            identity.role() as u8,
            suffix
        ))
    }

    fn allocate_port(operation: &'static str) -> Result<MachPort, ErrorReport> {
        let port = allocate_receive(operation)?;
        if let Err(error) = check(
            unsafe { mach_port_insert_right(mach_task_self_, port, port, MACH_MSG_TYPE_MAKE_SEND) },
            operation,
        ) {
            let _ = unsafe { mach_port_destroy(mach_task_self_, port) };
            return Err(error);
        }
        Ok(port)
    }

    fn lookup_service(service_name: &CString, publisher_pid: u32) -> Result<MachPort, ErrorReport> {
        let target_pid = i32::try_from(publisher_pid).map_err(|_| {
            rendezvous_error(ErrorCode::InvalidRange, "lookup Mach rendezvous publisher")
        })?;
        let instance = [0_u8; 16];
        let mut service = MACH_PORT_NULL;
        let status = unsafe {
            bootstrap_look_up3(
                bootstrap_port,
                service_name.as_ptr(),
                &raw mut service,
                target_pid,
                instance.as_ptr(),
                BOOTSTRAP_PER_PID_SERVICE,
            )
        };
        if status != KERN_SUCCESS {
            return Err(rendezvous_error(
                ErrorCode::RequiredCapabilityMissing,
                if status == 1100 {
                    "lookup Mach rendezvous denied"
                } else if status == 1102 {
                    "lookup Mach rendezvous missing"
                } else {
                    "lookup Mach rendezvous failed"
                },
            ));
        }
        Ok(service)
    }

    fn allocate_receive(operation: &'static str) -> Result<MachPort, ErrorReport> {
        let mut port = MACH_PORT_NULL;
        check(
            unsafe { mach_port_allocate(mach_task_self_, MACH_PORT_RIGHT_RECEIVE, &raw mut port) },
            operation,
        )?;
        Ok(port)
    }

    fn check(status: KernReturn, operation: &'static str) -> Result<(), ErrorReport> {
        if status == KERN_SUCCESS {
            Ok(())
        } else if status == MACH_RCV_TIMED_OUT {
            Err(rendezvous_error(ErrorCode::Timeout, operation))
        } else {
            Err(rendezvous_error(
                ErrorCode::RequiredCapabilityMissing,
                operation,
            ))
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use nwipc_error::ErrorReport;
    use nwipc_mach_transfer::{AuthenticatedControlHost, MachCapabilityBundle, TransferIdentity};

    use super::{PublishedMetadata, RendezvousDescriptor};

    pub(super) struct Host;

    pub(super) fn publish(
        _: TransferIdentity,
        _: AuthenticatedControlHost,
        _: u32,
        _: MachCapabilityBundle,
        _: Arc<AtomicBool>,
    ) -> Result<(Host, PublishedMetadata), ErrorReport> {
        Err(ErrorReport::unsupported("publish Mach rendezvous"))
    }

    pub(super) fn consume(
        _: &RendezvousDescriptor,
    ) -> Result<nwipc_mach_transfer::EndpointCapabilities, ErrorReport> {
        Err(ErrorReport::unsupported("consume Mach rendezvous"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_rejects_truncation_and_redacts_secrets() {
        assert!(RendezvousDescriptor::decode(b"NWIPCRV1").is_err());
        let descriptor = RendezvousDescriptor {
            identity: TransferIdentity::new(
                SessionId::from_u128(1).unwrap(),
                Generation::new(2).unwrap(),
                EndpointRole::Renderer,
            ),
            nonce: [7; NONCE_LENGTH],
            publisher_pid: 1,
            service_name: "com.nwipc.rendezvous.v1.test".into(),
            sandbox_extension: "extension-secret".into(),
        };
        let decoded = RendezvousDescriptor::decode(&descriptor.encode().unwrap()).unwrap();
        assert_eq!(decoded, descriptor);
        let debug = format!("{descriptor:?}");
        assert!(!debug.contains("extension-secret"));
        assert!(!debug.contains("0707"));
    }

    #[test]
    fn consume_rejects_wrong_identity_before_native_lookup() {
        let descriptor = RendezvousDescriptor {
            identity: TransferIdentity::new(
                SessionId::from_u128(1).unwrap(),
                Generation::new(2).unwrap(),
                EndpointRole::Renderer,
            ),
            nonce: [7; NONCE_LENGTH],
            publisher_pid: 1,
            service_name: "com.nwipc.rendezvous.v1.test".into(),
            sandbox_extension: "extension-secret".into(),
        };
        let wrong_role = TransferIdentity::new(
            descriptor.identity().session_id(),
            descriptor.identity().generation(),
            EndpointRole::Peer,
        );
        assert_eq!(
            consume(&descriptor, wrong_role).unwrap_err().code(),
            ErrorCode::AuthenticationFailed
        );
    }
}
