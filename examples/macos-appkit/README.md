# macOS AppKit example

This executable exercises the fail-closed AppKit configuration boundary. The system probe checks
the running macOS release and required Objective-C methods; an untested or incompatible build is
reported as unsupported rather than creating a relayed or uninstrumented WebView.

Build and assemble the injected bundle first:

```sh
cargo build -p nwipc-macos-bundle-shim
cargo xtask bundle-assemble target/debug/libnwipc_macos_bundle_shim.dylib
cargo run -p nwipc-example-macos-appkit -- target/NWIPC.bundle
```

The real signed/hardened AppKit process smoke is automated separately:

```sh
cargo xtask webkit-e2e
```

See [`docs/webkit-e2e.md`](../../docs/webkit-e2e.md) for trusted signing and failure semantics.
