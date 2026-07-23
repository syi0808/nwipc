//! Runtime discovery and evidence classification of the private `WebKit` host surface.

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};

/// SPI entries required by the vertical slice.
pub const REQUIRED_SPI: [SpiEntry; 4] = [
    SpiEntry::selector(
        "_WKProcessPoolConfiguration",
        "setInjectedBundleURL:",
        SpiRequirement::Required,
    ),
    SpiEntry::selector(
        "WKProcessPool",
        "_initWithConfiguration:",
        SpiRequirement::Required,
    ),
    SpiEntry::selector(
        "WKProcessPool",
        "_setObject:forBundleParameter:",
        SpiRequirement::Required,
    ),
    SpiEntry::selector(
        "WKWebViewConfiguration",
        "setProcessPool:",
        SpiRequirement::Required,
    ),
];

/// Lowest macOS release supported by Rust's `x86_64` Apple target.
pub const MINIMUM_X86_64_OS: OsVersion = OsVersion::new(10, 12, 0);

/// Lowest macOS release supported by Rust's arm64 Apple target.
pub const MINIMUM_ARM64_OS: OsVersion = OsVersion::new(11, 0, 0);

/// Signed, hardened systems that completed the production `WKWebView` E2E matrix.
pub const VERIFIED_SYSTEMS: [VerifiedSystem; 1] = [VerifiedSystem {
    version: OsVersion::new(26, 2, 0),
    build: "25C56",
    architecture: MacosArchitecture::Arm64,
}];

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

/// Numeric operating-system version used by compatibility assessment.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct OsVersion {
    /// Major release.
    pub major: u16,
    /// Minor release.
    pub minor: u16,
    /// Patch release.
    pub patch: u16,
}

impl OsVersion {
    /// Creates a numeric macOS product version.
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

/// CPU architecture relevant to deployment-target and verification evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MacosArchitecture {
    /// Apple Silicon target.
    Arm64,
    /// 64-bit Intel target.
    X86_64,
    /// A target for which NWIPC does not build macOS artifacts.
    Other,
}

/// One exact system with signed, hardened E2E evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedSystem {
    /// macOS product version.
    pub version: OsVersion,
    /// macOS build identifier.
    pub build: &'static str,
    /// CPU architecture.
    pub architecture: MacosArchitecture,
}

/// Runtime support level for an executable macOS configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MacosSupport {
    /// The exact OS build and architecture completed the production E2E matrix.
    Verified,
    /// Required runtime capabilities exist, but this exact system has no E2E guarantee.
    BestEffort,
}

/// Reason a configuration cannot logically execute the macOS `WebKit` path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncompatibilityReason {
    /// The operating-system version could not be determined.
    UnknownOsVersion,
    /// The binary's Rust target does not support this operating-system version.
    BelowDeploymentTarget,
    /// NWIPC does not build macOS artifacts for this CPU architecture.
    UnsupportedArchitecture,
    /// A private class or selector required for invocation is absent.
    MissingRequiredSpi,
}

/// Result of checking execution viability separately from verification evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompatibilityOutcome {
    /// Startup can proceed at the reported support level.
    Runnable(MacosSupport),
    /// Startup cannot safely reach the required implementation.
    Incompatible(IncompatibilityReason),
}

/// Complete, non-secret result of the macOS compatibility probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompatibilityAssessment {
    version: Option<OsVersion>,
    architecture: MacosArchitecture,
    outcome: CompatibilityOutcome,
}

impl CompatibilityAssessment {
    /// Reported macOS product version, when the system query succeeded.
    pub const fn version(self) -> Option<OsVersion> {
        self.version
    }

    /// Reported CPU architecture.
    pub const fn architecture(self) -> MacosArchitecture {
        self.architecture
    }

    /// Execution viability and verification level.
    pub const fn outcome(self) -> CompatibilityOutcome {
        self.outcome
    }
}

/// Injectable runtime queries, keeping raw Objective-C objects inside the adapter.
pub trait SpiProbe {
    /// Reports the running macOS version.
    fn os_version(&self) -> Option<OsVersion>;
    /// Reports the macOS build identifier.
    fn os_build(&self) -> Option<String>;
    /// Reports the current CPU architecture.
    fn architecture(&self) -> MacosArchitecture;
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
    fn os_build(&self) -> Option<String> {
        system_os_value("-buildVersion")
    }
    fn architecture(&self) -> MacosArchitecture {
        system_architecture()
    }
    fn has_entry(&self, entry: SpiEntry) -> bool {
        system_has_entry(entry)
    }
}

/// Verified access token required by the host configuration API.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MacosSpi {
    version: OsVersion,
    architecture: MacosArchitecture,
    support: MacosSupport,
}

