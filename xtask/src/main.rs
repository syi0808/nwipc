use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const UNSAFE_CRATES: &[&str] = &[
    "nwipc-atomic",
    "nwipc-memory-iosurface",
    "nwipc-signal-darwin",
    "nwipc-renderer-jsc",
    "nwipc-macos-spi",
    "nwipc-macos-bundle-shim",
];

fn main() -> ExitCode {
    let mut arguments = env::args().skip(1);
    let result = match arguments.next().as_deref() {
        Some("architecture-check") => architecture_check(),
        Some("bundle-manifest") => bundle_manifest(arguments.next()),
        Some("bundle-inspect") => bundle_inspect(arguments.next()),
        Some("bundle-assemble" | "example-embed") => Err(
            "Unsupported: macOS artifact assembly starts after a bundle binary exists".into(),
        ),
        _ => Err(
            "usage: cargo xtask <architecture-check|bundle-manifest|bundle-inspect|bundle-assemble|example-embed> [path]"
                .into(),
        ),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::FAILURE
        }
    }
}

fn architecture_check() -> Result<(), String> {
    let root = workspace_root()?;
    let mut manifests = Vec::new();
    collect_named_files(
        &root.join("crates"),
        OsStr::new("Cargo.toml"),
        &mut manifests,
    )?;
    let mut violations = Vec::new();

    for manifest_path in manifests {
        let manifest = read(&manifest_path)?;
        let crate_name = package_name(&manifest)
            .ok_or_else(|| format!("missing package name in {}", manifest_path.display()))?;
        let crate_root = manifest_path.parent().expect("manifest has a parent");

        for key in ["layer =", "stability =", "owner ="] {
            if !manifest.contains(key) {
                violations.push(format!("{crate_name}: missing metadata `{key}`"));
            }
        }
        if !crate_root.join("src/lib.rs").is_file() {
            violations.push(format!("{crate_name}: missing src/lib.rs"));
        }

        let permits_unsafe = UNSAFE_CRATES.contains(&crate_name.as_str());
        if permits_unsafe != manifest.contains("unsafe_code = \"allow\"") {
            violations.push(format!(
                "{crate_name}: unsafe lint does not match the allowlist"
            ));
        }

        if crate_name.starts_with("nwipc-macos-host")
            && [
                "nwipc-ring-writer",
                "nwipc-ring-reader",
                "nwipc-channel-core",
            ]
            .iter()
            .any(|dependency| manifest.contains(dependency))
        {
            violations.push(format!(
                "{crate_name}: host must not depend on data-plane implementations"
            ));
        }

        if crate_name.starts_with("nwipc-types")
            || crate_name.starts_with("nwipc-error")
            || crate_name.starts_with("nwipc-capabilities")
            || crate_name.starts_with("nwipc-state")
        {
            for forbidden in [
                "nwipc-protocol",
                "nwipc-runtime",
                "nwipc-peer",
                "nwipc-macos",
            ] {
                if manifest.contains(forbidden) {
                    violations.push(format!("{crate_name}: forbidden dependency on {forbidden}"));
                }
            }
        }

        let mut rust_files = Vec::new();
        collect_extension_files(&crate_root.join("src"), OsStr::new("rs"), &mut rust_files)?;
        if !permits_unsafe {
            for rust_file in rust_files {
                if contains_unsafe_token(&read(&rust_file)?) {
                    violations.push(format!(
                        "{crate_name}: unsafe token outside the allowlist in {}",
                        rust_file.display()
                    ));
                }
            }
        }
    }

    if violations.is_empty() {
        println!("architecture-check: ok");
        Ok(())
    } else {
        Err(violations.join("\n"))
    }
}

fn bundle_manifest(output: Option<String>) -> Result<(), String> {
    let output = output.unwrap_or_else(|| "target/nwipc-bundle-manifest.json".into());
    let output = PathBuf::from(output);
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let manifest = concat!(
        "{\n",
        "  \"schema\": 1,\n",
        "  \"bundleVersion\": \"0.0.0\",\n",
        "  \"layoutVersion\": 0,\n",
        "  \"protocolVersion\": 0,\n",
        "  \"status\": \"scaffold\"\n",
        "}\n"
    );
    fs::write(&output, manifest).map_err(|error| error.to_string())?;
    println!("wrote {}", output.display());
    Ok(())
}

fn bundle_inspect(path: Option<String>) -> Result<(), String> {
    let path = PathBuf::from(path.ok_or("bundle-inspect requires a bundle path")?);
    let plist = path.join("Contents/Info.plist");
    if !plist.is_file() {
        return Err(format!(
            "Unsupported bundle layout: {} is missing",
            plist.display()
        ));
    }
    println!("bundle scaffold: {}", path.display());
    Ok(())
}

fn workspace_root() -> Result<PathBuf, String> {
    let mut directory = env::current_dir().map_err(|error| error.to_string())?;
    loop {
        if directory.join("Cargo.toml").is_file() && directory.join("crates").is_dir() {
            return Ok(directory);
        }
        if !directory.pop() {
            return Err("could not locate the workspace root".into());
        }
    }
}

fn package_name(manifest: &str) -> Option<String> {
    manifest
        .lines()
        .find_map(|line| line.strip_prefix("name = \"")?.strip_suffix('"'))
        .map(str::to_owned)
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
}

fn collect_named_files(
    directory: &Path,
    name: &OsStr,
    output: &mut Vec<PathBuf>,
) -> Result<(), String> {
    collect_files(directory, output, &|path| path.file_name() == Some(name))
}

fn collect_extension_files(
    directory: &Path,
    extension: &OsStr,
    output: &mut Vec<PathBuf>,
) -> Result<(), String> {
    collect_files(directory, output, &|path| {
        path.extension() == Some(extension)
    })
}

fn collect_files(
    directory: &Path,
    output: &mut Vec<PathBuf>,
    predicate: &dyn Fn(&Path) -> bool,
) -> Result<(), String> {
    if !directory.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.is_dir() {
            collect_files(&path, output, predicate)?;
        } else if predicate(&path) {
            output.push(path);
        }
    }
    Ok(())
}

fn contains_unsafe_token(source: &str) -> bool {
    source
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .any(|token| token == "unsafe")
}

#[cfg(test)]
mod tests {
    use super::contains_unsafe_token;

    #[test]
    fn detects_tokens_but_not_substrings() {
        assert!(contains_unsafe_token("unsafe fn map() {}"));
        assert!(!contains_unsafe_token("fn unsafeish() {}"));
    }
}
