//! Mach port notification-hint provider.
//!
//! A host-owned broker transfers send and receive rights through Mach messages. Descriptors carry
//! an opaque bootstrap rendezvous name rather than a task-local numeric port name.

use std::fmt;
use std::time::Duration;

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_mach_transfer::{OwnedMachReceiveRight, OwnedMachSendRight};
use nwipc_signal_api::{SignalDirection, SignalListener, SignalSender, WaitOutcome};
use nwipc_types::{Generation, SessionId};

const PREFIX: &str = "com.nwipc.signal-mach.v1";
const MAXIMUM_NAME_LENGTH: usize = 127;

/// Transferable Mach signal rendezvous descriptor.
#[derive(Clone, Eq, PartialEq)]
pub struct MachSignalDescriptor {
    service_name: String,
    generation: Generation,
}

impl MachSignalDescriptor {
    /// Encodes generation and the broker rendezvous name.
    pub fn encode(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(8 + self.service_name.len());
        output.extend_from_slice(&self.generation.get().to_le_bytes());
        output.extend_from_slice(self.service_name.as_bytes());
        output
    }

    /// Decodes bounded descriptor metadata.
    ///
    /// # Errors
    ///
    /// Rejects malformed names and zero generations.
    pub fn decode(input: &[u8]) -> Result<Self, ErrorReport> {
        let (generation, name) = input
            .split_at_checked(8)
            .ok_or_else(|| signal_error(ErrorCode::Truncated, "decode Mach signal"))?;
        let generation =
            Generation::new(u64::from_le_bytes(generation.try_into().map_err(|_| {
                signal_error(ErrorCode::Truncated, "decode Mach signal")
            })?))
            .ok_or_else(|| signal_error(ErrorCode::StaleGeneration, "decode Mach signal"))?;
        let service_name = std::str::from_utf8(name)
            .map_err(|_| signal_error(ErrorCode::ProtocolViolation, "decode Mach signal"))?;
        if service_name.len() > MAXIMUM_NAME_LENGTH
            || !service_name.starts_with(PREFIX)
            || service_name.bytes().any(|byte| byte == 0)
        {
            return Err(signal_error(
                ErrorCode::ProtocolViolation,
                "decode Mach signal",
            ));
        }
        Ok(Self {
            service_name: service_name.to_owned(),
            generation,
        })
    }

    /// Resource generation bound to the descriptor.
    pub const fn generation(&self) -> Generation {
        self.generation
    }
}

impl fmt::Debug for MachSignalDescriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MachSignalDescriptor")
            .field("service_name", &"<redacted>")
            .field("generation", &self.generation)
            .finish()
    }
}

/// Mach signal provider and endpoint factory.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MachSignal;

/// Non-secret Mach signal characteristics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MachSignalDiagnostics {
    /// Repeated hints may be coalesced by a full port queue.
    pub coalescing: bool,
    /// Send and receive rights cross a task boundary as capabilities.
    pub capability_transfer: bool,
    /// Process-boundary delivery is supported.
    pub cross_process: bool,
}

impl MachSignal {
    /// Initializes Mach messaging on macOS.
    ///
    /// # Errors
    ///
    /// Returns explicit `Unsupported` on other platforms.
    pub fn initialize() -> Result<Self, ErrorReport> {
        if cfg!(target_os = "macos") {
            Ok(Self)
        } else {
            Err(ErrorReport::unsupported("initialize Mach signal"))
        }
    }

    /// Returns redacted provider capabilities.
    pub const fn diagnostics(self) -> MachSignalDiagnostics {
        MachSignalDiagnostics {
            coalescing: true,
            capability_transfer: cfg!(target_os = "macos"),
            cross_process: cfg!(target_os = "macos"),
        }
    }

    /// Creates a directional port and a broker which owns it until endpoint attachment.
    ///
    /// # Errors
    ///
    /// Returns a typed allocation, registration, or worker failure.
    pub fn create(
        self,
        session_id: SessionId,
        generation: Generation,
        direction: SignalDirection,
    ) -> Result<(MachSignalResource, MachSignalDescriptor), ErrorReport> {
        platform::create(session_id, generation, direction)
    }

