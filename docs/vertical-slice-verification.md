# Vertical slice verification matrix

This matrix maps every completion criterion in the roadmap to a committed test, required CI job,
and support boundary. A release claim also requires the immutable `release-record` artifact defined
in [release-gate.md](release-gate.md); the presence of a test or workflow definition alone is not
execution evidence.

| # | Contract/process evidence | Required job | Support boundary |
|---|---|---|---|
| 1 | `cargo test --workspace` on a non-macOS runner | `CI / rust-core (ubuntu)` | Portable core on Linux/macOS |
| 2 | `golden_prefix_is_architecture_independent`, `data_record_matches_the_external_golden_fixture`, `negotiates_golden_hello_and_ack`, `round_trips_canonical_envelope` | `CI / cross-architecture-fixtures (arm64, x86_64)` | macOS 15 arm64/x86_64 |
| 3 | `exchanges_bidirectional_fifo_messages`, `backpressure_recovers_with_one_writable_edge`, fragmentation and ring writer crash tests | `CI / rust-core`, `Hardening` | Portable data plane |
| 4 | protocol arbitrary-frame test and record/bootstrap/layout/protocol/validation fuzz targets | `Hardening / fuzz-smoke`, `address-sanitizer` | Linux x86_64 sanitizer/fuzz |
| 5 | IOSurface/Mach `two_process_*visibility`와 Darwin/Mach `two_process_notification_delivery` | `Release Gate / cross-architecture` | macOS 15 arm64/x86_64 providers |
| 6 | `dropped_primary_is_recovered_by_bounded_poll` and common transport contract | `Release Gate / cross-architecture` | Darwin/hybrid providers |
| 7 | runtime multi-session, replacement, failed-generation, and cleanup tests | `CI / rust-core` | Portable runtime |
| 8 | `public_endpoints_use_bootstrap_pipe_only_for_production_echo` | `CI / rust-core` | Native peer on Linux/macOS |
| 9 | `nwipc-renderer-core` contract, FIFO, invalidation, and close tests | `CI / rust-core` | Portable renderer core |
| 10 | `teardown_blocks_stale_callbacks_and_releases_handlers`, repeated lifecycle test | `CI / jsc-lifecycle` | macOS arm64 JSC |
| 11 | `only_main_normal_world_is_eligible`, subframe/reload bundle test | `CI / rust-core` | Allowlisted WebKit build |
| 12 | `public_facade_connects_renderer_and_peer_without_payload_stream` | `CI / rust-core (macOS)` | IOSurface + Darwin/hybrid |
| 13 | public facade contract and signed production-transport scenario | `CI / rust-core (macOS)`, `Release Gate / signed-webkit-e2e` | Allowlisted WebKit build |
| 14 | signed boundary/backpressure scenario | trusted local E2E; release는 `Release Gate / signed-webkit-e2e` | macOS 26.2 arm64 |
| 15 | bootstrap-only process test and host data-plane dependency checks | `CI / architecture`, `Release Gate / signed-webkit-e2e` | Production WebKit path |
| 16 | signed peer-kill and generation-replacement scenarios | trusted local E2E; release는 `Release Gate / signed-webkit-e2e` | macOS 26.2 arm64 |
| 17 | signed notification and writer-crash scenarios | trusted local E2E; release는 `Release Gate / signed-webkit-e2e` | macOS 26.2 arm64 |
| 18 | schema redaction/compatibility tests and public generation failure/cleanup snapshot test | `CI / rust-core`, `Release Gate / signed-webkit-e2e` | Diagnostics schema v2 |
| 19 | typed unsupported/fail-closed tests and architecture policy | `CI / rust-core`, `CI / architecture` | Documented support matrix |
| 20 | format, clippy, Rust/TypeScript tests, dependency policy, Miri, ASan, and fuzz smoke | `CI`, `Hardening` | Same candidate commit |
| 21 | This matrix plus the immutable run-link and artifact manifest | `Release Gate / release-evidence` | Same candidate commit |

The trusted signed E2E artifact retains scenario logs. Failure review must associate each failed or
replaced generation with schema-v2 session/generation/backend/state/failure/cleanup fields; payload,
secret, native handle, and provider-source data must remain absent.