impl MacosSpi {
    /// Assesses whether the current system can execute the required private surface.
    pub fn assess(probe: &impl SpiProbe) -> CompatibilityAssessment {
        let version = probe.os_version();
        let architecture = probe.architecture();
        let outcome = match version {
            None => CompatibilityOutcome::Incompatible(IncompatibilityReason::UnknownOsVersion),
            Some(version) => assess_known_system(probe, version, architecture),
        };
        CompatibilityAssessment {
            version,
            architecture,
            outcome,
        }
    }

    /// Probes execution viability and returns a token for runnable systems.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` only when the binary cannot reach every required implementation.
    pub fn probe(probe: &impl SpiProbe) -> Result<Self, ErrorReport> {
        let assessment = Self::assess(probe);
        match (assessment.version, assessment.outcome) {
            (Some(version), CompatibilityOutcome::Runnable(support)) => Ok(Self {
                version,
                architecture: assessment.architecture,
                support,
            }),
            (None, CompatibilityOutcome::Runnable(_)) => {
                Err(incompatible(IncompatibilityReason::UnknownOsVersion))
            }
            (_, CompatibilityOutcome::Incompatible(reason)) => Err(incompatible(reason)),
        }
    }

    /// Runs the system OS and Objective-C method-table probe.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` when the deployment target, architecture, or required SPI is
    /// unavailable. Untested but viable systems start in best-effort mode.
    pub fn initialize() -> Result<Self, ErrorReport> {
        Self::probe(&SystemSpiProbe)
    }

    /// Running OS version that passed the viability probe.
    pub const fn version(self) -> OsVersion {
        self.version
    }

    /// Running CPU architecture.
    pub const fn architecture(self) -> MacosArchitecture {
        self.architecture
    }

    /// Evidence level for this exact OS build and architecture.
    pub const fn support(self) -> MacosSupport {
        self.support
    }
}

fn assess_known_system(
    probe: &impl SpiProbe,
    version: OsVersion,
    architecture: MacosArchitecture,
) -> CompatibilityOutcome {
    let minimum = match architecture {
        MacosArchitecture::Arm64 => MINIMUM_ARM64_OS,
        MacosArchitecture::X86_64 => MINIMUM_X86_64_OS,
        MacosArchitecture::Other => {
            return CompatibilityOutcome::Incompatible(
                IncompatibilityReason::UnsupportedArchitecture,
            );
        }
    };
    if version < minimum {
        return CompatibilityOutcome::Incompatible(IncompatibilityReason::BelowDeploymentTarget);
    }
    if REQUIRED_SPI
        .iter()
        .any(|entry| entry.requirement == SpiRequirement::Required && !probe.has_entry(*entry))
    {
        return CompatibilityOutcome::Incompatible(IncompatibilityReason::MissingRequiredSpi);
    }
    let build = probe.os_build();
    if VERIFIED_SYSTEMS.iter().any(|system| {
        system.version == version
            && system.architecture == architecture
            && build.as_deref() == Some(system.build)
    }) {
        CompatibilityOutcome::Runnable(MacosSupport::Verified)
    } else {
        CompatibilityOutcome::Runnable(MacosSupport::BestEffort)
    }
}

fn incompatible(reason: IncompatibilityReason) -> ErrorReport {
    let operation = match reason {
        IncompatibilityReason::UnknownOsVersion => "macos version compatibility probe",
        IncompatibilityReason::BelowDeploymentTarget => "macos deployment target compatibility",
        IncompatibilityReason::UnsupportedArchitecture => "macos architecture compatibility",
        IncompatibilityReason::MissingRequiredSpi => "webkit spi compatibility probe",
    };
    ErrorReport::new(
        ErrorCategory::Unsupported,
        ErrorCode::Unsupported,
        Recoverability::Terminal,
        operation,
    )
}

#[cfg(target_os = "macos")]
fn system_os_version() -> Option<OsVersion> {
    let version = system_os_value("-productVersion")?;
    let mut components = version.split('.').map(str::parse::<u16>);
    Some(OsVersion {
        major: components.next()?.ok()?,
        minor: components.next().transpose().ok()?.unwrap_or(0),
        patch: components.next().transpose().ok()?.unwrap_or(0),
    })
}

#[cfg(target_os = "macos")]
fn system_os_value(argument: &str) -> Option<String> {
    let output = std::process::Command::new("/usr/bin/sw_vers")
        .arg(argument)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(std::str::from_utf8(&output.stdout).ok()?.trim().to_owned())
}

#[cfg(not(target_os = "macos"))]
const fn system_os_version() -> Option<OsVersion> {
    None
}

