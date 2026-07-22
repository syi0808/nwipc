# Process tests

The executable process tests live in `examples/native-peer/tests/process.rs` so Cargo can provide
the built native-peer path through `CARGO_BIN_EXE_nwipc-native-peer-example`. They cover binary
echo, graceful cleanup, abrupt child reaping, and stale-generation rejection.