    /// Creates a directional port whose rights are handed off by an external authenticated
    /// capability transfer.
    ///
    /// # Errors
    ///
    /// Returns a typed native port allocation failure.
    pub fn create_transfer_resource(self) -> Result<MachTransferSignalResource, ErrorReport> {
        platform::create_transfer_resource().map(|inner| MachTransferSignalResource { inner })
    }

    /// Opens a sender after validating the active generation.
    ///
    /// # Errors
    ///
    /// Rejects stale generations and unavailable brokers.
    pub fn sender(
        self,
        descriptor: &MachSignalDescriptor,
        expected_generation: Generation,
    ) -> Result<MachSender, ErrorReport> {
        validate_generation(descriptor, expected_generation)?;
        platform::sender(descriptor)
    }

    /// Moves the directional receive right into this task.
    ///
    /// Exactly one listener may attach to a descriptor.
    ///
    /// # Errors
    ///
    /// Rejects stale generations, duplicate attachment, and unavailable brokers.
    pub fn listener(
        self,
        descriptor: &MachSignalDescriptor,
        expected_generation: Generation,
    ) -> Result<MachListener, ErrorReport> {
        validate_generation(descriptor, expected_generation)?;
        platform::listener(descriptor)
    }
}

/// Host ownership keeping a capability-transfer broker alive.
pub struct MachSignalResource {
    _inner: platform::Resource,
}

/// Host-owned directional signal port awaiting endpoint capability transfer.
pub struct MachTransferSignalResource {
    inner: platform::TransferResource,
}

impl MachTransferSignalResource {
    /// Duplicates one outbound send-right reference.
    ///
    /// The returned task-local name must be adopted immediately and never serialized or logged.
    ///
    /// # Errors
    ///
    /// Returns a typed native right-duplication failure.
    pub fn duplicate_sender_right(&self) -> Result<OwnedMachSendRight, ErrorReport> {
        let raw = platform::duplicate_transfer_sender(&self.inner)?;
        unsafe { OwnedMachSendRight::from_raw(raw) }
    }

    /// Moves the sole receive right out for the endpoint listener.
    ///
    /// The returned task-local name must be adopted immediately and never serialized or logged.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the receive right was already moved.
    pub fn take_listener_right(&mut self) -> Result<OwnedMachReceiveRight, ErrorReport> {
        let raw = platform::take_transfer_listener(&mut self.inner)?;
        unsafe { OwnedMachReceiveRight::from_raw(raw) }
    }
}

impl fmt::Debug for MachTransferSignalResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MachTransferSignalResource(<redacted>)")
    }
}

impl fmt::Debug for MachSignalResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MachSignalResource")
            .field("native", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// Owned Mach send right.
pub struct MachSender(platform::SendRight);

impl MachSender {
    /// Adopts one send-right reference transferred into the current task.
    ///
    /// Ownership is represented by `OwnedMachSendRight` and is consumed on both success and
    /// failure.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the transferred right cannot be adopted.
    pub fn from_transferred_send_right(right: OwnedMachSendRight) -> Result<Self, ErrorReport> {
        unsafe { platform::sender_from_raw(right.into_raw()) }.map(Self)
    }
}

impl fmt::Debug for MachSender {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MachSender(<redacted>)")
    }
}

impl Clone for MachSender {
    fn clone(&self) -> Self {
        Self(platform::clone_sender(&self.0))
    }
}

impl SignalSender for MachSender {
    fn notify(&self) -> Result<(), ErrorReport> {
        platform::notify(&self.0)
    }
}

/// Owned Mach receive right.
pub struct MachListener {
    inner: Option<platform::ReceiveRight>,
}

impl MachListener {
    /// Adopts the uniquely owned receive right transferred into the current task.
    ///
    /// Unique ownership is represented by `OwnedMachReceiveRight` and is consumed on both success
    /// and failure.
    ///
    /// # Errors
    ///
    /// Returns a typed error when listener setup or no-senders notification registration fails.
    pub fn from_transferred_receive_right(
        right: OwnedMachReceiveRight,
    ) -> Result<Self, ErrorReport> {
        unsafe { platform::listener_from_raw(right.into_raw()) }
            .map(|inner| Self { inner: Some(inner) })
    }
}

