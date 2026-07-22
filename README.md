# NWIPC

NWIPC is an experimental, host-relay-free shared-memory IPC transport for a macOS
`WKWebView` renderer and a native peer process.

The repository includes its protocol wire foundation, an in-process SPSC data plane, and a native
two-process bootstrap slice. The bootstrap codec rejects partial, oversized, duplicate, stale, and
unknown-required input; the process harness verifies HELLO/ACK, binary echo, and crash cleanup. OS
shared-memory, signal, and renderer providers still return a typed `Unsupported` error instead of
silently succeeding.

## Development

```sh
cargo xtask architecture-check
cargo test --workspace
corepack pnpm install --frozen-lockfile
corepack pnpm typecheck
corepack pnpm test
```

The design and implementation sequence are documented in
[`docs/2차구현계획.md`](docs/2차구현계획.md).

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
