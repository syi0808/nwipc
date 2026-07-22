//! C ABI entrypoint and unwind boundary for the injected bundle.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_macos_bundle_api::{BundleEntrypoint, BundleEvent};
#[cfg(target_os = "macos")]
use nwipc_memory_api::{MappedRegion, MappingAccess, SharedMemoryProvider};
#[cfg(target_os = "macos")]
use nwipc_memory_iosurface::{IoSurfaceDescriptor, IoSurfaceProvider};
#[cfg(target_os = "macos")]
use nwipc_types::Generation;
#[cfg(target_os = "macos")]
use nwipc_webkit_testkit::{
    ECHO_GENERATION, ECHO_PAYLOAD, ECHO_REGION_LENGTH, EchoState, decode_echo_frame,
    encode_echo_frame,
};

/// Prefix accepted for per-run E2E bundle-load notifications.
pub const E2E_BUNDLE_LOAD_NOTIFICATION_PREFIX: &str = "dev.nwipc.webkit-e2e.bundle-loaded.";
/// Prefix accepted for per-run renderer↔peer echo completion notifications.
pub const E2E_BINARY_ECHO_NOTIFICATION_PREFIX: &str = "dev.nwipc.webkit-e2e.binary-echo.";

static ENTRYPOINT: OnceLock<Mutex<Option<Box<dyn BundleEntrypoint>>>> = OnceLock::new();
static INITIALIZATION_FAILED: AtomicBool = AtomicBool::new(false);
#[cfg(target_os = "macos")]
static E2E_CONFIGURATION: OnceLock<E2eConfiguration> = OnceLock::new();

#[cfg(target_os = "macos")]
struct E2eConfiguration {
    descriptor: String,
    load_notification: String,
    echo_notification: String,
    timeout: std::time::Duration,
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
        start_e2e_binary_echo();
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
fn start_e2e_binary_echo() {
    static STARTED: OnceLock<()> = OnceLock::new();
    if E2E_CONFIGURATION.get().is_none() || STARTED.set(()).is_err() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("nwipc-webkit-e2e-echo".into())
        .spawn(|| {
            let _ = run_e2e_binary_echo();
        });
}

#[cfg(target_os = "macos")]
fn run_e2e_binary_echo() -> Result<(), ErrorReport> {
    let configuration = E2E_CONFIGURATION
        .get()
        .ok_or_else(|| shim_error("E2E bundle parameters"))?;
    let descriptor = decode_hex::<20>(&configuration.descriptor)
        .ok_or_else(|| shim_error("E2E IOSurface descriptor"))?;
    let descriptor = IoSurfaceDescriptor::decode(&descriptor)?;
    let generation =
        Generation::new(ECHO_GENERATION).ok_or_else(|| shim_error("E2E IOSurface generation"))?;
    let provider = IoSurfaceProvider::initialize()?;
    let mut mapping = provider.attach(&descriptor, generation, MappingAccess::ReadWrite)?;
    let mut snapshot = [0; ECHO_REGION_LENGTH];
    mapping.read(0, &mut snapshot)?;
    if decode_echo_frame(&snapshot).is_ok_and(|frame| frame.state == EchoState::RendererVerified) {
        return Ok(());
    }
    let request = encode_echo_frame(EchoState::RendererRequest, ECHO_PAYLOAD)?;
    mapping.write(0, &request)?;
    let deadline = std::time::Instant::now() + configuration.timeout;
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(shim_error("renderer peer echo timeout"));
        }
        mapping.read(0, &mut snapshot)?;
        if let Ok(frame) = decode_echo_frame(&snapshot) {
            if frame.state == EchoState::PeerEcho {
                if frame.payload != ECHO_PAYLOAD {
                    return Err(shim_error("renderer peer echo mismatch"));
                }
                let verified = encode_echo_frame(EchoState::RendererVerified, ECHO_PAYLOAD)?;
                mapping.write(0, &verified)?;
                post_e2e_notification(
                    &configuration.echo_notification,
                    E2E_BINARY_ECHO_NOTIFICATION_PREFIX,
                );
                return Ok(());
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

#[cfg(target_os = "macos")]
fn decode_hex<const LENGTH: usize>(input: &str) -> Option<[u8; LENGTH]> {
    if input.len() != LENGTH * 2 {
        return None;
    }
    let mut output = [0; LENGTH];
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
    let Some(descriptor) = copy_bundle_parameter(bundle, "nwipc.e2e.iosurface") else {
        return;
    };
    let Some(load_notification) = copy_bundle_parameter(bundle, "nwipc.e2e.load-notification")
    else {
        return;
    };
    let Some(echo_notification) = copy_bundle_parameter(bundle, "nwipc.e2e.echo-notification")
    else {
        return;
    };
    let timeout = copy_bundle_parameter(bundle, "nwipc.e2e.timeout")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| (1..=300).contains(seconds))
        .unwrap_or(20);
    if decode_hex::<20>(&descriptor).is_none()
        || !load_notification.starts_with(E2E_BUNDLE_LOAD_NOTIFICATION_PREFIX)
        || !echo_notification.starts_with(E2E_BINARY_ECHO_NOTIFICATION_PREFIX)
    {
        return;
    }
    let _ = E2E_CONFIGURATION.set(E2eConfiguration {
        descriptor,
        load_notification,
        echo_notification,
        timeout: std::time::Duration::from_secs(timeout),
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
        if value.to_bytes().len() > 1024 {
            return None;
        }
        value.to_str().ok().map(str::to_owned)
    }
}

#[cfg(not(target_os = "macos"))]
fn post_e2e_load_marker() {}

#[cfg(not(target_os = "macos"))]
fn start_e2e_binary_echo() {}

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
