# M6 release gate

A release candidate is the same immutable commit for every gate below. A local pass or a run from
another commit is not substitutable. The release record links the `CI`, `Hardening`, and manually
dispatched `Release Gate` workflow runs plus signed E2E artifacts.

## Required gates

| Gate | Required scope | Workflow/job |
|---|---|---|
| Stable workspace | format, clippy, Rust/TypeScript tests, dependency policy, JSC lifecycle repetition | `CI` |
| Memory safety | Miri, AddressSanitizer, record/bootstrap/layout/fragment/protocol/validation fuzz smoke | `Hardening` |
| Cross architecture | identical fixed-width fixtures and diagnostics contracts on macOS arm64 and x86_64 | `Release Gate / cross-architecture` |
| Actual providers | IOSurface, Darwin/hybrid channel contracts and baseline benchmark on both macOS architectures | `Release Gate / cross-architecture` |
| Production process | trusted-identity signed/hardened WebKit notification, crash, kill, reload, and generation matrix | `Release Gate / signed-webkit-e2e` |

The signing job intentionally uses a restricted self-hosted runner because GitHub-hosted runners do
not contain the private signing identity. Release runs set `signed_e2e=true`; a skipped signing job
is not a release pass. Ad-hoc local E2E remains useful for development but is not trusted release
evidence.

## Evidence

- Cross-architecture jobs assert `uname -m` before testing. Their uploaded logs record OS,
  architecture, provider, ring capacity, payload sizes, latency, throughput, and transport counters.
- Signed E2E retains scenario logs in `target/webkit-e2e/`; the workflow uploads them even on failure.
- Benchmark values are observational. Compare regressions only on the same runner class and power
  profile; M6 does not impose a cross-machine numeric threshold.
- Diagnostics snapshots use [schema v2](diagnostics-schema.md). Failure review records the session,
  generation, backend, state, structured failure, and cleanup status without attaching payload or
  native-handle data.

## Support boundary

Portable layout/protocol behavior is released for Linux/macOS x86_64 and arm64. IOSurface and
Darwin/hybrid provider contracts are gated on macOS 15 arm64 and Intel runners. Production WebKit
support remains the explicit allowlisted macOS 26.5.2 arm64 build until another signed process
matrix is recorded; provider coverage alone does not expand the WebKit support claim.
