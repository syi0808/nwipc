# Observability·Testing 구현계획

## Diagnostics와 metrics

대상은 `nwipc-diagnostics`, `nwipc-metrics`다.

구현:

- Session identity/generation/state, topology, capability
- Memory/signal backend와 lifecycle snapshot
- Sent/received bytes/messages
- Backpressure, wake-up, coalescing, polling recovery
- Validation/authentication/reconnect failure counter
- Payload, secret, native handle을 기록하지 않는 redaction schema

Core는 telemetry SDK에 직접 의존하지 않는다. Tracing adapter는 후순위 feature로 둔다.

## Testkit

`nwipc-testkit`:

- Owned byte buffer fake mapped region
- Deterministic/coalescing/dropping signal
- Fake clock, fake supervisor
- Corrupt record와 fault injection point

`nwipc-process-testkit`:

- Child process orchestration
- Inherited descriptor 제어
- Bounded timeout, exit/log artifact 수집
- Reservation/header/payload/commit/signal/ack crash point

`nwipc-webkit-testkit`:

- Bundle load, frame/world callback
- Reload/navigation/process replacement
- WebContent kill
- Signing/hardened build와 JSC callback lifecycle

## 테스트 계층

| 계층 | 기법 |
|---|---|
| Unit | Table/boundary test |
| Property | Cursor/layout/protocol proptest |
| Fuzz | Record/bootstrap/protocol arbitrary bytes |
| Concurrency | Loom 또는 추상 model |
| Contract | Fake/실제 provider 공통 suite |
| Process | 실제 child와 fault injection |
| WebKit E2E | AppKit harness, reload/kill/signing |
| Benchmark | Latency, throughput, wake-up, copy count |

## 필수 failure matrix

```text
bootstrap: partial write / invalid length / timeout / wrong version
writer:    after header / mid payload / before commit / after commit
signal:    before post / after post / dropped / duplicated / delayed
reader:    malformed cursor / length / kind / stale generation
lifecycle: peer exit / renderer exit / reload / navigation / repeated close
platform:  bundle missing / SPI missing / IOSurface fail / notify fail
```

각 test는 expected state, error code, cleanup, 다음 generation 영향을 함께 검증한다.

## 성능 baseline

- Payload: 64 B, 1 KiB, 16 KiB, maximum inline
- One-way/round-trip latency와 throughput
- Ring saturation bytes/messages
- Signal/message와 coalescing ratio
- JSC boundary copy count
- Idle CPU와 polling recovery latency

결과에는 OS, architecture, build mode, provider, ring capacity를 기록한다.

## 보안·안전성 검증

- Pointer 생성 전 descriptor/mapping 범위 검사
- 상대 cursor/header의 checked arithmetic
- Commit되지 않은 bytes 읽기 금지
- Session/generation mismatch 즉시 실패
- Bootstrap one-shot/bounded size/lifetime
- FFI panic boundary와 unsafe audit
- IOSurface/bootstrap 노출 threat model

Threat model상 인증 없는 공유 region이 허용 불가능하면 crypto를 vertical slice 선행 조건으로 승격한다.

## 완료 기준

- 임의 record/bootstrap input에서 panic/OOB가 없다.
- Crash와 lost signal matrix가 자동화된다.
- Diagnostics만으로 backend/state/failure code를 식별할 수 있고 payload/secret은 노출되지 않는다.

