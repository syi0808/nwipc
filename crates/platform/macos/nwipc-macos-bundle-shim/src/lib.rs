//! C ABI entrypoint and unwind boundary for the injected bundle.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_macos_bundle_api::{BundleEntrypoint, BundleEvent};
#[cfg(target_os = "macos")]
use nwipc_macos_transport::MacosRendererTransportFactory;
#[cfg(all(target_os = "macos", feature = "e2e-fault-injection"))]
use nwipc_macos_transport::{FaultInjection, NotificationFault, WriterCrashPoint};
#[cfg(target_os = "macos")]
use nwipc_renderer_api::{RendererTransport, SendDisposition, TransportEvent};
#[cfg(target_os = "macos")]
use nwipc_renderer_bootstrap::RendererBootstrap;
#[cfg(target_os = "macos")]
use nwipc_webkit_testkit::{
    EXACT_INLINE_LENGTH, FRAGMENTED_MESSAGE_LENGTH, MAXIMUM_MESSAGE_LENGTH,
};

/// Prefix accepted for per-run E2E bundle-load notifications.
pub const E2E_BUNDLE_LOAD_NOTIFICATION_PREFIX: &str = "dev.nwipc.webkit-e2e.bundle-loaded.";
/// Prefix accepted for per-run renderer↔peer echo completion notifications.
pub const E2E_TRANSPORT_NOTIFICATION_PREFIX: &str = "dev.nwipc.webkit-e2e.transport.";

static ENTRYPOINT: OnceLock<Mutex<Option<Box<dyn BundleEntrypoint>>>> = OnceLock::new();
static INITIALIZATION_FAILED: AtomicBool = AtomicBool::new(false);
#[cfg(target_os = "macos")]
static E2E_CONFIGURATION: OnceLock<E2eConfiguration> = OnceLock::new();

#[cfg(target_os = "macos")]
struct E2eConfiguration {
    renderer_bootstrap: String,
    load_notification: String,
    transport_notification: String,
    timeout: std::time::Duration,
    #[cfg(feature = "e2e-fault-injection")]
    fault: E2eFault,
}

#[cfg(all(target_os = "macos", feature = "e2e-fault-injection"))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum E2eFault {
    #[default]
    None,
    NotificationDropped,
    NotificationDuplicate,
    NotificationDelayed,
    WriterBeforeCommit,
    WriterAfterCommit,
}

/// Installs the Rust orchestration object before `WebKit` invokes the exported entrypoint.
///
/// # Errors
///
/// Rejects duplicate installation.
pub fn install_entrypoint(entrypoint: Box<dyn BundleEntrypoint>) -> Result<(), ErrorReport> {
    let mut slot = ENTRYPOINT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| shim_error("bundle entrypoint lock"))?;
    if slot.is_some() {
        return Err(shim_error("duplicate bundle entrypoint"));
    }
    *slot = Some(entrypoint);
    Ok(())
}

/// Dispatches normalized callbacks while preventing a Rust panic from crossing the ABI.
///
/// # Errors
///
/// Reports missing initialization, callback errors, and caught panics.
pub fn dispatch(event: BundleEvent<'_>) -> Result<(), ErrorReport> {
    let mut slot = ENTRYPOINT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| shim_error("bundle callback lock"))?;
    let result = invoke(
        slot.as_mut()
            .ok_or_else(|| shim_error("missing bundle entrypoint"))?
            .as_mut(),
        event,
    );
    if result
        .as_ref()
        .is_err_and(|error| error.context().operation_name() == "bundle callback panic")
    {
        INITIALIZATION_FAILED.store(true, Ordering::Release);
    }
    result
}

fn invoke(
    entrypoint: &mut dyn BundleEntrypoint,
    event: BundleEvent<'_>,
) -> Result<(), ErrorReport> {
    match catch_unwind(AssertUnwindSafe(|| entrypoint.handle(event))) {
        Ok(result) => result,
        Err(_) => Err(shim_error("bundle callback panic")),
    }
}

