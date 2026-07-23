# M6 release gate

A release candidate is the same immutable commit for every gate below. A local pass or a run from
another commit is not substitutable. The release record links the `CI`, `Hardening`, and manually
dispatched `Release Gate` workflow runs plus signed E2E artifacts.

## Vertical slice와 release qualification

Vertical slice milestone은 같은 candidate의 clean `CI`/`Hardening`, 양쪽 architecture/provider gate와
기록된 로컬 trusted-identity hardened E2E로 완료할 수 있다. 이는 제품 호출 경로의 폐쇄를 판정하는
개발 milestone이다. 배포용 release candidate에는 예외가 없으며 아래 자동 gate와 immutable
`release-record`가 모두 성공해야 한다. 현재 vertical slice 증거는
[`evidence/vertical-slice-3fecc42.md`](evidence/vertical-slice-3fecc42.md)에 기록한다.

## Required gates

| Gate | Required scope | Workflow/job |
|---|---|---|
| Stable workspace | format, clippy, Rust/TypeScript tests, dependency policy, JSC lifecycle repetition | `CI` |
| Memory safety | Miri, AddressSanitizer, record/bootstrap/layout/fragment/protocol/validation/crypto fuzz smoke | `Hardening` |
| Cross architecture | identical fixed-width fixtures and diagnostics contracts on macOS arm64 and x86_64 | `CI / cross-architecture-fixtures`, `Release Gate / cross-architecture` |
| Actual providers | IOSurface, Darwin/hybrid, Mach memory/port contracts and baseline benchmark on both macOS architectures | `Release Gate / cross-architecture` |
| Production process | trusted-identity signed/hardened WebKit notification, crash, kill, reload, and generation matrix | `Release Gate / signed-webkit-e2e` |

The release dispatch requires the full candidate commit SHA. Its preflight checks out that exact
commit and refuses to proceed unless successful `CI` and `Hardening` runs exist for the same SHA.
The final `release-evidence` job uploads a record linking those runs and the current release gate;
it fails unless both architecture jobs and the requested trusted signed E2E job succeed.
Test-to-job-to-support traceability is fixed in
[vertical-slice-verification.md](vertical-slice-verification.md).
Mach-only 전환 전후의 provider 동등성, fault, diagnostics와 cleanup 비교 항목은
[mach-migration-baseline.md](mach-migration-baseline.md)에 고정한다.

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

## Dispatch

After the candidate is pushed and its automatic `CI` and `Hardening` runs pass:

```sh
gh workflow run release.yml \
  -f candidate_sha="$(git rev-parse HEAD)" \
  -f signed_e2e=true
```

Download and retain the `release-record`, both `actual-provider-*`, and `signed-webkit-e2e`
artifacts. The release is incomplete if any artifact comes from another commit or the evidence job
does not pass.

## Support boundary

Portable layout/protocol behavior is released for Linux/macOS x86_64 and arm64. IOSurface and
Darwin/hybrid provider contracts are gated on macOS 15 arm64 and Intel runners. Production WebKit
verified support remains the exact macOS 26.2 (25C56) arm64 build until another signed process
matrix is recorded; provider coverage alone does not expand the WebKit support claim.
