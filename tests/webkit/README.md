# WebKit tests

Portable contract tests live beside the relevant crates and cover:

- strict SPI allowlisting and missing-selector failure;
- property-list bootstrap field validation and ordered provider attachment;
- main-frame/normal-world binding eligibility;
- document invalidation and page teardown;
- renderer generation replacement and stale-event rejection;
- artifact manifest/layout inspection and the `WKBundleInitialize` panic boundary.

The signed, hardened, real-`WKWebView` smoke matrix runs with `cargo xtask webkit-e2e`. It validates
initial bundle load, the production protocol/channel boundary and backpressure matrix with a signed
native peer, and forced `WebContent` process replacement. Unsupported OS/build combinations fail closed at the SPI probe
rather than using an uninstrumented WebView. Trusted-identity signing is enabled through
`NWIPC_CODESIGN_IDENTITY`; production ring/record handshake and backpressure remain later work.
