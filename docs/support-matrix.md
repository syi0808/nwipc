# 지원 및 검증 매트릭스

## Runtime 지원

| 범위 | OS/architecture | 상태 | 검증 |
|---|---|---|---|
| Domain, wire codec, fragmented in-process data plane, renderer core | Linux/macOS, x86_64/arm64 | 지원 | stable workspace CI, fixture, stress, fuzz |
| Native process-test peer | Linux/macOS, x86_64/arm64 | 실험적 | child echo, partial bootstrap, timeout, kill/reap, replacement |
| IOSurface + Darwin Notify provider | macOS arm64/x86_64 | 실험적 | authenticated encryption, provider contract, two-process tests, actual-provider release benchmark |
| Mach memory + port signal provider | macOS arm64/x86_64 | 실험적 | capability transfer, native protection, two-process raw-byte/notification contract |
| JSC binding | macOS arm64 | 실험적 | callback/teardown contract; signed E2E에서 load |
| Runtime-neutral async / Tokio peer adapter | Linux/macOS, x86_64/arm64 | 실험적 | fake readiness contract, bounded polling recovery, workspace CI |
| Wry 0.55 / Tauri 2.11 adapter | macOS arm64/x86_64 | 실험적 | builder configuration merge, framework lifecycle/cleanup, stale generation, workspace CI |
| WKWebView injected bundle | macOS 26.2 (25C56) arm64 | 검증됨 | trusted signed hardened E2E 전체 matrix 통과 |
| 미검증 macOS/build | x86_64 10.12+, arm64 11.0+ | `BestEffort` | 필수 SPI runtime probe 통과 시 실행; 동작과 오류를 보장하지 않음 |
| 실행 불가능한 macOS | x86_64 10.12 미만, arm64 11.0 미만, 기타 architecture, 필수 SPI 누락 | `Incompatible` | loader 하한 미달은 실행 불가; runtime probe 실패는 typed `Unsupported` |
| Linux native shared-memory/signal provider | 전체 | 지원 안 함 | provider 없음; 별도 구현 계획 |
| Windows, iOS, Android | 전체 | 지원 안 함 | provider 없음 |

Rust MSRV는 `1.85.0`, TypeScript client의 Node.js 최소 버전은 20이다. Wire layout은 explicit
little-endian/fixed-width fixture로 architecture 독립성을 검사하지만 실제 WebKit 지원은 위에
명시한 OS/architecture 조합만 의미한다.

판정은 **실행 가능성**과 **검증 증거**를 분리한다. Apple은 [`WKWebView`를 macOS 10.10부터
제공](https://developer.apple.com/documentation/webkit/wkwebview)하지만, Rust Apple target의
하한이 [x86_64 macOS 10.12, arm64 macOS
11.0](https://doc.rust-lang.org/rustc/platform-support/apple-darwin.html)이므로 NWIPC의 논리적
하한은 후자다. 이 하한 이상에서는 `_WKProcessPoolConfiguration`과 필수 selector를 runtime에
조회하고 모두 존재하면 실행한다. 정확히 검증된 product version/build/architecture만
`Verified`, 나머지는 `BestEffort`다. 버전이 더 새롭거나 major가 달라도 그 사실만으로 차단하지
않는다. `BestEffort`의 실패는 NWIPC가 재현이나 호환성을 보장하지 않지만, 입력 검증·메모리
안전성·인증 실패 같은 보안 경계는 동일하게 fail closed 한다.

## Failure matrix

| Fault | 자동 검증 | 기대 결과 |
|---|---|---|
| bootstrap partial read / early EOF / invalid length / timeout | `nwipc-peer-bootstrap` tests, bootstrap fuzz | typed `Truncated`/`InvalidRange`/`Timeout`, endpoint replacement |
| record header/payload before commit에서 writer 종료 | ring writer crash-injection test | producer cursor 불변, reader에 bytes 비노출 |
| after commit에서 writer 종료 | concurrent/stress ring tests | commit된 complete record만 FIFO 전달 |
| signal dropped / duplicated / coalesced / delayed | channel/signal contract tests와 signed `webkit-e2e` | cursor drain으로 progress, 중복 delivery 없음 |
| malformed record length/kind/flags/cursor | record/ring tests and fuzz | panic/OOB 없이 typed protocol error |
| ciphertext/tag/counter tamper, replay, wrong secret/generation | `nwipc-crypto` contract tests | typed `AuthenticationFailed`/`ReplayDetected`, endpoint replacement, receive counter 불변 |
| stale generation/document | peer, renderer, WebKit contract tests | attach/delivery 거부와 generation 교체 |
| peer/WebContent exit, commit 전후 writer exit, repeated close / replacement | native process tests와 signed `webkit-e2e` | partial bytes 비노출, committed bytes 전달, bounded cleanup, child reap, 새 generation만 사용 |
| SPI/bundle/signing/IOSurface failure | provider tests and `webkit-e2e` | silent fallback 없이 fail closed |

## 알려진 제한

- Data-plane fragmentation은 production WebKit handshake와 signed boundary matrix에 연결됐다.
- Record inline payload 상한은 1 MiB이고 논리 메시지 상한은 채널 생성 시 ring 범위 안에서 설정한다.
- Production frame authentication/encryption은 필수이며 certificate identity, forward secrecy와 reconnect policy는 아직 없다.
- Production WebKit origin별 binding policy는 아직 없다.
- Wry/Tauri adapter는 macOS production provider만 지원하며 framework IPC를 payload fallback으로 사용하지 않는다.
- Bun은 외부 adapter/addon이 아니라 source 내부 native integration으로 계획하며, 현재 embedding
  bootstrap과 host-driven readiness 경계는 완료되지 않았다.
- Windows/Linux native provider는 구현되지 않았으며
  [`implementation-plan/14-windows-linux-providers.md`](implementation-plan/14-windows-linux-providers.md)의
  독립 계획을 따른다.
- Developer ID notarization/stapling과 WebKit macOS minor-release/x86_64 matrix는 아직 없다.
- GitHub-hosted trusted signing과 immutable release evidence 자동화는 아직 완료되지 않았다.
- Mach provider는 독립 process contract만 완료했으며 production WebKit 전환과 legacy provider 제거는
  [`implementation-plan/12-mach-only-migration.md`](implementation-plan/12-mach-only-migration.md)를 따른다.
- Benchmark 수치는 machine-specific baseline이며 회귀 임계값으로 사용하기 전에 같은 runner에서
  반복 측정해야 한다.

## 검증 명령

```sh
cargo xtask hardening-check
cargo test --workspace
cargo run --release -p xtask -- benchmark
cargo xtask webkit-e2e                 # runtime-compatible macOS
cargo test --release -p nwipc-macos-transport --test actual_provider_benchmark -- --ignored --nocapture
```

Nightly sanitizer/Miri와 fuzz smoke는 `.github/workflows/hardening.yml`에서 수동 또는 주간 실행된다.
전체 release 증거와 diagnostics compatibility 규칙은 [`release-gate.md`](release-gate.md)와
[`diagnostics-schema.md`](diagnostics-schema.md)에 고정한다.
