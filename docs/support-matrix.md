# 지원 및 검증 매트릭스

## Runtime 지원

| 범위 | OS/architecture | 상태 | 검증 |
|---|---|---|---|
| Domain, wire codec, in-process data plane, renderer core | Linux/macOS, x86_64/arm64 | 지원 | stable workspace CI, fixture, stress, fuzz |
| Native process-test peer | Linux/macOS, x86_64/arm64 | 실험적 | child echo, partial bootstrap, timeout, kill/reap, replacement |
| IOSurface + Darwin Notify provider | macOS arm64 | 실험적 | provider contract와 two-process tests |
| JSC binding | macOS arm64 | 실험적 | callback/teardown contract; signed E2E에서 load |
| WKWebView injected bundle | macOS 26.2 arm64 | 제한적 지원 | SPI allowlist/probe와 signed hardened E2E |
| macOS 26.2 x86_64 | 미검증 | 지원 안 함 | runtime probe가 architecture 보증을 대신하지 않음 |
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
| signal dropped / duplicated / coalesced / delayed | channel and signal contract tests | cursor drain으로 progress, 중복 delivery 없음 |
| malformed record length/kind/flags/cursor | record/ring tests and fuzz | panic/OOB 없이 typed protocol error |
| stale generation/document | peer, renderer, WebKit contract tests | attach/delivery 거부와 generation 교체 |
| peer exit / repeated close / replacement | native process tests | bounded cleanup, child reap, 새 session만 사용 |
| SPI/bundle/signing/IOSurface failure | provider tests and `webkit-e2e` | silent fallback 없이 fail closed |

## 알려진 제한

- Fragmentation, encryption/authentication, async API, reconnect policy는 아직 없다.
- Inline payload 상한은 1 MiB이며 초과 입력은 `MessageTooLarge`다.
- Production ring/record handshake를 사용하는 WebKit echo와 origin별 binding policy는 아직 없다.
- Developer ID notarization/stapling과 macOS minor-release/x86_64 matrix는 아직 없다.
- Benchmark 수치는 machine-specific baseline이며 회귀 임계값으로 사용하기 전에 같은 runner에서
  반복 측정해야 한다.

## 검증 명령

```sh
cargo xtask hardening-check
cargo test --workspace
cargo run --release -p xtask -- benchmark
cargo xtask webkit-e2e                 # allowlisted macOS only
```

Nightly sanitizer/Miri와 fuzz smoke는 `.github/workflows/hardening.yml`에서 수동 또는 주간 실행된다.
