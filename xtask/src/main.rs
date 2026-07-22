use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output};

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
        Some("bundle-assemble" | "example-embed") => bundle_assemble(arguments.next()),
        Some("webkit-e2e") => webkit_e2e(),
        _ => Err(
            "usage: cargo xtask <architecture-check|bundle-manifest|bundle-inspect|bundle-assemble|example-embed|webkit-e2e> [path]"
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
    let output = output.unwrap_or_else(|| "target/nwipc-bundle-manifest.txt".into());
    let output = PathBuf::from(output);
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(&output, nwipc_macos_artifact::current_manifest())
        .map_err(|error| error.to_string())?;
    println!("wrote {}", output.display());
    Ok(())
}

fn bundle_inspect(path: Option<String>) -> Result<(), String> {
    let path = PathBuf::from(path.ok_or("bundle-inspect requires a bundle path")?);
    nwipc_macos_artifact::MacosArtifact::inspect(&path).map_err(|error| error.to_string())?;
    println!("bundle valid: {}", path.display());
    Ok(())
}

fn bundle_assemble(binary: Option<String>) -> Result<(), String> {
    let binary = PathBuf::from(binary.ok_or("bundle-assemble requires a bundle binary")?);
    if !binary.is_file() {
        return Err(format!("bundle binary is missing: {}", binary.display()));
    }
    let root = workspace_root()?;
    let bundle = root.join("target/NWIPC.bundle");
    let contents = bundle.join("Contents");
    let executable = contents
        .join("MacOS")
        .join(nwipc_macos_artifact::BUNDLE_EXECUTABLE);
    let resources = contents.join("Resources");
    fs::create_dir_all(executable.parent().expect("executable has parent"))
        .map_err(|error| error.to_string())?;
    fs::create_dir_all(&resources).map_err(|error| error.to_string())?;
    fs::copy(
        root.join("native/macos/bundle/Info.plist"),
        contents.join("Info.plist"),
    )
    .map_err(|error| error.to_string())?;
    fs::copy(&binary, &executable).map_err(|error| error.to_string())?;
    fs::write(
        resources.join(nwipc_macos_artifact::MANIFEST_FILE),
        nwipc_macos_artifact::current_manifest(),
    )
    .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&executable)
            .map_err(|error| error.to_string())?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).map_err(|error| error.to_string())?;
    }
    bundle_inspect(Some(bundle.to_string_lossy().into_owned()))?;
    println!("assembled {}", bundle.display());
    Ok(())
}

