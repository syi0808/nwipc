//! Fail-closed discovery of the private `WebKit` surface used by the injected bundle host.

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};

/// SPI entries required by the vertical slice.
pub const REQUIRED_SPI: [SpiEntry; 3] = [
    SpiEntry::selector(
        "_WKProcessPoolConfiguration",
        "_setInjectedBundleURL:",
        SpiRequirement::Required,
    ),
    SpiEntry::selector(
        "WKWebViewConfiguration",
        "_setProcessPoolConfiguration:",
        SpiRequirement::Required,
    ),
    SpiEntry::selector(
        "_WKProcessPoolConfiguration",
        "_setInjectedBundleInitializationUserData:",
        SpiRequirement::Required,
    ),
];

/// Explicitly tested macOS releases. Patch updates still require the same SPI method probe.
pub const SUPPORTED_OS: [(u16, u16); 1] = [(26, 2)];

/// One symbol or Objective-C selector used by the adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpiEntry {
    /// Objective-C class queried for the method.
    pub class_name: &'static str,
    /// Stable lookup name.
    pub name: &'static str,
    /// Lookup namespace.
    pub kind: SpiKind,
    /// Whether startup can proceed without the entry.
    pub requirement: SpiRequirement,
}

impl SpiEntry {
    /// Creates a selector manifest entry.
    pub const fn selector(
        class_name: &'static str,
        name: &'static str,
        requirement: SpiRequirement,
    ) -> Self {
        Self {
            class_name,
            name,
            kind: SpiKind::Selector,
            requirement,
        }
    }
}

/// Namespace containing an SPI entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpiKind {
    /// Objective-C selector.
    Selector,
    /// Dynamic linker symbol.
    Symbol,
}

/// Availability policy for an entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpiRequirement {
    /// Missing entry makes the platform unsupported.
    Required,
    /// Missing entry disables only an optional diagnostic.
    Optional,
}

/// Numeric operating-system version used by the compatibility allowlist.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct OsVersion {
    /// Major release.
    pub major: u16,
    /// Minor release.
    pub minor: u16,
    /// Patch release.
    pub patch: u16,
}

/// Injectable runtime queries, keeping raw Objective-C objects inside the adapter.
pub trait SpiProbe {
    /// Reports the running macOS version.
    fn os_version(&self) -> Option<OsVersion>;
    /// Reports whether the named entry is callable on the configured object.
    fn has_entry(&self, entry: SpiEntry) -> bool;
}

/// Runtime probe backed by `sw_vers` and the Objective-C method table.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemSpiProbe;

impl SpiProbe for SystemSpiProbe {
    fn os_version(&self) -> Option<OsVersion> {
        system_os_version()
    }
    fn has_entry(&self, entry: SpiEntry) -> bool {
        system_has_entry(entry)
    }
}

/// Verified access token required by the host configuration API.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MacosSpi {
    version: OsVersion,
}

impl MacosSpi {
    /// Probes the explicit version allowlist and every required entry.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` without silently falling back when compatibility is unknown.
    pub fn probe(probe: &impl SpiProbe) -> Result<Self, ErrorReport> {
        let version = probe.os_version().ok_or_else(unsupported)?;
        if !is_version_allowed(version)
            || REQUIRED_SPI.iter().any(|entry| {
                entry.requirement == SpiRequirement::Required && !probe.has_entry(*entry)
            })
        {
            return Err(unsupported());
        }
        Ok(Self { version })
    }

    /// Runs the system OS and Objective-C method-table probe.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` when the current release or required method is unavailable.
    pub fn initialize() -> Result<Self, ErrorReport> {
        Self::probe(&SystemSpiProbe)
    }

    /// Running OS version that passed the allowlist.
    pub const fn version(self) -> OsVersion {
        self.version
    }
}

const fn is_version_allowed(version: OsVersion) -> bool {
    let mut index = 0;
    while index < SUPPORTED_OS.len() {
        let (major, minor) = SUPPORTED_OS[index];
        if version.major == major && version.minor == minor {
            return true;
        }
        index += 1;
    }
    false
}

fn unsupported() -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Unsupported,
        ErrorCode::Unsupported,
        Recoverability::Terminal,
        "webkit spi compatibility probe",
    )
}

#[cfg(target_os = "macos")]
fn system_os_version() -> Option<OsVersion> {
    let output = std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let version = std::str::from_utf8(&output.stdout).ok()?.trim();
    let mut components = version.split('.').map(str::parse::<u16>);
    Some(OsVersion {
        major: components.next()?.ok()?,
        minor: components.next().transpose().ok()?.unwrap_or(0),
        patch: components.next().transpose().ok()?.unwrap_or(0),
    })
}

#[cfg(not(target_os = "macos"))]
const fn system_os_version() -> Option<OsVersion> {
    None
}

#[cfg(target_os = "macos")]
fn system_has_entry(entry: SpiEntry) -> bool {
    use std::ffi::{CString, c_char, c_void};
    use std::sync::OnceLock;

    #[link(name = "System")]
    unsafe extern "C" {
        fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
    }
    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> *mut c_void;
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn class_getInstanceMethod(class: *mut c_void, selector: *mut c_void) -> *mut c_void;
    }

    static WEBKIT_LOADED: OnceLock<bool> = OnceLock::new();
    let loaded = WEBKIT_LOADED.get_or_init(|| {
        let path = c"/System/Library/Frameworks/WebKit.framework/WebKit";
        unsafe { !dlopen(path.as_ptr(), 0x1).is_null() }
    });
    if !loaded {
        return false;
    }

    let Ok(class_name) = CString::new(entry.class_name) else {
        return false;
    };
    let Ok(selector_name) = CString::new(entry.name) else {
        return false;
    };
    unsafe {
        let class = objc_getClass(class_name.as_ptr());
        let selector = sel_registerName(selector_name.as_ptr());
        !class.is_null()
            && !selector.is_null()
            && !class_getInstanceMethod(class, selector).is_null()
    }
}

#[cfg(not(target_os = "macos"))]
const fn system_has_entry(_: SpiEntry) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Probe {
        version: Option<OsVersion>,
        missing: Option<&'static str>,
    }

    impl SpiProbe for Probe {
        fn os_version(&self) -> Option<OsVersion> {
            self.version
        }
        fn has_entry(&self, entry: SpiEntry) -> bool {
            self.missing != Some(entry.name)
        }
    }

    #[test]
    fn requires_allowlisted_os_and_complete_spi() {
        let supported = Probe {
            version: Some(OsVersion {
                major: 26,
                minor: 2,
                patch: 0,
            }),
            missing: None,
        };
        assert_eq!(MacosSpi::probe(&supported).unwrap().version().major, 26);
        let missing = Probe {
            missing: Some(REQUIRED_SPI[1].name),
            ..supported
        };
        assert_eq!(
            MacosSpi::probe(&missing).unwrap_err().code(),
            ErrorCode::Unsupported
        );
    }

    #[test]
    fn unknown_os_fails_closed() {
        let probe = Probe {
            version: Some(OsVersion {
                major: 27,
                minor: 0,
                patch: 0,
            }),
            missing: None,
        };
        assert!(MacosSpi::probe(&probe).is_err());
    }
}
