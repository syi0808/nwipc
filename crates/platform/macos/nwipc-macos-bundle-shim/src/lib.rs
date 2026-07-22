//! C ABI entrypoint and unwind boundary for the injected bundle.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};
use nwipc_macos_bundle_api::{BundleEntrypoint, BundleEvent};

/// Prefix accepted for per-run E2E bundle-load notifications.
pub const E2E_BUNDLE_LOAD_NOTIFICATION_PREFIX: &str = "dev.nwipc.webkit-e2e.bundle-loaded.";

static ENTRYPOINT: OnceLock<Mutex<Option<Box<dyn BundleEntrypoint>>>> = OnceLock::new();
static INITIALIZATION_FAILED: AtomicBool = AtomicBool::new(false);

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
pub extern "C" fn WKBundleInitialize(_bundle: *mut c_void, _user_data: *mut c_void) {
    if catch_unwind(AssertUnwindSafe(|| {
        post_e2e_load_marker();
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
    use std::ffi::{CString, c_char};

    #[link(name = "System")]
    unsafe extern "C" {
        fn notify_post(name: *const c_char) -> u32;
    }

    if std::env::var_os("NWIPC_WEBKIT_E2E").is_none_or(|value| value != "1") {
        return;
    }
    let Ok(name) = std::env::var("NWIPC_WEBKIT_E2E_NOTIFICATION") else {
        return;
    };
    if !name.starts_with(E2E_BUNDLE_LOAD_NOTIFICATION_PREFIX) || name.len() > 128 {
        return;
    }
    if let Ok(name) = CString::new(name) {
        unsafe { notify_post(name.as_ptr()) };
    }
}

#[cfg(not(target_os = "macos"))]
fn post_e2e_load_marker() {}

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