/// Whether the ABI boundary has caught a panic.
pub fn initialization_failed() -> bool {
    INITIALIZATION_FAILED.load(Ordering::Acquire)
}

/// `WebKit` injected-bundle entrypoint. Raw `WebKit` values remain in the shim and are never
/// dereferenced by portable Rust.
#[cfg_attr(target_os = "macos", unsafe(no_mangle))]
pub extern "C" fn WKBundleInitialize(bundle: *mut c_void, _user_data: *mut c_void) {
    if catch_unwind(AssertUnwindSafe(|| {
        configure_e2e(bundle);
        post_e2e_load_marker();
        start_e2e_transport_matrix();
        let slot = ENTRYPOINT.get_or_init(|| Mutex::new(None));
        if slot.lock().map_or(true, |entrypoint| entrypoint.is_none()) {
            INITIALIZATION_FAILED.store(true, Ordering::Release);
        }
    }))
    .is_err()
    {
        INITIALIZATION_FAILED.store(true, Ordering::Release);
    }
}

#[cfg(target_os = "macos")]
fn post_e2e_load_marker() {
    if let Some(configuration) = E2E_CONFIGURATION.get() {
        post_e2e_notification(
            &configuration.load_notification,
            E2E_BUNDLE_LOAD_NOTIFICATION_PREFIX,
        );
    }
}

#[cfg(target_os = "macos")]
fn post_e2e_notification(name: &str, prefix: &str) {
    use std::ffi::{CString, c_char};

    #[link(name = "System")]
    unsafe extern "C" {
        fn notify_post(name: *const c_char) -> u32;
    }

    if !name.starts_with(prefix) || name.len() > 128 {
        return;
    }
    if let Ok(name) = CString::new(name) {
        unsafe { notify_post(name.as_ptr()) };
    }
}

#[cfg(target_os = "macos")]
fn start_e2e_transport_matrix() {
    static STARTED: OnceLock<()> = OnceLock::new();
    if E2E_CONFIGURATION.get().is_none() || STARTED.set(()).is_err() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("nwipc-webkit-e2e-transport".into())
        .spawn(|| {
            if let Err(error) = run_e2e_transport_matrix() {
                if let Some(configuration) = E2E_CONFIGURATION.get() {
                    let stage = get_e2e_state(&configuration.transport_notification)
                        .unwrap_or_default()
                        .min(10_000);
                    set_e2e_state(
                        &configuration.transport_notification,
                        2000 + stage * 100 + error.code() as u64,
                    );
                    post_e2e_notification(
                        &configuration.transport_notification,
                        E2E_TRANSPORT_NOTIFICATION_PREFIX,
                    );
                }
            }
        });
}

