use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use nwipc_channel_core::{ChannelEvent, in_process_channel};
use nwipc_error::ErrorCode;

#[cfg(target_os = "macos")]
use std::io::Write;
#[cfg(target_os = "macos")]
use std::process::{Command, Output};

const UNSAFE_CRATES: &[&str] = &[
    "nwipc-atomic",
    "nwipc-memory-iosurface",
    "nwipc-memory-mach",
    "nwipc-signal-darwin",
    "nwipc-signal-mach",
    "nwipc-mach-transfer",
    "nwipc-mach-rendezvous",
    "nwipc-renderer-jsc",
    "nwipc-macos-spi",
    "nwipc-macos-bundle-shim",
];

const UNSAFE_AUDIT_BASELINE: &[(&str, usize)] = &[
    ("crates/data-plane/nwipc-atomic", 5),
    ("crates/memory/nwipc-memory-iosurface", 28),
    ("crates/memory/nwipc-memory-mach", 60),
    ("crates/signal/nwipc-signal-darwin", 5),
    ("crates/signal/nwipc-signal-mach", 63),
    ("crates/platform/macos/nwipc-mach-transfer", 16),
    ("crates/platform/macos/nwipc-mach-rendezvous", 29),
    ("crates/renderer/nwipc-renderer-jsc", 76),
    ("crates/platform/macos/nwipc-macos-spi", 4),
    ("crates/platform/macos/nwipc-macos-bundle-shim", 33),
];

