//! Injected-bundle metadata and on-disk layout validation.

use std::fs;
use std::path::{Path, PathBuf};

use nwipc_error::{ErrorCategory, ErrorCode, ErrorReport, Recoverability};

/// Bundle executable name shared with `Info.plist` and `xtask`.
pub const BUNDLE_EXECUTABLE: &str = "nwipc-macos-bundle";
/// Embedded compatibility manifest name.
pub const MANIFEST_FILE: &str = "nwipc-manifest.txt";
/// Current artifact manifest format.
pub const MANIFEST_SCHEMA: u16 = 1;
/// Current NWIPC layout version embedded in artifacts.
pub const LAYOUT_VERSION: u16 = 1;
/// Current NWIPC protocol version embedded in artifacts.
pub const PROTOCOL_VERSION: u16 = 1;

/// Parsed and validated bundle paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MacosArtifact {
    bundle: PathBuf,
    executable: PathBuf,
}

impl MacosArtifact {
    /// Inspects the required bundle layout and compatibility manifest.
    ///
    /// # Errors
    ///
    /// Returns a structured configuration error for missing or mismatched artifacts.
    pub fn inspect(bundle: impl AsRef<Path>) -> Result<Self, ErrorReport> {
        let bundle = bundle.as_ref();
        let contents = bundle.join("Contents");
        let plist = contents.join("Info.plist");
        let executable = contents.join("MacOS").join(BUNDLE_EXECUTABLE);
        let manifest = contents.join("Resources").join(MANIFEST_FILE);
        if !plist.is_file() || !executable.is_file() || !manifest.is_file() {
            return Err(artifact_error("injected bundle layout"));
        }
        let plist =
            fs::read_to_string(plist).map_err(|_| artifact_error("injected bundle plist"))?;
        if !plist.contains(&format!("<string>{BUNDLE_EXECUTABLE}</string>")) {
            return Err(artifact_error("injected bundle executable metadata"));
        }
        let manifest =
            fs::read_to_string(manifest).map_err(|_| artifact_error("injected bundle manifest"))?;
        validate_manifest(&manifest)?;
        Ok(Self {
            bundle: bundle.to_path_buf(),
            executable,
        })
    }

    /// Bundle URL passed to `WebKit` configuration.
    pub fn bundle_path(&self) -> &Path {
        &self.bundle
    }
    /// Executable checked by the artifact inspector.
    pub fn executable_path(&self) -> &Path {
        &self.executable
    }
}

/// Produces the deterministic compatibility manifest embedded by `xtask`.
pub fn current_manifest() -> String {
    format!(
        "schema={MANIFEST_SCHEMA}\nbundleVersion={}\nlayoutVersion={LAYOUT_VERSION}\nprotocolVersion={PROTOCOL_VERSION}\narchitecture={}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH,
    )
}

/// Validates exact required manifest fields and current protocol compatibility.
///
/// # Errors
///
/// Rejects missing, duplicate, unknown, and mismatched fields.
pub fn validate_manifest(manifest: &str) -> Result<(), ErrorReport> {
    let mut fields = std::collections::BTreeMap::new();
    for line in manifest.lines() {
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| artifact_error("injected bundle manifest syntax"))?;
        if fields.insert(key, value).is_some() {
            return Err(artifact_error("injected bundle duplicate manifest field"));
        }
    }
    let expected = [
        ("schema", MANIFEST_SCHEMA.to_string()),
        ("bundleVersion", env!("CARGO_PKG_VERSION").to_owned()),
        ("layoutVersion", LAYOUT_VERSION.to_string()),
        ("protocolVersion", PROTOCOL_VERSION.to_string()),
        ("architecture", std::env::consts::ARCH.to_owned()),
    ];
    if fields.len() != expected.len()
        || expected
            .iter()
            .any(|(key, value)| fields.get(key).copied() != Some(value.as_str()))
    {
        return Err(artifact_error("injected bundle compatibility"));
    }
    Ok(())
}

fn artifact_error(operation: &'static str) -> ErrorReport {
    ErrorReport::new(
        ErrorCategory::Configuration,
        ErrorCode::ProtocolViolation,
        Recoverability::Terminal,
        operation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_manifest_round_trips() {
        validate_manifest(&current_manifest()).unwrap();
    }

    #[test]
    fn mismatch_and_unknown_fields_are_rejected() {
        assert!(
            validate_manifest(
                &current_manifest().replace("protocolVersion=1", "protocolVersion=2")
            )
            .is_err()
        );
        assert!(validate_manifest(&(current_manifest() + "optional=yes\n")).is_err());
    }
}