#[cfg(target_os = "macos")]
fn webkit_e2e() -> Result<(), String> {
    let root = workspace_root()?;
    let target = root.join("target");
    let work = target.join("webkit-e2e");
    let app = target.join("NWIPC-E2E.app");
    let app_contents = app.join("Contents");
    let app_executable = app_contents.join("MacOS/nwipc-webkit-e2e");
    let embedded_bundle = app_contents.join("PlugIns/NWIPC.bundle");
    let bundle_executable = embedded_bundle
        .join("Contents/MacOS")
        .join(nwipc_macos_artifact::BUNDLE_EXECUTABLE);
    let (identity, timeout) = e2e_environment()?;

    recreate_directory(&work)?;
    if app.exists() {
        fs::remove_dir_all(&app).map_err(|error| error.to_string())?;
    }

    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    run_checked(
        Command::new(cargo)
            .current_dir(&root)
            .args(["build", "-p", "nwipc-macos-bundle-shim"]),
        "build injected bundle shim",
    )?;
    let shim = target.join("debug/libnwipc_macos_bundle_shim.dylib");
    bundle_assemble(Some(shim.to_string_lossy().into_owned()))?;

    let harness = work.join("nwipc-webkit-e2e");
    run_checked(
        Command::new("/usr/bin/xcrun")
            .current_dir(&root)
            .args([
                "clang",
                "-fobjc-arc",
                "-fmodules",
                "-framework",
                "Cocoa",
                "-framework",
                "WebKit",
                "native/macos/appkit/main.m",
                "-o",
            ])
            .arg(&harness),
        "compile AppKit WebKit harness",
    )?;

    fs::create_dir_all(app_contents.join("MacOS")).map_err(|error| error.to_string())?;
    fs::create_dir_all(app_contents.join("PlugIns")).map_err(|error| error.to_string())?;
    fs::copy(&harness, &app_executable).map_err(|error| error.to_string())?;
    fs::copy(
        root.join("native/macos/appkit/Info.plist"),
        app_contents.join("Info.plist"),
    )
    .map_err(|error| error.to_string())?;
    copy_tree(&target.join("NWIPC.bundle"), &embedded_bundle)?;

    require_export(&bundle_executable, "_WKBundleInitialize")?;
    let entitlements = root.join("native/macos/entitlements/nwipc-example.entitlements");
    sign(&embedded_bundle, &identity, &entitlements)?;
    sign(&app, &identity, &entitlements)?;
    verify_signature(&embedded_bundle)?;
    verify_signature(&app)?;
    require_hardened_runtime(&embedded_bundle)?;
    require_hardened_runtime(&app)?;
    require_restricted_entitlements(&embedded_bundle)?;
    require_restricted_entitlements(&app)?;

    let output = Command::new(&app_executable)
        .env("NWIPC_WEBKIT_E2E", "1")
        .env(
            "NWIPC_WEBKIT_E2E_NOTIFICATION",
            format!("dev.nwipc.webkit-e2e.bundle-loaded.{}", std::process::id()),
        )
        .arg(&embedded_bundle)
        .arg(timeout.to_string())
        .output()
        .map_err(|error| format!("launch WebKit E2E harness: {error}"))?;
    fs::write(work.join("stdout.log"), &output.stdout).map_err(|error| error.to_string())?;
    fs::write(work.join("stderr.log"), &output.stderr).map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "WebKit E2E harness failed with {}; logs: {}",
            output.status,
            work.display()
        ));
    }
    nwipc_webkit_testkit::WebKitE2eReport::parse(&String::from_utf8_lossy(&output.stdout))
        .map_err(|error| error.to_string())?;
    print_output(&output);
    println!(
        "webkit-e2e: ok signing={} app={} logs={}",
        if identity == "-" { "ad-hoc" } else { "trusted" },
        app.display(),
        work.display()
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn e2e_environment() -> Result<(String, u64), String> {
    let identity = env::var("NWIPC_CODESIGN_IDENTITY").unwrap_or_else(|_| "-".into());
    let trusted_required =
        env::var("NWIPC_REQUIRE_TRUSTED_SIGNING").is_ok_and(|value| value == "1");
    let timeout = env::var("NWIPC_E2E_TIMEOUT_SECONDS")
        .unwrap_or_else(|_| "20".into())
        .parse::<u64>()
        .map_err(|_| "NWIPC_E2E_TIMEOUT_SECONDS must be an integer")?;
    if !(1..=300).contains(&timeout) {
        return Err("NWIPC_E2E_TIMEOUT_SECONDS must be between 1 and 300".into());
    }
    if trusted_required && identity == "-" {
        return Err("trusted signing requires NWIPC_CODESIGN_IDENTITY".into());
    }
    if identity != "-" {
        require_signing_identity(&identity)?;
    }
    Ok((identity, timeout))
}

#[cfg(not(target_os = "macos"))]
fn webkit_e2e() -> Result<(), String> {
    Err("webkit-e2e requires macOS".into())
}

#[cfg(target_os = "macos")]
fn require_signing_identity(identity: &str) -> Result<(), String> {
    let output = run_output(
        Command::new("/usr/bin/security").args(["find-identity", "-v", "-p", "codesigning"]),
        "list code-signing identities",
    )?;
    let identities = String::from_utf8_lossy(&output.stdout);
    if !identities.contains(identity) {
        return Err(format!("code-signing identity is unavailable: {identity}"));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn sign(path: &Path, identity: &str, entitlements: &Path) -> Result<(), String> {
    run_checked(
        Command::new("/usr/bin/codesign")
            .args(["--force", "--sign"])
            .arg(identity)
            .args(["--options", "runtime", "--timestamp=none", "--entitlements"])
            .arg(entitlements)
            .arg(path),
        "sign hardened artifact",
    )
}

#[cfg(target_os = "macos")]
fn verify_signature(path: &Path) -> Result<(), String> {
    run_checked(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--strict", "--verbose=2"])
            .arg(path),
        "verify code signature",
    )
}

#[cfg(target_os = "macos")]
fn require_hardened_runtime(path: &Path) -> Result<(), String> {
    let output = run_output(
        Command::new("/usr/bin/codesign")
            .args(["--display", "--verbose=4"])
            .arg(path),
        "inspect hardened runtime",
    )?;
    let diagnostics = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !diagnostics
        .lines()
        .any(|line| line.starts_with("CodeDirectory ") && line.contains("runtime"))
    {
        return Err(format!(
            "hardened runtime flag is missing: {}",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn require_restricted_entitlements(path: &Path) -> Result<(), String> {
    let output = run_output(
        Command::new("/usr/bin/codesign")
            .args(["--display", "--entitlements", ":-"])
            .arg(path),
        "inspect signed entitlements",
    )?;
    let entitlements = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for forbidden in [
        "com.apple.security.cs.allow-jit",
        "com.apple.security.cs.allow-unsigned-executable-memory",
        "com.apple.security.cs.disable-library-validation",
        "com.apple.security.get-task-allow",
    ] {
        if entitlements.contains(forbidden) {
            return Err(format!(
                "forbidden E2E entitlement {forbidden}: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn require_export(binary: &Path, symbol: &str) -> Result<(), String> {
    let output = run_output(
        Command::new("/usr/bin/nm").args(["-gU"]).arg(binary),
        "inspect bundle exports",
    )?;
    if !String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.ends_with(symbol))
    {
        return Err(format!("required export is missing: {symbol}"));
    }
    Ok(())
}

fn recreate_directory(path: &Path) -> Result<(), String> {
    if path.exists() {
        fs::remove_dir_all(path).map_err(|error| error.to_string())?;
    }
    fs::create_dir_all(path).map_err(|error| error.to_string())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|error| error.to_string())?;
    for entry in fs::read_dir(source).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let target = destination.join(entry.file_name());
        if entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn run_checked(command: &mut Command, operation: &str) -> Result<(), String> {
    let output = run_output(command, operation)?;
    print_output(&output);
    Ok(())
}

fn run_output(command: &mut Command, operation: &str) -> Result<Output, String> {
    let output = command
        .output()
        .map_err(|error| format!("{operation}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{operation} failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output)
}

fn print_output(output: &Output) {
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
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
