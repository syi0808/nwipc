# Bundle shim

`nwipc-macos-bundle-shim` builds the `WKBundleInitialize` C ABI export. It keeps raw WebKit
objects inside the shim, normalizes lifecycle callbacks through `nwipc-macos-bundle-api`, and
catches Rust panics before they cross the ABI boundary.
