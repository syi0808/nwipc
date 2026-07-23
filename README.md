# NWIPC

NWIPC is an experimental, host-relay-free shared-memory IPC transport for a macOS
`WKWebView` renderer and a native peer process.

The repository includes its protocol wire foundation, in-process SPSC data plane, native
two-process bootstrap, macOS memory/signal providers, renderer runtime, and the macOS WebKit
control plane. The data plane supports bounded, atomically published fragmentation with
single-message reassembly. The WebKit slice provides a runtime viability and verification probe, injected-bundle lifecycle and
panic boundaries, strict renderer bootstrap attachment, generation replacement, and deterministic
bundle assembly/inspection. Untested but viable OS/build combinations run best effort, while
logically incompatible configurations are reported explicitly.

The `nwipc` facade now owns production configuration, generation-scoped session resources,
peer bootstrap, renderer transport creation, close, and redacted diagnostics. Its macOS path attaches
the same IOSurface/Darwin channel in both public endpoints; inherited stdin carries bootstrap only.
See `examples/native-peer/tests/process.rs` for the complete two-process public API path.

Native peers can remain runtime-neutral with `nwipc-peer-async`, or opt into Tokio readiness and
bounded correctness polling through `nwipc-peer-tokio`. Both adapters preserve the synchronous
core's nonblocking send/receive and typed backpressure contract without owning a task or thread.

`nwipc-wry` and `nwipc-tauri` merge the verified macOS host plan before WebView creation and route
only framework identity, lifecycle, cleanup, and redacted diagnostics. Neither adapter relays
application payload through Wry/Tauri IPC or owns native-peer process policy.

## Development

```sh
cargo xtask architecture-check
cargo xtask hardening-check
cargo test --workspace
corepack pnpm install --frozen-lockfile
corepack pnpm typecheck
corepack pnpm test
```

Phase 7 threat/unsafe audit, fuzz/sanitizer scope, benchmark procedure, and supported runtime
combinations are documented in [`docs/security.md`](docs/security.md) and
[`docs/support-matrix.md`](docs/support-matrix.md).

On a runtime-compatible macOS release, run the real ad-hoc signed hardened `WKWebView` process smoke,
including direct renderer↔native-peer `IOSurface` binary echo:

```sh
cargo xtask webkit-e2e
```

Trusted identity configuration and failure semantics are documented in
[`docs/webkit-e2e.md`](docs/webkit-e2e.md).

The design and implementation sequence are documented in
[`docs/2차구현계획.md`](docs/2차구현계획.md).

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