fn main() -> ExitCode {
    let mut arguments = env::args().skip(1);
    let result = match arguments.next().as_deref() {
        Some("architecture-check") => architecture_check(),
        Some("unsafe-audit") => unsafe_audit(),
        Some("hardening-check") => hardening_check(),
        Some("benchmark") => benchmark(arguments.next()),
        Some("bundle-manifest") => bundle_manifest(arguments.next()),
        Some("bundle-inspect") => bundle_inspect(arguments.next()),
        Some("bundle-assemble" | "example-embed") => bundle_assemble(arguments.next()),
        Some("webkit-e2e") => webkit_e2e(),
        _ => Err(
            "usage: cargo xtask <architecture-check|unsafe-audit|hardening-check|benchmark|bundle-manifest|bundle-inspect|bundle-assemble|example-embed|webkit-e2e> [path]"
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

fn unsafe_audit() -> Result<(), String> {
    let root = workspace_root()?;
    let mut failures = Vec::new();
    for (relative, expected) in UNSAFE_AUDIT_BASELINE {
        let mut sources = Vec::new();
        collect_extension_files(
            &root.join(relative).join("src"),
            OsStr::new("rs"),
            &mut sources,
        )?;
        let actual = sources.iter().try_fold(0, |count, source| {
            read(source).map(|contents| count + unsafe_token_count(&contents))
        })?;
        if actual != *expected {
            failures.push(format!(
                "{relative}: unsafe token count changed from audited {expected} to {actual}"
            ));
        }
    }
    let audit = read(&root.join("docs/security.md"))?;
    for crate_name in UNSAFE_CRATES {
        if !audit.contains(crate_name) {
            failures.push(format!("{crate_name}: missing from docs/security.md audit"));
        }
    }
    if failures.is_empty() {
        println!("unsafe-audit: ok");
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

fn hardening_check() -> Result<(), String> {
    architecture_check()?;
    unsafe_audit()?;
    let root = workspace_root()?;
    for required in [
        "docs/security.md",
        "docs/support-matrix.md",
        "docs/diagnostics-schema.md",
        "docs/release-gate.md",
        "fuzz/Cargo.toml",
        "fuzz/fuzz_targets/record.rs",
        "fuzz/fuzz_targets/bootstrap.rs",
        "fuzz/fuzz_targets/layout.rs",
        "fuzz/fuzz_targets/fragment.rs",
        "fuzz/fuzz_targets/crypto.rs",
    ] {
        if !root.join(required).is_file() {
            return Err(format!("missing hardening artifact: {required}"));
        }
    }
    println!("hardening-check: ok");
    Ok(())
}

fn benchmark(output: Option<String>) -> Result<(), String> {
    if cfg!(debug_assertions) {
        return Err("benchmark requires `cargo run --release -p xtask -- benchmark`".into());
    }
    let root = workspace_root()?;
    let output = output.unwrap_or_else(|| "target/hardening/benchmark.md".into());
    let output = root.join(output);
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let scale = env::var("NWIPC_BENCH_SCALE")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| "NWIPC_BENCH_SCALE must be a positive integer")?
        .unwrap_or(1);
    if scale == 0 {
        return Err("NWIPC_BENCH_SCALE must be a positive integer".into());
    }
    let cases = [
        (64_usize, 20_000_usize),
        (1024, 10_000),
        (16 * 1024, 2_000),
        (1024 * 1024, 32),
    ];
    let mut report = format!(
        "# NWIPC benchmark baseline\n\n- OS: {}\n- Architecture: {}\n- Build: release\n- Provider: in-process SPSC\n- Scale: {scale}\n\n| Payload | Iterations | Mean round trip | Throughput | Saturation |\n|---:|---:|---:|---:|---:|\n",
        env::consts::OS,
        env::consts::ARCH,
    );
    for (payload_length, base_iterations) in cases {
        let iterations = base_iterations
            .checked_mul(scale)
            .ok_or("benchmark iteration overflow")?;
        let row = benchmark_case(payload_length, iterations)?;
        report.push_str(&row);
    }
    fs::write(&output, report).map_err(|error| error.to_string())?;
    println!("wrote {}", output.display());
    Ok(())
}

fn benchmark_case(payload_length: usize, iterations: usize) -> Result<String, String> {
    let record_length = payload_length
        .checked_add(31)
        .map(|length| length & !7)
        .ok_or("benchmark record length overflow")?;
    let capacity = u32::try_from(
        record_length
            .checked_mul(8)
            .ok_or("benchmark capacity overflow")?,
    )
    .map_err(|_| "benchmark capacity is not representable")?;
    let maximum_inline =
        u32::try_from(payload_length).map_err(|_| "payload is not representable")?;
    let iterations_u32 =
        u32::try_from(iterations).map_err(|_| "iteration count is not representable")?;
    let (mut sender, mut receiver) = in_process_channel(
        capacity,
        maximum_inline,
        capacity / 4,
        capacity - capacity / 4,
    )
    .map_err(|error| error.to_string())?;
    let payload = vec![0xa5; payload_length];
    let start = Instant::now();
    for _ in 0..iterations {
        sender.send(&payload).map_err(|error| error.to_string())?;
        match receiver.receive().map_err(|error| error.to_string())? {
            Some(ChannelEvent::Message(message)) if message.len() == payload_length => {}
            _ => return Err("benchmark received an unexpected event".into()),
        }
    }
    let elapsed = start.elapsed();
    let mean_ns = elapsed.as_nanos() / iterations as u128;
    let bytes_per_second =
        (f64::from(maximum_inline) * f64::from(iterations_u32)) / elapsed.as_secs_f64();

    let (mut saturation_sender, _saturation_receiver) = in_process_channel(
        capacity,
        maximum_inline,
        capacity / 4,
        capacity - capacity / 4,
    )
    .map_err(|error| error.to_string())?;
    let mut messages = 0_u64;
    let mut buffered = 0_u32;
    loop {
        match saturation_sender.send(&payload) {
            Ok(sent) => {
                messages += 1;
                buffered = sent.buffered_amount;
            }
            Err(error) if error.code() == ErrorCode::Backpressured => break,
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(format!(
        "| {payload_length} B | {iterations} | {mean_ns} ns | {:.2} MiB/s | {messages} msg / {buffered} B |\n",
        bytes_per_second / (1024.0 * 1024.0),
    ))
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

        check_runtime_framework_dependency(&crate_name, &manifest, &mut violations);

        if matches!(crate_name.as_str(), "nwipc-wry" | "nwipc-tauri") {
            check_framework_adapter(&crate_name, &manifest, &mut violations);
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

    let bundle_shim_manifest =
        read(&root.join("crates/platform/macos/nwipc-macos-bundle-shim/Cargo.toml"))?;
    if !bundle_shim_manifest.contains("nwipc-macos-transport.workspace = true")
        || bundle_shim_manifest.contains("nwipc-memory-iosurface")
        || bundle_shim_manifest.contains("nwipc-memory-api")
    {
        violations.push(
            "nwipc-macos-bundle-shim: WebKit production path must use macOS transport without raw memory access"
                .into(),
        );
    }
    check_mach_transfer(&root, &mut violations)?;
    check_experimental_rendezvous(&root, &mut violations)?;
    let appkit_harness = read(&root.join("native/macos/appkit/main.m"))?;
    for forbidden in ["IOSurface", "EchoFrame", "ECHO_PAYLOAD", "MappedRegion"] {
        if appkit_harness.contains(forbidden) {
            violations.push(format!(
                "native AppKit host: payload-path token `{forbidden}` is forbidden"
            ));
        }
    }

    if violations.is_empty() {
        println!("architecture-check: ok");
        Ok(())
    } else {
        Err(violations.join("\n"))
    }
}

fn check_experimental_rendezvous(root: &Path, violations: &mut Vec<String>) -> Result<(), String> {
    let production_transport =
        read(&root.join("crates/platform/macos/nwipc-macos-transport/Cargo.toml"))?;
    if production_transport.contains("nwipc-mach-rendezvous") {
        violations.push(
            "nwipc-macos-transport: experimental Mach rendezvous must not enter production".into(),
        );
    }
    Ok(())
}

fn check_mach_transfer(root: &Path, violations: &mut Vec<String>) -> Result<(), String> {
    let source = read(&root.join("crates/platform/macos/nwipc-mach-transfer/src/lib.rs"))?;
    for forbidden in ["bootstrap_register", "bootstrap_look_up", "bootstrap_port"] {
        if source.contains(forbidden) {
            violations.push(format!(
                "nwipc-mach-transfer: global rendezvous token `{forbidden}` is forbidden"
            ));
        }
    }
    Ok(())
}

fn check_runtime_framework_dependency(
    crate_name: &str,
    manifest: &str,
    violations: &mut Vec<String>,
) {
    if crate_name.starts_with("nwipc-runtime")
        && ["wry", "tauri"]
            .iter()
            .any(|dependency| manifest.contains(dependency))
    {
        violations.push(format!(
            "{crate_name}: runtime must not depend on WebView frameworks"
        ));
    }
}

fn check_framework_adapter(crate_name: &str, manifest: &str, violations: &mut Vec<String>) {
    for forbidden in [
        "nwipc-ring-",
        "nwipc-channel-",
        "nwipc-memory-",
        "nwipc-peer =",
    ] {
        if manifest.contains(forbidden) {
            violations.push(format!(
                "{crate_name}: framework adapter must not own payload dependency {forbidden}"
            ));
        }
    }
    if !manifest.contains("nwipc.workspace = true")
        || !manifest.contains("nwipc-macos-host.workspace = true")
    {
        violations.push(format!(
            "{crate_name}: framework adapter must use the public facade and macOS host"
        ));
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
    let spi = nwipc_macos_spi::MacosSpi::initialize().map_err(|error| error.to_string())?;
    println!(
        "webkit-e2e: macos-support={:?} version={}.{}.{} architecture={:?}",
        spi.support(),
        spi.version().major,
        spi.version().minor,
        spi.version().patch,
        spi.architecture()
    );
    let root = workspace_root()?;
    let target = root.join("target");
    let work = target.join("webkit-e2e");
    let (identity, timeout) = e2e_environment()?;
    recreate_directory(&work)?;
    let artifacts = prepare_e2e_artifacts(&root, &target, &work)?;
    sign_e2e_artifacts(&root, &artifacts, &identity)?;
    run_e2e_processes(&artifacts, &work, timeout)?;
    println!(
        "webkit-e2e: ok signing={} app={} logs={}",
        if identity == "-" { "ad-hoc" } else { "trusted" },
        artifacts.app.display(),
        work.display()
    );
    Ok(())
}

#[cfg(target_os = "macos")]
struct E2eArtifacts {
    app: PathBuf,
    app_executable: PathBuf,
    peer_executable: PathBuf,
    embedded_bundle: PathBuf,
}

#[cfg(target_os = "macos")]
fn prepare_e2e_artifacts(root: &Path, target: &Path, work: &Path) -> Result<E2eArtifacts, String> {
    let app = target.join("NWIPC-E2E.app");
    if app.exists() {
        fs::remove_dir_all(&app).map_err(|error| error.to_string())?;
    }
    let app_contents = app.join("Contents");
    let artifacts = E2eArtifacts {
        app,
        app_executable: app_contents.join("MacOS/nwipc-webkit-e2e"),
        peer_executable: app_contents.join("Helpers/nwipc-webkit-e2e-peer"),
        embedded_bundle: app_contents.join("PlugIns/NWIPC.bundle"),
    };
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    run_checked(
        Command::new(&cargo).current_dir(root).args([
            "build",
            "-p",
            "nwipc-macos-bundle-shim",
            "--features",
            "e2e-fault-injection",
        ]),
        "build injected bundle shim",
    )?;
    run_checked(
        Command::new(cargo).current_dir(root).args([
            "build",
            "-p",
            "nwipc-native-peer-example",
            "--bin",
            "nwipc-webkit-e2e-peer",
        ]),
        "build native-peer E2E helper",
    )?;
    let shim = target.join("debug/libnwipc_macos_bundle_shim.dylib");
    bundle_assemble(Some(shim.to_string_lossy().into_owned()))?;
    let harness = work.join("nwipc-webkit-e2e");
    run_checked(
        Command::new("/usr/bin/xcrun")
            .current_dir(root)
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
    fs::create_dir_all(app_contents.join("Helpers")).map_err(|error| error.to_string())?;
    fs::create_dir_all(app_contents.join("PlugIns")).map_err(|error| error.to_string())?;
    fs::copy(&harness, &artifacts.app_executable).map_err(|error| error.to_string())?;
    fs::copy(
        target.join("debug/nwipc-webkit-e2e-peer"),
        &artifacts.peer_executable,
    )
    .map_err(|error| error.to_string())?;
    let app_plist = fs::read_to_string(root.join("native/macos/appkit/Info.plist"))
        .map_err(|error| error.to_string())?
        .replace(
            "dev.nwipc.webkit-e2e",
            &format!("dev.nwipc.webkit-e2e.run-{}", std::process::id()),
        );
    fs::write(app_contents.join("Info.plist"), app_plist).map_err(|error| error.to_string())?;
    copy_tree(&target.join("NWIPC.bundle"), &artifacts.embedded_bundle)?;
    Ok(artifacts)
}

#[cfg(target_os = "macos")]
fn sign_e2e_artifacts(root: &Path, artifacts: &E2eArtifacts, identity: &str) -> Result<(), String> {
    let bundle_executable = artifacts
        .embedded_bundle
        .join("Contents/MacOS")
        .join(nwipc_macos_artifact::BUNDLE_EXECUTABLE);
    require_export(&bundle_executable, "_WKBundleInitialize")?;
    let entitlements = root.join("native/macos/entitlements/nwipc-example.entitlements");
    sign(&artifacts.peer_executable, identity, &entitlements)?;
    sign(&artifacts.embedded_bundle, identity, &entitlements)?;
    sign(&artifacts.app, identity, &entitlements)?;
    for artifact in [
        &artifacts.peer_executable,
        &artifacts.embedded_bundle,
        &artifacts.app,
    ] {
        verify_signature(artifact)?;
        require_hardened_runtime(artifact)?;
        require_restricted_entitlements(artifact)?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn run_e2e_processes(artifacts: &E2eArtifacts, work: &Path, timeout: u64) -> Result<(), String> {
    let mut nwipc = nwipc::Nwipc::initialize().map_err(|error| error.to_string())?;
    let mut normal_session = nwipc.create_session().map_err(|error| error.to_string())?;
    let normal_output = run_e2e_scenario(
        artifacts,
        work,
        timeout,
        &mut normal_session,
        "normal",
        "normal",
        ScenarioExpectation::Success("production-transport=ok"),
    )?;
    nwipc
        .observe_external_connection(&normal_session)
        .map_err(|error| error.to_string())?;
    nwipc
        .close(&normal_session)
        .map_err(|error| error.to_string())?;

    for fault in [
        "notification-dropped",
        "notification-duplicate",
        "notification-delayed",
    ] {
        let mut session = nwipc.create_session().map_err(|error| error.to_string())?;
        run_e2e_scenario(
            artifacts,
            work,
            timeout,
            &mut session,
            fault,
            "normal",
            ScenarioExpectation::Success("production-transport=ok"),
        )?;
        nwipc
            .observe_external_connection(&session)
            .map_err(|error| error.to_string())?;
        nwipc.close(&session).map_err(|error| error.to_string())?;
    }

    let mut crash_session = nwipc.create_session().map_err(|error| error.to_string())?;
    let logical_session = crash_session.id();
    for (fault, peer_mode, expectation) in [
        (
            "writer-before-commit",
            "writer-before-commit",
            ScenarioExpectation::Success("writer-before-commit-hidden=ok"),
        ),
        (
            "writer-after-commit",
            "writer-after-commit",
            ScenarioExpectation::Success("writer-after-commit-visible=ok"),
        ),
        ("peer-kill", "peer-kill", ScenarioExpectation::PeerCrash),
    ] {
        let old_generation = crash_session.generation();
        run_e2e_scenario(
            artifacts,
            work,
            timeout,
            &mut crash_session,
            fault,
            peer_mode,
            expectation,
        )?;
        nwipc
            .observe_external_connection(&crash_session)
            .map_err(|error| error.to_string())?;
        crash_session = nwipc
            .replace_renderer(&crash_session)
            .map_err(|error| error.to_string())?;
        if crash_session.id() != logical_session || crash_session.generation() == old_generation {
            return Err("endpoint crash did not replace the generation".into());
        }
    }
    nwipc
        .close(&crash_session)
        .map_err(|error| error.to_string())?;
    let report = "webkit-e2e: initial-load=ok production-transport=ok boundaries=ok backpressure=ok replacement-process=ok hardened-process=ok notification-faults=ok writer-crash=ok peer-kill=ok generation-replacement=ok\n";
    nwipc_webkit_testkit::WebKitE2eReport::parse(report).map_err(|error| error.to_string())?;
    print_output(&normal_output);
    print!("{report}");
    Ok(())
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
enum ScenarioExpectation {
    Success(&'static str),
    PeerCrash,
}

#[cfg(target_os = "macos")]
fn run_e2e_scenario(
    artifacts: &E2eArtifacts,
    work: &Path,
    timeout: u64,
    session: &mut nwipc::Session,
    fault: &str,
    peer_mode: &str,
    expectation: ScenarioExpectation,
) -> Result<Output, String> {
    let mut renderer_bootstrap = Vec::new();
    session
        .write_renderer_bootstrap(&mut renderer_bootstrap)
        .map_err(|error| error.to_string())?;
    let renderer_bootstrap = encode_hex(&renderer_bootstrap);
    let process_id = format!("{}.{}", std::process::id(), session.generation().get());
    let mut peer_command = Command::new(&artifacts.peer_executable);
    peer_command
        .env("NWIPC_WEBKIT_E2E_PEER_MODE", peer_mode)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    session.peer_environment().apply(&mut peer_command);
    let mut peer = peer_command
        .spawn()
        .map_err(|error| format!("launch native-peer E2E helper: {error}"))?;
    session
        .write_peer_bootstrap(
            peer.stdin
                .as_mut()
                .ok_or("native-peer bootstrap stdin is unavailable")?,
        )
        .map_err(|error| error.to_string())?;
    peer.stdin
        .take()
        .ok_or("native-peer bootstrap stdin is unavailable")?
        .flush()
        .map_err(|error| format!("flush native-peer bootstrap: {error}"))?;
    let output = Command::new(&artifacts.app_executable)
        .env("NWIPC_WEBKIT_E2E", "1")
        .env(
            "NWIPC_WEBKIT_E2E_NOTIFICATION",
            format!("dev.nwipc.webkit-e2e.bundle-loaded.{process_id}"),
        )
        .env(
            nwipc_webkit_testkit::TRANSPORT_NOTIFICATION_ENV,
            format!("dev.nwipc.webkit-e2e.transport.{process_id}"),
        )
        .env(
            nwipc_webkit_testkit::RENDERER_BOOTSTRAP_ENV,
            &renderer_bootstrap,
        )
        .env("NWIPC_E2E_TIMEOUT_SECONDS", timeout.to_string())
        .env("NWIPC_WEBKIT_E2E_FAULT", fault)
        .arg(&artifacts.embedded_bundle)
        .arg(timeout.to_string())
        .output();
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            let _ = peer.kill();
            let peer_output = peer
                .wait_with_output()
                .map_err(|wait_error| format!("reap native-peer E2E helper: {wait_error}"))?;
            write_peer_logs(work, fault, &peer_output)?;
            return Err(format!("launch WebKit E2E harness: {error}"));
        }
    };
    write_app_logs(work, fault, &output)?;
    if !output.status.success() && !matches!(expectation, ScenarioExpectation::PeerCrash) {
        let _ = peer.kill();
        let peer_output = peer
            .wait_with_output()
            .map_err(|error| format!("reap native-peer E2E helper: {error}"))?;
        write_peer_logs(work, fault, &peer_output)?;
        return Err(format!(
            "WebKit E2E harness failed with {}; logs: {}",
            output.status,
            work.display()
        ));
    }
    let peer_output = peer
        .wait_with_output()
        .map_err(|error| format!("wait native-peer E2E helper: {error}"))?;
    write_peer_logs(work, fault, &peer_output)?;
    validate_e2e_scenario(fault, work, &output, &peer_output, expectation)?;
    Ok(output)
}

#[cfg(target_os = "macos")]
fn validate_e2e_scenario(
    fault: &str,
    work: &Path,
    output: &Output,
    peer_output: &Output,
    expectation: ScenarioExpectation,
) -> Result<(), String> {
    match expectation {
        ScenarioExpectation::Success(marker) => {
            if !output.status.success()
                || !peer_output.status.success()
                || !String::from_utf8_lossy(&peer_output.stdout).contains(marker)
            {
                return Err(format!(
                    "WebKit E2E scenario {fault} failed app={} peer={}; logs: {}",
                    output.status,
                    peer_output.status,
                    work.display()
                ));
            }
        }
        ScenarioExpectation::PeerCrash => {
            if output.status.success()
                || peer_output.status.success()
                || !String::from_utf8_lossy(&peer_output.stdout)
                    .contains("handshake-before-kill=ok")
            {
                return Err(format!(
                    "WebKit E2E peer-kill scenario unexpectedly succeeded; logs: {}",
                    work.display()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn write_app_logs(work: &Path, scenario: &str, output: &Output) -> Result<(), String> {
    fs::write(work.join(format!("{scenario}-stdout.log")), &output.stdout)
        .map_err(|error| error.to_string())?;
    fs::write(work.join(format!("{scenario}-stderr.log")), &output.stderr)
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
fn write_peer_logs(work: &Path, scenario: &str, output: &Output) -> Result<(), String> {
    fs::write(
        work.join(format!("{scenario}-peer-stdout.log")),
        &output.stdout,
    )
    .map_err(|error| error.to_string())?;
    fs::write(
        work.join(format!("{scenario}-peer-stderr.log")),
        &output.stderr,
    )
    .map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
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

#[cfg(target_os = "macos")]
fn recreate_directory(path: &Path) -> Result<(), String> {
    if path.exists() {
        fs::remove_dir_all(path).map_err(|error| error.to_string())?;
    }
    fs::create_dir_all(path).map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
fn run_checked(command: &mut Command, operation: &str) -> Result<(), String> {
    let output = run_output(command, operation)?;
    print_output(&output);
    Ok(())
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
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
    unsafe_token_count(source) != 0
}

fn unsafe_token_count(source: &str) -> usize {
    source
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .filter(|token| *token == "unsafe")
        .count()
}

#[cfg(test)]
mod tests {
    use super::{contains_unsafe_token, unsafe_token_count};

    #[test]
    fn detects_tokens_but_not_substrings() {
        assert!(contains_unsafe_token("unsafe fn map() {}"));
        assert!(!contains_unsafe_token("fn unsafeish() {}"));
        assert_eq!(unsafe_token_count("unsafe { unsafe_call() }"), 1);
    }
}
