# NWIPC

NWIPC is an experimental, host-relay-free shared-memory IPC transport for a macOS
`WKWebView` renderer and a native peer process.

The repository includes its protocol wire foundation, in-process SPSC data plane, native
two-process bootstrap, macOS memory/signal providers, and renderer runtime. The renderer slice
provides generation-scoped Rust port state, a JavaScriptCore binding, TypeScript clients, and a
deterministic mock binding without requiring WebKit.

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
