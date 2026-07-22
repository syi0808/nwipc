# NWIPC

NWIPC is an experimental, host-relay-free shared-memory IPC transport for a macOS
`WKWebView` renderer and a native peer process.

The repository includes its protocol wire foundation, in-process SPSC data plane, native
two-process bootstrap, macOS memory/signal providers, renderer runtime, and the macOS WebKit
control plane. The WebKit slice provides a fail-closed SPI probe, injected-bundle lifecycle and
panic boundaries, strict renderer bootstrap attachment, generation replacement, and deterministic
bundle assembly/inspection. Unsupported OS/build combinations are reported explicitly.

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

On an allowlisted macOS release, run the real ad-hoc signed hardened `WKWebView` process smoke,
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
