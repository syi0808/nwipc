# WebKit tests

Portable contract tests live beside the relevant crates and cover:

- strict SPI allowlisting and missing-selector failure;
- property-list bootstrap field validation and ordered provider attachment;
- main-frame/normal-world binding eligibility;
- document invalidation and page teardown;
- renderer generation replacement and stale-event rejection;
- artifact manifest/layout inspection and the `WKBundleInitialize` panic boundary.

The signed, hardened, real-`WKWebView` process matrix remains a macOS packaging test. Unsupported
OS/build combinations fail closed at the SPI probe rather than using an uninstrumented WebView.