#[cfg(target_os = "macos")]
fn run_e2e_transport_matrix() -> Result<(), ErrorReport> {
    let configuration = E2E_CONFIGURATION
        .get()
        .ok_or_else(|| shim_error("E2E bundle parameters"))?;
    set_e2e_state(&configuration.transport_notification, 10);
    let bootstrap = decode_hex(&configuration.renderer_bootstrap)
        .ok_or_else(|| shim_error("E2E renderer bootstrap encoding"))?;
    let envelope = nwipc_bootstrap_codec::decode(&bootstrap)?;
    let session = envelope.session_id();
    let generation = envelope.generation();
    let protocol = envelope.protocols().minimum();
    #[cfg(feature = "e2e-fault-injection")]
    let mut factory = match configuration.fault {
        E2eFault::None => MacosRendererTransportFactory::default(),
        E2eFault::NotificationDropped => {
            fault_factory(NotificationFault::Dropped, WriterCrashPoint::None)
        }
        E2eFault::NotificationDuplicate => {
            fault_factory(NotificationFault::Duplicate, WriterCrashPoint::None)
        }
        E2eFault::NotificationDelayed => {
            fault_factory(NotificationFault::Delayed, WriterCrashPoint::None)
        }
        E2eFault::WriterBeforeCommit => {
            fault_factory(NotificationFault::None, WriterCrashPoint::BeforeCommit)
        }
        E2eFault::WriterAfterCommit => {
            fault_factory(NotificationFault::None, WriterCrashPoint::AfterCommit)
        }
    };
    #[cfg(not(feature = "e2e-fault-injection"))]
    let mut factory = MacosRendererTransportFactory::default();
    let mut transport =
        RendererBootstrap::open_transport(envelope, session, generation, protocol, &mut factory)?;
    let deadline = std::time::Instant::now() + configuration.timeout;
    set_e2e_state(&configuration.transport_notification, 20);
    #[cfg(feature = "e2e-fault-injection")]
    if configuration.fault == E2eFault::None {
        run_standard_transport_matrix(
            &mut transport,
            &configuration.transport_notification,
            deadline,
        )?;
    } else {
        let fault_payload = payload(257, 0xf017_f017);
        let _ = transport.send(&fault_payload)?;
        wait_for_echo(&mut transport, &fault_payload, deadline)?;
    }
    #[cfg(not(feature = "e2e-fault-injection"))]
    run_standard_transport_matrix(
        &mut transport,
        &configuration.transport_notification,
        deadline,
    )?;
    transport.close()?;
    set_e2e_state(&configuration.transport_notification, 1);
    post_e2e_notification(
        &configuration.transport_notification,
        E2E_TRANSPORT_NOTIFICATION_PREFIX,
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn run_standard_transport_matrix(
    transport: &mut impl RendererTransport,
    notification: &str,
    deadline: std::time::Instant,
) -> Result<(), ErrorReport> {
    for length in [
        0,
        EXACT_INLINE_LENGTH,
        FRAGMENTED_MESSAGE_LENGTH,
        MAXIMUM_MESSAGE_LENGTH,
    ] {
        let payload = payload(length, u64::try_from(length).unwrap_or(u64::MAX));
        let _ = transport.send(&payload)?;
        wait_for_echo(transport, &payload, deadline)?;
    }

    let saturation_payload = payload(4096, 0xfeed_beef);
    set_e2e_state(notification, 30);
    let mut outstanding = 0_usize;
    loop {
        outstanding = outstanding
            .checked_add(1)
            .ok_or_else(|| shim_error("E2E saturation count"))?;
        if transport.send(&saturation_payload)? == SendDisposition::Backpressured {
            break;
        }
        if outstanding > 4096 {
            set_e2e_state(notification, 301);
            return Err(shim_error("E2E transport did not backpressure"));
        }
    }
    let mut writable = false;
    set_e2e_state(notification, 40);
    while outstanding != 0 || !writable {
        if std::time::Instant::now() >= deadline {
            set_e2e_state(notification, 404);
            return Err(shim_error("E2E writable recovery timeout"));
        }
        match transport.poll()? {
            Some(TransportEvent::Message(payload)) if payload == saturation_payload => {
                outstanding -= 1;
                if outstanding == 0 {
                    set_e2e_state(notification, 41);
                } else if outstanding % 32 == 0 {
                    set_e2e_state(
                        notification,
                        1000 + u64::try_from(outstanding).unwrap_or(u64::MAX - 1000),
                    );
                }
            }
            Some(TransportEvent::Writable) => {
                writable = true;
                set_e2e_state(notification, 42);
            }
            Some(TransportEvent::Error(error)) => return Err(error),
            Some(_) => {
                set_e2e_state(notification, 405);
                return Err(shim_error("E2E saturation echo mismatch"));
            }
            None => std::thread::sleep(std::time::Duration::from_millis(1)),
        }
    }
    set_e2e_state(notification, 50);
    Ok(())
}

#[cfg(all(target_os = "macos", feature = "e2e-fault-injection"))]
fn fault_factory(
    notification: NotificationFault,
    writer_crash: WriterCrashPoint,
) -> MacosRendererTransportFactory {
    MacosRendererTransportFactory::with_fault_injection(FaultInjection {
        notification,
        writer_crash,
    })
}

#[cfg(target_os = "macos")]
fn set_e2e_state(name: &str, state: u64) {
    use std::ffi::{CString, c_char};

    #[link(name = "System")]
    unsafe extern "C" {
        fn notify_register_check(name: *const c_char, token: *mut i32) -> u32;
        fn notify_set_state(token: i32, state: u64) -> u32;
        fn notify_cancel(token: i32) -> u32;
    }

    let Ok(name) = CString::new(name) else {
        return;
    };
    let mut token = 0;
    unsafe {
        if notify_register_check(name.as_ptr(), &raw mut token) == 0 {
            let _ = notify_set_state(token, state);
            let _ = notify_cancel(token);
        }
    }
}

#[cfg(target_os = "macos")]
fn get_e2e_state(name: &str) -> Option<u64> {
    use std::ffi::{CString, c_char};

    #[link(name = "System")]
    unsafe extern "C" {
        fn notify_register_check(name: *const c_char, token: *mut i32) -> u32;
        fn notify_get_state(token: i32, state: *mut u64) -> u32;
        fn notify_cancel(token: i32) -> u32;
    }

    let name = CString::new(name).ok()?;
    let mut token = 0;
    let mut state = 0;
    unsafe {
        if notify_register_check(name.as_ptr(), &raw mut token) != 0 {
            return None;
        }
        let status = notify_get_state(token, &raw mut state);
        let _ = notify_cancel(token);
        (status == 0).then_some(state)
    }
}

#[cfg(target_os = "macos")]
fn wait_for_echo(
    transport: &mut impl RendererTransport,
    expected: &[u8],
    deadline: std::time::Instant,
) -> Result<(), ErrorReport> {
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(shim_error("E2E boundary echo timeout"));
        }
        match transport.poll()? {
            Some(TransportEvent::Message(payload)) if payload == expected => return Ok(()),
            Some(TransportEvent::Writable) | None => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Some(TransportEvent::Error(error)) => return Err(error),
            Some(_) => return Err(shim_error("E2E boundary echo mismatch")),
        }
    }
}

