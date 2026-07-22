# Diagnostics schema v2

`nwipc-diagnostics` defines the public operational snapshot. It is an in-process Rust contract,
not a wire protocol: adapters that serialize it must preserve the field meanings below and publish
their own encoding/content type. Payload bytes, bootstrap secrets, provider names, native handles,
notification tokens, mapping addresses, and provider source errors are not fields in this schema.

## Compatibility

- `schema_version` identifies the complete field and enum contract.
- `minimum_compatible_schema_version` is the oldest reader that can interpret every required field.
- A reader accepts a snapshot only when
  `reader_version >= schema_version && reader_version >= minimum_compatible_schema_version`.
- Adding a required field, changing a counter meaning, removing/renaming a field, or changing an
  enum meaning increments `schema_version`. The minimum version also advances when an older reader
  would have to guess a value.
- Version 2 is intentionally incompatible with version 1 because cleanup outcome, structured
  failure, and wake-up counters are required. Unknown versions fail closed; there is no best-effort
  projection in the core crate.

Session entries are ordered by canonical session bytes and then generation. The latest 16
generations per session remain in the runtime snapshot so operators can distinguish recent terminal
state and cleanup results from the active generation without unbounded diagnostic retention.

## Session fields

Every entry contains session identity, generation, canonical state, direct topology, negotiated
capabilities, memory backend, signal backend, the last structured failure, and cleanup outcome.
`FailureDiagnostics` contains only a stable `FailureStage`, `ErrorCategory`, `ErrorCode`, and
`Recoverability`. `CleanupStatus` distinguishes active ownership (`Pending`), successful cleanup
(`Complete`), and cleanup failure (`Failed`). `last_error` and `resources_cleaned` remain as the
v1 compatibility projection; v2 consumers use `last_failure` and `cleanup`.

## Counter semantics

All counters are process-local, monotonic, saturating `u64` values.

| Counter group | Meaning |
|---|---|
| bytes/messages sent/received | Application payload accepted or delivered at the public boundary |
| backpressure/writable | Backpressure observations and low-watermark recovery edges |
| primary/polling wakeups | Provider hints and correctness-poll wakeups observed by transports |
| coalesced wakeups | Posts suppressed because shared state was already pending |
| polling recoveries | Correctness polls that found shared-state progress independent of a hint |
| signal failures | Sender/listener provider failures |
| validation/authentication failures | Protocol/bootstrap validation and trust-policy failures |
| failures | All stable non-backpressure failures observed at the public boundary |
| replacements | Successfully installed new generations |

Snapshots are diagnostic observations rather than a transactional accounting ledger. Concurrent
counter reads can span adjacent operations, but individual counters never decrease or wrap.
