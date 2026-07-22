# 단계별 실행계획과 완료 정의

## 선행 관계

```text
Domain → Layout/Record → Validation/Protocol → Atomic/Ring → Channel
Memory API → IOSurface ─┐
Signal API → Darwin  ───┼→ Bootstrap/Peer → Runtime
Channel ────────────────┘
Renderer Core → JSC ───────────┐
Runtime + SPI + Bundle ────────┼→ AppKit E2E
TypeScript Client ─────────────┘
```

## Phase 0 — 저장소 기반과 결정 기록

- Workspace, lint, license, CI, xtask
- Crate stability/owner metadata
- Layout/cursor/memory ordering/bootstrap/IOSurface/WebKit ADR
- Architecture dependency check

완료: 첫-slice skeleton compile, core CI 통과, 금지 dependency/unsafe 탐지.

## Phase 1 — Protocol foundation

- Domain, layout, record, protocol, validation
- Golden fixture, property test, fuzz harness

완료: Cross-architecture fixture 일치, arbitrary decode panic 없음, mismatch typed error.

## Phase 2 — In-process data plane

- Atomic, ring, flow, channel
- Fake memory/signal과 fault injection

완료: 양방향 FIFO/backpressure, crash-safe publication, lost signal progress.

## Phase 3 — Native two-process

- Bootstrap schema/codec/pipe
- Peer core/facade와 process harness

완료: Parent/child echo, partial bootstrap와 crash cleanup, stale generation 거부.

## Phase 4 — macOS providers

- IOSurface
- Darwin Notify, polling, hybrid

완료: 두 process raw bytes/notification, dropped signal recovery, provider diagnostics.

## Phase 5 — Renderer runtime

- Renderer API/core, JSC
- TypeScript packages와 mock binding

완료: WebKit 없는 state test, JSC contract, stale document invalidation.

## Phase 6 — WebKit/AppKit vertical slice

- SPI/host, bundle/shim/artifact
- AppKit example와 native peer

완료: Host relay 없는 양방향 echo, reload/kill generation replacement, unsupported 명시.

## Phase 7 — Hardening

- Stress, crash injection, fuzz corpus, sanitizer/Miri 범위
- Unsafe audit, threat model, signed/hardened build
- Benchmark baseline과 support matrix

완료: Deadlock/OOB/stale delivery 없음, crash matrix 통과, 제한 문서화.

## Phase 8 — 확장

1. Fragmentation
2. Async/Tokio
3. Wry
4. Tauri
5. Authentication/encryption
6. Mach provider
7. Chunk pool/borrowed API
8. Bun/타 플랫폼

## 최초 백로그

P0:

- [x] Cargo/pnpm workspace, toolchain, lint, deny, license
- [x] First-slice skeleton과 architecture check
- [x] Domain contract와 layout/record ADR
- [ ] Protocol fixture와 fake provider

P1:

- [ ] Layout/record/validation/handshake
- [x] Atomic/ring/flow/channel
- [x] Data-plane boundary/concurrency test와 in-process echo

P2:

- [x] Bootstrap/peer/process harness
- [ ] IOSurface와 Darwin/hybrid
- [ ] Two-process provider contract

P3:

- [ ] Renderer core, TS packages, JSC
- [ ] SPI/bundle/host/AppKit example

P4:

- [ ] Reload/kill/generation replacement
- [ ] Diagnostics, signing, stress/crash/fuzz, support matrix

## Vertical slice 완료 정의

1. Core가 macOS SDK 없이 compile/test된다.
2. Layout fixture가 arm64/x86_64에서 동일하다.
3. Ring이 FIFO, boundary, backpressure, crash tests를 통과한다.
4. Arbitrary input이 panic/OOB를 만들지 않는다.
5. IOSurface/Darwin이 두 process 사이에서 독립 검증된다.
6. Signal 유실을 polling이 회복한다.
7. Peer가 WebView dependency 없이 동작한다.
8. Renderer core가 JSC/WebKit 없이 테스트된다.
9. JSC teardown 뒤 callback/protected object가 남지 않는다.
10. Bundle은 main frame normal world에만 binding을 설치한다.
11. Renderer↔peer가 host relay 없이 binary echo를 수행한다.
12. Reload/crash 뒤 stale generation message가 전달되지 않는다.
13. Failure는 structured error/diagnostics로 보인다.
14. Silent fallback/no-op가 없다.
15. Unsafe/dependency/license/lint/test/package CI가 통과한다.

## 구현 전 ADR 결정

| 결정 | 기본 제안 |
|---|---|
| Rust | Edition 2024 + CI 검증 MSRV |
| Cursor | aligned shared `u32`, wrapping distance 제한 |
| Region | 방향별 IOSurface 1개, 총 2개 |
| Header | fixed page-sized 후보 |
| Alignment | record 8 bytes |
| Message | 명시적 maximum inline |
| Bootstrap | peer binary codec + renderer plist adapter |
| IOSurface descriptor | ID/Mach representation 실험 후 선택 |
| Notification | session+generation+direction 기반 이름 |
| JSC receive | 첫 slice JS-owned copy |
| Reconnect | runtime mechanism, application policy |
| WebKit | OS/build allowlist + runtime probe |

Cursor/layout/record/bootstrap 결정은 Phase 1 전에 고정한다. IOSurface와 WebKit SPI는 실행 실험으로 가능성을 확인한 뒤 production API를 고정한다.

## 권장 착수 순서

1. Workspace/architecture check
2. Domain/layout/record/validation
3. Golden fixture/property test
4. Fake provider 위 atomic/ring/channel
5. In-process echo와 crash/signal-loss test
6. Bootstrap/peer two-process echo
7. IOSurface/Darwin 교체
8. Renderer core/TS mock 완성
9. JSC/bundle stub
10. Host/AppKit E2E
11. Reload/crash/replacement hardening
