# macOS E2E entitlements

`nwipc-example.entitlements` is intentionally empty. The E2E app and injected bundle are signed
with hardened runtime enabled without disabling library validation or allowing JIT/unsigned
executable memory.

Signing identity is injected with `NWIPC_CODESIGN_IDENTITY`. Never commit a certificate name,
Team ID, keychain path, password, or provisioning profile.