impl fmt::Debug for MachListener {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MachListener")
            .field("native", &"<redacted>")
            .field("cancelled", &self.inner.is_none())
            .finish()
    }
}

impl SignalListener for MachListener {
    fn try_wait(&mut self) -> Result<WaitOutcome, ErrorReport> {
        let Some(inner) = &self.inner else {
            return Ok(WaitOutcome::Cancelled);
        };
        platform::wait(inner, Duration::ZERO)
    }

    fn wait_timeout(&mut self, timeout: Duration) -> Result<WaitOutcome, ErrorReport> {
        let Some(inner) = &self.inner else {
            return Ok(WaitOutcome::Cancelled);
        };
        platform::wait(inner, timeout)
    }

    fn cancel(&mut self) {
        self.inner.take();
    }
}

impl Drop for MachListener {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn validate_generation(
    descriptor: &MachSignalDescriptor,
    expected: Generation,
) -> Result<(), ErrorReport> {
    if descriptor.generation != expected {
        return Err(signal_error(
            ErrorCode::StaleGeneration,
            "open Mach signal endpoint",
        ));
    }
    Ok(())
}

fn signal_error(code: ErrorCode, operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Signal,
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
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread::JoinHandle;

    use super::{
        Duration, ErrorCode, ErrorReport, Generation, MachListener, MachSender,
        MachSignalDescriptor, MachSignalResource, PREFIX, SessionId, SignalDirection, WaitOutcome,
        signal_error,
    };

    type MachPort = u32;
    type KernReturn = i32;

    const KERN_SUCCESS: KernReturn = 0;
    const MACH_PORT_NULL: MachPort = 0;
    const MACH_PORT_RIGHT_RECEIVE: i32 = 1;
    const MACH_PORT_RIGHT_SEND: i32 = 0;
    const MACH_MSG_TYPE_MOVE_RECEIVE: u8 = 16;
    const MACH_MSG_TYPE_MOVE_SEND_ONCE: u8 = 18;
    const MACH_MSG_TYPE_COPY_SEND: u8 = 19;
    const MACH_MSG_TYPE_MAKE_SEND: u32 = 20;
    const MACH_MSG_TYPE_MAKE_SEND_ONCE: u32 = 21;
    const MACH_MSG_PORT_DESCRIPTOR: u8 = 0;
    const MACH_MSGH_BITS_COMPLEX: u32 = 0x8000_0000;
    const MACH_SEND_MSG: u32 = 1;
    const MACH_RCV_MSG: u32 = 2;
    const MACH_SEND_TIMEOUT: u32 = 0x10;
    const MACH_RCV_TIMEOUT: u32 = 0x100;
    const MACH_SEND_TIMED_OUT: KernReturn = 0x1000_0004;
    const MACH_RCV_TIMED_OUT: KernReturn = 0x1000_4003;
    const REQUEST_SENDER: i32 = 1;
    const REQUEST_LISTENER: i32 = 2;
    const SIGNAL_MESSAGE: i32 = 3;
    const MACH_NOTIFY_NO_SENDERS: i32 = 70;

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
    struct HeaderBuffer {
        header: MessageHeader,
        trailer: [u8; 32],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct PortBuffer {
        message: PortMessage,
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
        fn mach_port_mod_refs(task: MachPort, name: MachPort, right: i32, delta: i32)
        -> KernReturn;
        fn mach_port_request_notification(
            task: MachPort,
            name: MachPort,
            message_id: i32,
            sync: u32,
            notify: MachPort,
            notify_type: u32,
            previous: *mut MachPort,
        ) -> KernReturn;
        fn mach_port_deallocate(task: MachPort, name: MachPort) -> KernReturn;
        fn mach_port_destroy(task: MachPort, name: MachPort) -> KernReturn;
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

    pub(super) struct Resource {
        control: MachPort,
        worker: Option<JoinHandle<()>>,
    }

    pub(super) struct TransferResource {
        port: MachPort,
        owns_receive: bool,
    }

    impl Drop for TransferResource {
        fn drop(&mut self) {
            let task = unsafe { mach_task_self_ };
            if self.owns_receive {
                let _ = unsafe { mach_port_destroy(task, self.port) };
            } else {
                let _ = unsafe { mach_port_deallocate(task, self.port) };
            }
        }
    }

    impl Drop for Resource {
        fn drop(&mut self) {
            let task = unsafe { mach_task_self_ };
            let _ = unsafe { mach_port_destroy(task, self.control) };
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    pub(super) struct SendRight(MachPort);

    unsafe impl Send for SendRight {}
    unsafe impl Sync for SendRight {}

    impl Drop for SendRight {
        fn drop(&mut self) {
            let task = unsafe { mach_task_self_ };
            let _ = unsafe { mach_port_deallocate(task, self.0) };
        }
    }

    pub(super) struct ReceiveRight(MachPort);

    unsafe impl Send for ReceiveRight {}

    impl Drop for ReceiveRight {
        fn drop(&mut self) {
            let task = unsafe { mach_task_self_ };
            let _ = unsafe { mach_port_destroy(task, self.0) };
        }
    }

    pub(super) fn create(
        session_id: SessionId,
        generation: Generation,
        direction: SignalDirection,
    ) -> Result<(MachSignalResource, MachSignalDescriptor), ErrorReport> {
        let nonce = service_nonce()?;
        let task = unsafe { mach_task_self_ };
        let signal = allocate_port("allocate Mach signal port")?;
        let control = match allocate_port("allocate Mach signal broker") {
            Ok(port) => port,
            Err(error) => {
                let _ = unsafe { mach_port_destroy(task, signal) };
                return Err(error);
            }
        };
        let sequence = NEXT_SERVICE.fetch_add(1, Ordering::Relaxed);
        let session = session_id.to_bytes();
        let service_name = format!(
            "{PREFIX}.{:02x}{:02x}{:02x}{:02x}.{}.{}.{}.{nonce}",
            session[0],
            session[1],
            session[2],
            session[3],
            generation.get(),
            direction.suffix(),
            sequence
        );
        let name = CString::new(service_name.as_bytes())
            .map_err(|_| signal_error(ErrorCode::ProtocolViolation, "name Mach signal broker"))?;
        let bootstrap = unsafe { bootstrap_port };
        if let Err(error) = check(
            unsafe { bootstrap_register(bootstrap, name.as_ptr(), control) },
            "register Mach signal broker",
        ) {
            let _ = unsafe { mach_port_destroy(task, control) };
            let _ = unsafe { mach_port_destroy(task, signal) };
            return Err(error);
        }
        let worker = std::thread::Builder::new()
            .name("nwipc-mach-signal".into())
            .spawn(move || serve(control, signal));
        let Ok(worker) = worker else {
            let _ = unsafe { mach_port_destroy(task, control) };
            let _ = unsafe { mach_port_destroy(task, signal) };
            return Err(signal_error(
                ErrorCode::Internal,
                "spawn Mach signal broker",
            ));
        };
        Ok((
            MachSignalResource {
                _inner: Resource {
                    control,
                    worker: Some(worker),
                },
            },
            MachSignalDescriptor {
                service_name,
                generation,
            },
        ))
    }

    pub(super) fn create_transfer_resource() -> Result<TransferResource, ErrorReport> {
        allocate_port("allocate transferable Mach signal").map(|port| TransferResource {
            port,
            owns_receive: true,
        })
    }

    pub(super) fn duplicate_transfer_sender(
        resource: &TransferResource,
    ) -> Result<MachPort, ErrorReport> {
        check(
            unsafe { mach_port_mod_refs(mach_task_self_, resource.port, MACH_PORT_RIGHT_SEND, 1) },
            "duplicate transferable Mach sender",
        )?;
        Ok(resource.port)
    }

    pub(super) fn take_transfer_listener(
        resource: &mut TransferResource,
    ) -> Result<MachPort, ErrorReport> {
        if !resource.owns_receive {
            return Err(signal_error(
                ErrorCode::RequiredCapabilityMissing,
                "move transferable Mach listener",
            ));
        }
        resource.owns_receive = false;
        Ok(resource.port)
    }

    pub(super) unsafe fn sender_from_raw(raw: MachPort) -> Result<SendRight, ErrorReport> {
        if raw == MACH_PORT_NULL {
            return Err(signal_error(
                ErrorCode::RequiredCapabilityMissing,
                "adopt transferred Mach sender",
            ));
        }
        Ok(SendRight(raw))
    }

    pub(super) unsafe fn listener_from_raw(raw: MachPort) -> Result<ReceiveRight, ErrorReport> {
        if raw == MACH_PORT_NULL {
            return Err(signal_error(
                ErrorCode::RequiredCapabilityMissing,
                "adopt transferred Mach listener",
            ));
        }
        let task = unsafe { mach_task_self_ };
        let mut previous = MACH_PORT_NULL;
        if let Err(error) = check(
            unsafe {
                mach_port_request_notification(
                    task,
                    raw,
                    MACH_NOTIFY_NO_SENDERS,
                    1,
                    raw,
                    MACH_MSG_TYPE_MAKE_SEND_ONCE,
                    &raw mut previous,
                )
            },
            "arm transferred Mach no-senders notification",
        ) {
            let _ = unsafe { mach_port_destroy(task, raw) };
            return Err(error);
        }
        if previous != MACH_PORT_NULL {
            let _ = unsafe { mach_port_deallocate(task, previous) };
        }
        Ok(ReceiveRight(raw))
    }

    pub(super) fn sender(descriptor: &MachSignalDescriptor) -> Result<MachSender, ErrorReport> {
        request_right(&descriptor.service_name, REQUEST_SENDER)
            .map(|port| MachSender(SendRight(port)))
    }

    pub(super) fn listener(descriptor: &MachSignalDescriptor) -> Result<MachListener, ErrorReport> {
        let port = request_right(&descriptor.service_name, REQUEST_LISTENER)?;
        let task = unsafe { mach_task_self_ };
        let mut previous = MACH_PORT_NULL;
        if let Err(error) = check(
            unsafe {
                mach_port_request_notification(
                    task,
                    port,
                    MACH_NOTIFY_NO_SENDERS,
                    1,
                    port,
                    MACH_MSG_TYPE_MAKE_SEND_ONCE,
                    &raw mut previous,
                )
            },
            "arm Mach no-senders notification",
        ) {
            let _ = unsafe { mach_port_destroy(task, port) };
            return Err(error);
        }
        if previous != MACH_PORT_NULL {
            let _ = unsafe { mach_port_deallocate(task, previous) };
        }
        Ok(MachListener {
            inner: Some(ReceiveRight(port)),
        })
    }

    pub(super) fn clone_sender(sender: &SendRight) -> SendRight {
        let task = unsafe { mach_task_self_ };
        let status = unsafe { mach_port_mod_refs(task, sender.0, MACH_PORT_RIGHT_SEND, 1) };
        assert_eq!(
            status, KERN_SUCCESS,
            "cloning a live Mach send right failed"
        );
        SendRight(sender.0)
    }

    pub(super) fn notify(sender: &SendRight) -> Result<(), ErrorReport> {
        let mut message = MessageHeader {
            bits: u32::from(MACH_MSG_TYPE_COPY_SEND),
            size: u32::try_from(size_of::<MessageHeader>()).unwrap(),
            remote_port: sender.0,
            local_port: MACH_PORT_NULL,
            voucher_port: MACH_PORT_NULL,
            id: SIGNAL_MESSAGE,
        };
        let status = unsafe {
            mach_msg(
                &raw mut message,
                MACH_SEND_MSG | MACH_SEND_TIMEOUT,
                message.size,
                0,
                MACH_PORT_NULL,
                0,
                MACH_PORT_NULL,
            )
        };
        if status == KERN_SUCCESS || status == MACH_SEND_TIMED_OUT {
            Ok(())
        } else {
            Err(signal_error(ErrorCode::Closed, "post Mach signal"))
        }
    }

    pub(super) fn wait(
        listener: &ReceiveRight,
        timeout: Duration,
    ) -> Result<WaitOutcome, ErrorReport> {
        let timeout_ms = if timeout.is_zero() {
            0
        } else {
            u32::try_from(timeout.as_millis().clamp(1, u128::from(u32::MAX))).unwrap()
        };
        let mut buffer = HeaderBuffer::default();
        let status = unsafe {
            mach_msg(
                &raw mut buffer.header,
                MACH_RCV_MSG | MACH_RCV_TIMEOUT,
                0,
                u32::try_from(size_of::<HeaderBuffer>()).unwrap(),
                listener.0,
                timeout_ms,
                MACH_PORT_NULL,
            )
        };
        if status == MACH_RCV_TIMED_OUT {
            return Ok(WaitOutcome::TimedOut);
        }
        check(status, "receive Mach signal")?;
        if buffer.header.id == MACH_NOTIFY_NO_SENDERS {
            return Ok(WaitOutcome::Cancelled);
        }
        loop {
            let mut duplicate = HeaderBuffer::default();
            let status = unsafe {
                mach_msg(
                    &raw mut duplicate.header,
                    MACH_RCV_MSG | MACH_RCV_TIMEOUT,
                    0,
                    u32::try_from(size_of::<HeaderBuffer>()).unwrap(),
                    listener.0,
                    0,
                    MACH_PORT_NULL,
                )
            };
            if status == MACH_RCV_TIMED_OUT {
                break;
            }
            check(status, "coalesce Mach signal")?;
        }
        Ok(WaitOutcome::Signaled)
    }

    fn allocate_port(operation: &'static str) -> Result<MachPort, ErrorReport> {
        let task = unsafe { mach_task_self_ };
        let mut port = MACH_PORT_NULL;
        check(
            unsafe { mach_port_allocate(task, MACH_PORT_RIGHT_RECEIVE, &raw mut port) },
            operation,
        )?;
        if let Err(error) = check(
            unsafe { mach_port_insert_right(task, port, port, MACH_MSG_TYPE_MAKE_SEND) },
            operation,
        ) {
            let _ = unsafe { mach_port_destroy(task, port) };
            return Err(error);
        }
        Ok(port)
    }

    fn serve(control: MachPort, signal: MachPort) {
        let mut listener_available = true;
        loop {
            let mut request = HeaderBuffer::default();
            if unsafe {
                mach_msg(
                    &raw mut request.header,
                    MACH_RCV_MSG,
                    0,
                    u32::try_from(size_of::<HeaderBuffer>()).unwrap(),
                    control,
                    0,
                    MACH_PORT_NULL,
                )
            } != KERN_SUCCESS
            {
                break;
            }
            let disposition = match request.header.id {
                REQUEST_SENDER => MACH_MSG_TYPE_COPY_SEND,
                REQUEST_LISTENER if listener_available => {
                    listener_available = false;
                    MACH_MSG_TYPE_MOVE_RECEIVE
                }
                _ => {
                    let task = unsafe { mach_task_self_ };
                    let _ = unsafe { mach_port_deallocate(task, request.header.remote_port) };
                    continue;
                }
            };
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
                    name: signal,
                    disposition,
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
        if listener_available {
            let _ = unsafe { mach_port_destroy(task, signal) };
        } else {
            let _ = unsafe { mach_port_deallocate(task, signal) };
        }
    }

    fn request_right(service_name: &str, request_id: i32) -> Result<MachPort, ErrorReport> {
        let name = CString::new(service_name.as_bytes())
            .map_err(|_| signal_error(ErrorCode::ProtocolViolation, "lookup Mach signal broker"))?;
        let task = unsafe { mach_task_self_ };
        let mut service = MACH_PORT_NULL;
        let bootstrap = unsafe { bootstrap_port };
        check(
            unsafe { bootstrap_look_up(bootstrap, name.as_ptr(), &raw mut service) },
            "lookup Mach signal broker",
        )?;
        let mut reply = MACH_PORT_NULL;
        check(
            unsafe { mach_port_allocate(task, MACH_PORT_RIGHT_RECEIVE, &raw mut reply) },
            "allocate Mach signal reply",
        )?;
        let mut request = MessageHeader {
            bits: u32::from(MACH_MSG_TYPE_COPY_SEND) | (MACH_MSG_TYPE_MAKE_SEND_ONCE << 8),
            size: u32::try_from(size_of::<MessageHeader>()).unwrap(),
            remote_port: service,
            local_port: reply,
            voucher_port: MACH_PORT_NULL,
            id: request_id,
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
            "request Mach signal right",
        );
        let _ = unsafe { mach_port_deallocate(task, service) };
        if let Err(error) = sent {
            let _ = unsafe { mach_port_destroy(task, reply) };
            return Err(error);
        }
        let mut response = PortBuffer::default();
        let received = check(
            unsafe {
                mach_msg(
                    &raw mut response.message.header,
                    MACH_RCV_MSG,
                    0,
                    u32::try_from(size_of::<PortBuffer>()).unwrap(),
                    reply,
                    0,
                    MACH_PORT_NULL,
                )
            },
            "receive Mach signal right",
        );
        let _ = unsafe { mach_port_destroy(task, reply) };
        received?;
        if response.message.body.descriptor_count != 1
            || response.message.port.descriptor_type != MACH_MSG_PORT_DESCRIPTOR
            || response.message.port.name == MACH_PORT_NULL
        {
            return Err(signal_error(
                ErrorCode::ProtocolViolation,
                "validate Mach signal right",
            ));
        }
        Ok(response.message.port.name)
    }

    fn check(status: KernReturn, operation: &'static str) -> Result<(), ErrorReport> {
        if status == KERN_SUCCESS {
            Ok(())
        } else {
            Err(signal_error(
                ErrorCode::RequiredCapabilityMissing,
                operation,
            ))
        }
    }

    fn service_nonce() -> Result<String, ErrorReport> {
        let mut bytes = [0_u8; 16];
        File::open("/dev/urandom")
            .and_then(|mut source| source.read_exact(&mut bytes))
            .map_err(|_| signal_error(ErrorCode::Internal, "randomize Mach signal broker"))?;
        Ok(bytes.iter().fold(String::new(), |mut output, byte| {
            use std::fmt::Write as _;
            write!(output, "{byte:02x}").expect("writing to String cannot fail");
            output
        }))
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::{
        Duration, ErrorReport, Generation, MachListener, MachSender, MachSignalDescriptor,
        MachSignalResource, SessionId, SignalDirection, WaitOutcome,
    };

    pub(super) struct Resource;
    pub(super) struct TransferResource;
    pub(super) struct SendRight;
    pub(super) struct ReceiveRight;

    pub(super) fn create(
        _: SessionId,
        _: Generation,
        _: SignalDirection,
    ) -> Result<(MachSignalResource, MachSignalDescriptor), ErrorReport> {
        Err(ErrorReport::unsupported("create Mach signal"))
    }

    pub(super) fn sender(_: &MachSignalDescriptor) -> Result<MachSender, ErrorReport> {
        Err(ErrorReport::unsupported("open Mach sender"))
    }

    pub(super) fn listener(_: &MachSignalDescriptor) -> Result<MachListener, ErrorReport> {
        Err(ErrorReport::unsupported("open Mach listener"))
    }

    pub(super) fn create_transfer_resource() -> Result<TransferResource, ErrorReport> {
        Err(ErrorReport::unsupported("create transferable Mach signal"))
    }

    pub(super) fn duplicate_transfer_sender(_: &TransferResource) -> Result<u32, ErrorReport> {
        Err(ErrorReport::unsupported(
            "duplicate transferable Mach sender",
        ))
    }

    pub(super) fn take_transfer_listener(_: &mut TransferResource) -> Result<u32, ErrorReport> {
        Err(ErrorReport::unsupported("move transferable Mach listener"))
    }

    pub(super) unsafe fn sender_from_raw(_: u32) -> Result<SendRight, ErrorReport> {
        Err(ErrorReport::unsupported("adopt transferred Mach sender"))
    }

    pub(super) unsafe fn listener_from_raw(_: u32) -> Result<ReceiveRight, ErrorReport> {
        Err(ErrorReport::unsupported("adopt transferred Mach listener"))
    }

    pub(super) fn clone_sender(_: &SendRight) -> SendRight {
        SendRight
    }

    pub(super) fn notify(_: &SendRight) -> Result<(), ErrorReport> {
        Err(ErrorReport::unsupported("post Mach signal"))
    }

    pub(super) fn wait(_: &ReceiveRight, _: Duration) -> Result<WaitOutcome, ErrorReport> {
        Err(ErrorReport::unsupported("wait Mach signal"))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use std::process::Command;

    use super::*;

    fn identity() -> (SessionId, Generation) {
        (
            SessionId::from_u128(9).unwrap(),
            Generation::new(2).unwrap(),
        )
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn same_process_delivery_coalescing_and_cancellation() {
        let (session, generation) = identity();
        let provider = MachSignal::initialize().unwrap();
        let (_resource, descriptor) = provider
            .create(session, generation, SignalDirection::RendererToPeer)
            .unwrap();
        let sender = provider.sender(&descriptor, generation).unwrap();
        let mut listener = provider.listener(&descriptor, generation).unwrap();
        assert_eq!(
            provider
                .listener(&descriptor, generation)
                .unwrap_err()
                .code(),
            ErrorCode::ProtocolViolation
        );
        assert_eq!(
            provider
                .sender(&descriptor, Generation::new(3).unwrap())
                .unwrap_err()
                .code(),
            ErrorCode::StaleGeneration
        );
        sender.notify().unwrap();
        sender.notify().unwrap();
        assert_eq!(
            listener.wait_timeout(Duration::from_millis(50)).unwrap(),
            WaitOutcome::Signaled
        );
        assert_eq!(listener.try_wait().unwrap(), WaitOutcome::TimedOut);
        listener.cancel();
        assert_eq!(listener.try_wait().unwrap(), WaitOutcome::Cancelled);
        assert_eq!(sender.notify().unwrap_err().code(), ErrorCode::Closed);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn broker_exit_delivers_no_senders_notification() {
        let (session, generation) = identity();
        let provider = MachSignal::initialize().unwrap();
        let (resource, descriptor) = provider
            .create(session, generation, SignalDirection::RendererToPeer)
            .unwrap();
        let mut listener = provider.listener(&descriptor, generation).unwrap();
        drop(resource);
        assert_eq!(
            listener.wait_timeout(Duration::from_secs(1)).unwrap(),
            WaitOutcome::Cancelled
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn two_process_notification_delivery() {
        const ENVIRONMENT: &str = "NWIPC_MACH_SIGNAL_DESCRIPTOR";
        if std::env::var_os(ENVIRONMENT).is_some() {
            return;
        }
        let (session, generation) = identity();
        let provider = MachSignal::initialize().unwrap();
        let (_resource, descriptor) = provider
            .create(session, generation, SignalDirection::PeerToRenderer)
            .unwrap();
        let mut listener = provider.listener(&descriptor, generation).unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::mach_signal_process_child", "--nocapture"])
            .env(ENVIRONMENT, encode_hex(&descriptor.encode()))
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(
            listener.wait_timeout(Duration::from_secs(1)).unwrap(),
            WaitOutcome::Signaled
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mach_signal_process_child() {
        let Ok(encoded) = std::env::var("NWIPC_MACH_SIGNAL_DESCRIPTOR") else {
            return;
        };
        let descriptor = MachSignalDescriptor::decode(&decode_hex(&encoded)).unwrap();
        MachSignal::initialize()
            .unwrap()
            .sender(&descriptor, descriptor.generation())
            .unwrap()
            .notify()
            .unwrap();
    }

    #[test]
    fn descriptor_is_generation_bound_and_redacted() {
        let descriptor = MachSignalDescriptor {
            service_name: format!("{PREFIX}.synthetic"),
            generation: Generation::new(4).unwrap(),
        };
        assert_eq!(
            MachSignalDescriptor::decode(&descriptor.encode()).unwrap(),
            descriptor
        );
        assert!(!format!("{descriptor:?}").contains("synthetic"));
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