#[cfg(target_os = "macos")]
fn payload(length: usize, seed: u64) -> Vec<u8> {
    let mut state = seed ^ 0x9e37_79b9_7f4a_7c15;
    (0..length)
        .map(|index| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            if index % 251 == 0 {
                0
            } else {
                state.to_le_bytes()[0]
            }
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn decode_hex(input: &str) -> Option<Vec<u8>> {
    if input.len() % 2 != 0 || input.len() > 32 * 1024 {
        return None;
    }
    let mut output = vec![0; input.len() / 2];
    for (index, byte) in output.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&input[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(output)
}

#[cfg(target_os = "macos")]
fn configure_e2e(bundle: *mut c_void) {
    if bundle.is_null() || E2E_CONFIGURATION.get().is_some() {
        return;
    }
    let Some(enabled) = copy_bundle_parameter(bundle, "nwipc.e2e.enabled") else {
        return;
    };
    if enabled != "1" {
        return;
    }
    let Some(renderer_bootstrap) = copy_bundle_parameter(bundle, "nwipc.e2e.renderer-bootstrap")
    else {
        return;
    };
    let Some(load_notification) = copy_bundle_parameter(bundle, "nwipc.e2e.load-notification")
    else {
        return;
    };
    let Some(transport_notification) =
        copy_bundle_parameter(bundle, "nwipc.e2e.transport-notification")
    else {
        return;
    };
    let timeout = copy_bundle_parameter(bundle, "nwipc.e2e.timeout")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| (1..=300).contains(seconds))
        .unwrap_or(20);
    #[cfg(feature = "e2e-fault-injection")]
    let fault = match copy_bundle_parameter(bundle, "nwipc.e2e.fault").as_deref() {
        None | Some("" | "none" | "normal" | "peer-kill") => E2eFault::None,
        Some("notification-dropped") => E2eFault::NotificationDropped,
        Some("notification-duplicate") => E2eFault::NotificationDuplicate,
        Some("notification-delayed") => E2eFault::NotificationDelayed,
        Some("writer-before-commit") => E2eFault::WriterBeforeCommit,
        Some("writer-after-commit") => E2eFault::WriterAfterCommit,
        Some(_) => return,
    };
    if decode_hex(&renderer_bootstrap).is_none()
        || !load_notification.starts_with(E2E_BUNDLE_LOAD_NOTIFICATION_PREFIX)
        || !transport_notification.starts_with(E2E_TRANSPORT_NOTIFICATION_PREFIX)
    {
        return;
    }
    let _ = E2E_CONFIGURATION.set(E2eConfiguration {
        renderer_bootstrap,
        load_notification,
        transport_notification,
        timeout: std::time::Duration::from_secs(timeout),
        #[cfg(feature = "e2e-fault-injection")]
        fault,
    });
}

#[cfg(target_os = "macos")]
fn copy_bundle_parameter(bundle: *mut c_void, key: &str) -> Option<String> {
    use std::ffi::{CString, c_char};

    #[link(name = "WebKit", kind = "framework")]
    unsafe extern "C" {
        fn WKBundleGetParameters(bundle: *mut c_void) -> *const c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(value: *const c_void);
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            value: *const c_char,
            encoding: u32,
        ) -> *const c_void;
    }
    #[link(name = "objc")]
    unsafe extern "C" {
        fn sel_registerName(name: *const c_char) -> *const c_void;
        fn objc_msgSend(
            receiver: *const c_void,
            selector: *const c_void,
            argument: *const c_void,
        ) -> *const c_void;
    }

    let key = CString::new(key).ok()?;
    unsafe {
        let parameters = WKBundleGetParameters(bundle);
        if parameters.is_null() {
            return None;
        }
        let key = CFStringCreateWithCString(std::ptr::null(), key.as_ptr(), 0x0800_0100);
        if key.is_null() {
            return None;
        }
        let value_for_key = sel_registerName(c"valueForKey:".as_ptr());
        let value = objc_msgSend(parameters, value_for_key, key);
        CFRelease(key);
        if value.is_null() {
            return None;
        }
        let utf8_string = sel_registerName(c"UTF8String".as_ptr());
        let value = objc_msgSend(value, utf8_string, std::ptr::null()).cast::<c_char>();
        if value.is_null() {
            return None;
        }
        let value = std::ffi::CStr::from_ptr(value);
        if value.to_bytes().len() > 32 * 1024 {
            return None;
        }
        value.to_str().ok().map(str::to_owned)
    }
}

#[cfg(not(target_os = "macos"))]
fn post_e2e_load_marker() {}

#[cfg(not(target_os = "macos"))]
fn start_e2e_transport_matrix() {}

#[cfg(not(target_os = "macos"))]
fn configure_e2e(_: *mut c_void) {}

fn shim_error(operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Platform,
        ErrorCode::Internal,
        Recoverability::ReplaceEndpoint,
        operation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Panics;
    impl BundleEntrypoint for Panics {
        fn handle(&mut self, _: BundleEvent<'_>) -> Result<(), ErrorReport> {
            panic!("contained")
        }
    }

    #[test]
    fn panic_does_not_escape_callback_boundary() {
        let mut local: Box<dyn BundleEntrypoint> = Box::new(Panics);
        assert_eq!(
            invoke(local.as_mut(), BundleEvent::Initialize(b"x"))
                .unwrap_err()
                .code(),
            ErrorCode::Internal
        );
    }
}