#[cfg(not(target_os = "macos"))]
fn system_os_value(_: &str) -> Option<String> {
    None
}

const fn system_architecture() -> MacosArchitecture {
    if cfg!(target_arch = "aarch64") {
        MacosArchitecture::Arm64
    } else if cfg!(target_arch = "x86_64") {
        MacosArchitecture::X86_64
    } else {
        MacosArchitecture::Other
    }
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

    #[derive(Clone, Copy)]
    struct Probe {
        version: Option<OsVersion>,
        build: Option<&'static str>,
        architecture: MacosArchitecture,
        missing: Option<&'static str>,
    }

    impl SpiProbe for Probe {
        fn os_version(&self) -> Option<OsVersion> {
            self.version
        }
        fn os_build(&self) -> Option<String> {
            self.build.map(str::to_owned)
        }
        fn architecture(&self) -> MacosArchitecture {
            self.architecture
        }
        fn has_entry(&self, entry: SpiEntry) -> bool {
            self.missing != Some(entry.name)
        }
    }

    #[test]
    fn exact_e2e_system_is_verified() {
        let verified = Probe {
            version: Some(OsVersion::new(26, 2, 0)),
            build: Some("25C56"),
            architecture: MacosArchitecture::Arm64,
            missing: None,
        };
        let spi = MacosSpi::probe(&verified).unwrap();
        assert_eq!(spi.version(), OsVersion::new(26, 2, 0));
        assert_eq!(spi.architecture(), MacosArchitecture::Arm64);
        assert_eq!(spi.support(), MacosSupport::Verified);
    }

    #[test]
    fn unverified_systems_with_required_spi_run_best_effort() {
        for probe in [
            Probe {
                version: Some(OsVersion::new(26, 2, 1)),
                build: Some("25C57"),
                architecture: MacosArchitecture::Arm64,
                missing: None,
            },
            Probe {
                version: Some(OsVersion::new(27, 0, 0)),
                build: Some("26A1"),
                architecture: MacosArchitecture::Arm64,
                missing: None,
            },
            Probe {
                version: Some(MINIMUM_X86_64_OS),
                build: None,
                architecture: MacosArchitecture::X86_64,
                missing: None,
            },
        ] {
            assert_eq!(
                MacosSpi::probe(&probe).unwrap().support(),
                MacosSupport::BestEffort
            );
        }
    }

    #[test]
    fn missing_required_spi_is_incompatible() {
        let supported = Probe {
            version: Some(OsVersion::new(26, 2, 0)),
            build: Some("25C56"),
            architecture: MacosArchitecture::Arm64,
            missing: None,
        };
        let missing = Probe {
            missing: Some(REQUIRED_SPI[1].name),
            ..supported
        };
        assert_eq!(
            MacosSpi::assess(&missing).outcome(),
            CompatibilityOutcome::Incompatible(IncompatibilityReason::MissingRequiredSpi)
        );
        assert_eq!(
            MacosSpi::probe(&missing).unwrap_err().code(),
            ErrorCode::Unsupported
        );
    }

    #[test]
    fn deployment_target_and_architecture_boundaries_are_incompatible() {
        let too_old_x86 = Probe {
            version: Some(OsVersion::new(10, 11, 6)),
            build: None,
            architecture: MacosArchitecture::X86_64,
            missing: None,
        };
        assert_eq!(
            MacosSpi::assess(&too_old_x86).outcome(),
            CompatibilityOutcome::Incompatible(IncompatibilityReason::BelowDeploymentTarget)
        );
        let too_old_arm = Probe {
            version: Some(OsVersion::new(10, 15, 7)),
            architecture: MacosArchitecture::Arm64,
            ..too_old_x86
        };
        assert_eq!(
            MacosSpi::assess(&too_old_arm).outcome(),
            CompatibilityOutcome::Incompatible(IncompatibilityReason::BelowDeploymentTarget)
        );
        let other_architecture = Probe {
            version: Some(OsVersion::new(26, 2, 0)),
            architecture: MacosArchitecture::Other,
            ..too_old_x86
        };
        assert_eq!(
            MacosSpi::assess(&other_architecture).outcome(),
            CompatibilityOutcome::Incompatible(IncompatibilityReason::UnsupportedArchitecture)
        );
    }

    #[test]
    fn unknown_os_version_is_incompatible() {
        let probe = Probe {
            version: None,
            build: None,
            architecture: MacosArchitecture::Arm64,
            missing: None,
        };
        assert_eq!(
            MacosSpi::assess(&probe).outcome(),
            CompatibilityOutcome::Incompatible(IncompatibilityReason::UnknownOsVersion)
        );
    }
}
