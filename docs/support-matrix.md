# 지원 및 검증 매트릭스

## Runtime 지원

| 범위 | OS/architecture | 상태 | 검증 |
|---|---|---|---|
| Domain, wire codec, fragmented in-process data plane, renderer core | Linux/macOS, x86_64/arm64 | 지원 | stable workspace CI, fixture, stress, fuzz |
| Native process-test peer | Linux/macOS, x86_64/arm64 | 실험적 | child echo, partial bootstrap, timeout, kill/reap, replacement |
| IOSurface + Darwin Notify provider | macOS arm64/x86_64 | 실험적 | provider contract, two-process tests, actual-provider release benchmark |
| JSC binding | macOS arm64 | 실험적 | callback/teardown contract; signed E2E에서 load |
| Runtime-neutral async / Tokio peer adapter | Linux/macOS, x86_64/arm64 | 실험적 | fake readiness contract, bounded polling recovery, workspace CI |
| Wry 0.55 / Tauri 2.11 adapter | macOS 26.2 arm64 | 실험적 | builder configuration merge, framework lifecycle/cleanup, stale generation, workspace CI |
| WKWebView injected bundle | macOS 26.2 arm64 | 제한적 지원 | SPI allowlist/probe와 trusted signed hardened E2E |
| macOS 26.2 x86_64 | provider만 검증 | WebKit 지원 안 함 | trusted signed process matrix가 필요함 |
| 그 외 macOS release | allowlist 밖 | 지원 안 함 | `Unsupported`로 fail closed |
| Windows, iOS, Android | 전체 | 지원 안 함 | provider 없음 |

Rust MSRV는 `1.85.0`, TypeScript client의 Node.js 최소 버전은 20이다. Wire layout은 explicit
little-endian/fixed-width fixture로 architecture 독립성을 검사하지만 실제 WebKit 지원은 위에
명시한 OS/architecture 조합만 의미한다.

## Failure matrix

| Fault | 자동 검증 | 기대 결과 |
|---|---|---|
| bootstrap partial read / early EOF / invalid length / timeout | `nwipc-peer-bootstrap` tests, bootstrap fuzz | typed `Truncated`/`InvalidRange`/`Timeout`, endpoint replacement |
| record header/payload before commit에서 writer 종료 | ring writer crash-injection test | producer cursor 불변, reader에 bytes 비노출 |
| after commit에서 writer 종료 | concurrent/stress ring tests | commit된 complete record만 FIFO 전달 |
| signal dropped / duplicated / coalesced / delayed | channel/signal contract tests와 signed `webkit-e2e` | cursor drain으로 progress, 중복 delivery 없음 |
| malformed record length/kind/flags/cursor | record/ring tests and fuzz | panic/OOB 없이 typed protocol error |
| stale generation/document | peer, renderer, WebKit contract tests | attach/delivery 거부와 generation 교체 |
| peer/WebContent exit, commit 전후 writer exit, repeated close / replacement | native process tests와 signed `webkit-e2e` | partial bytes 비노출, committed bytes 전달, bounded cleanup, child reap, 새 generation만 사용 |
| SPI/bundle/signing/IOSurface failure | provider tests and `webkit-e2e` | silent fallback 없이 fail closed |

## 알려진 제한

- Data-plane fragmentation은 production WebKit handshake와 signed boundary matrix에 연결됐다.
- Record inline payload 상한은 1 MiB이고 논리 메시지 상한은 채널 생성 시 ring 범위 안에서 설정한다.
- Encryption/authentication과 reconnect policy는 아직 없다.
- Production WebKit origin별 binding policy는 아직 없다.
- Wry/Tauri adapter는 macOS production provider만 지원하며 framework IPC를 payload fallback으로 사용하지 않는다.
- Developer ID notarization/stapling과 WebKit macOS minor-release/x86_64 matrix는 아직 없다.
- GitHub-hosted trusted signing과 immutable release evidence 자동화는 아직 완료되지 않았다.
- Benchmark 수치는 machine-specific baseline이며 회귀 임계값으로 사용하기 전에 같은 runner에서
  반복 측정해야 한다.

## 검증 명령

```sh
cargo xtask hardening-check
cargo test --workspace
cargo run --release -p xtask -- benchmark
cargo xtask webkit-e2e                 # allowlisted macOS only
cargo test --release -p nwipc-macos-transport --test actual_provider_benchmark -- --ignored --nocapture
```

Nightly sanitizer/Miri와 fuzz smoke는 `.github/workflows/hardening.yml`에서 수동 또는 주간 실행된다.
전체 release 증거와 diagnostics compatibility 규칙은 [`release-gate.md`](release-gate.md)와
[`diagnostics-schema.md`](diagnostics-schema.md)에 고정한다.
