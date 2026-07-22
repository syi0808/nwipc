# 단계별 실행계획과 완료 정의

## 선행 관계

```text
Domain → Layout/Record → Validation/Protocol ─┐
Atomic/Ring/Fragment/Channel ─────────────────┼→ Provider-neutral transport
Memory API → IOSurface ───────────────────────┤
Signal API → Darwin/Poll/Hybrid ──────────────┘

Bootstrap schema/codec → Session machine → Runtime ─────┐
Provider-neutral transport → Peer/Renderer endpoint ────┼→ Public facade
Renderer Core → JSC → Bundle/SPI ───────────────────────┘

Public facade + AppKit host + TypeScript client → Production WebKit E2E
```

## 상태 판정 규칙

로드맵의 상태는 코드 존재 여부가 아니라 실제 호출 경로와 검증 수준으로 판정한다.

| 상태 | 의미 |
|---|---|
| 미착수 | 공개 contract와 구현이 아직 없다. |
| 경계 정의 | Type/API는 있으나 operational path가 typed `Unsupported`를 반환한다. |
| 단위 완료 | Fake/in-process provider 위에서 기능과 failure contract가 검증된다. |
| Provider 통합 | 실제 OS/process provider의 독립 contract test가 통과한다. |
| Production 통합 | Public facade에서 protocol, runtime, data plane, provider가 우회 없이 연결된다. |
| E2E 검증 | 지원 대상의 signed/hardened process에서 production 통합 경로가 통과한다. |

Testkit 전용 frame, raw mapping 접근 또는 payload용 stream transport는 하위 계층 검증에는 사용할
수 있지만 Production 통합이나 E2E 검증의 근거로 삼지 않는다. Phase와 backlog의 `[x]`는 해당 범위의
완료 정의, committed artifact, clean CI 증거가 모두 있을 때만 사용한다.

## Phase 0 — 저장소 기반과 결정 기록

- Workspace, lint, license, CI, xtask
- Crate stability/owner metadata
- Layout/cursor/memory ordering/bootstrap/IOSurface/WebKit ADR
- Architecture dependency check

기존 완료 기준: 첫-slice skeleton compile, core CI 통과, 금지 dependency/unsafe 탐지.

## Phase 1 — Protocol foundation

- Domain, layout, record, protocol, validation
- Golden fixture, property test, fuzz harness

기존 완료 기준: Cross-architecture fixture 일치, arbitrary decode panic 없음, mismatch typed error.

## Phase 2 — In-process data plane

- Atomic, ring, flow, channel
- Fake memory/signal과 fault injection

기존 완료 기준: 양방향 FIFO/backpressure, crash-safe publication, lost signal progress.

## Phase 3 — Native two-process

- Bootstrap schema/codec/pipe
- Peer core/facade와 process harness

기존 완료 기준: Parent/child echo, partial bootstrap와 crash cleanup, stale generation 거부.

## Phase 4 — macOS providers

- IOSurface
- Darwin Notify, polling, hybrid

기존 완료 기준: 두 process raw bytes/notification, dropped signal recovery, provider diagnostics.

## Phase 5 — Renderer runtime

- Renderer API/core, JSC
- TypeScript packages와 mock binding

기존 완료 기준: WebKit 없는 state test, JSC contract, stale document invalidation.

## Phase 6 — WebKit/AppKit vertical slice

- SPI/host, bundle/shim/artifact
- AppKit example와 native peer

기존 완료 기준: Host relay 없는 양방향 echo, reload/kill generation replacement, unsupported 명시.

## Phase 7 — Hardening

- Stress, crash injection, fuzz corpus, sanitizer/Miri 범위
- Unsafe audit, threat model, signed/hardened build
- Benchmark baseline과 support matrix

기존 완료 기준: Deadlock/OOB/stale delivery 없음, crash matrix 통과, 제한 문서화.

## Phase 8 — 확장

1. [x] Fragmentation (data-plane integration)
2. [ ] Async/Tokio
3. [ ] Wry
4. [ ] Tauri
5. [ ] Authentication/encryption
6. [ ] Mach provider
7. [ ] Chunk pool/borrowed API
8. [ ] Bun/타 플랫폼

Phase 8 항목은 production vertical slice의 통합 경로가 닫힌 뒤 착수한다. 이미 완료된 fragmentation도
production handshake가 capability를 협상하기 전까지 WebKit transport에서는 활성화하지 않는다.

## 최초 백로그 상태

P0:

- [x] Cargo/pnpm workspace, toolchain, lint, deny, license
- [x] First-slice skeleton과 architecture check
- [x] Domain contract와 layout/record ADR
- [x] Layout/record fixture와 fake memory/signal provider
- [x] Protocol/handshake fixture

P1:

- [x] Layout/record
- [x] Validation/protocol handshake
- [x] Atomic/ring/flow/channel
- [x] Data-plane boundary/concurrency test와 in-process echo

P2:

- [x] Bootstrap/peer/process harness
- [x] IOSurface와 Darwin/hybrid
- [x] Two-process provider contract

P3:

- [x] Renderer core, TS packages, JSC
- [x] SPI/bundle/host/AppKit example

P4:

- [x] Reload/kill/generation replacement
- [x] Diagnostics/metrics operational snapshot
- [x] Signed/hardened build와 threat model/unsafe audit
- [x] Stress/crash/fuzz와 sanitizer/Miri 자동화
- [x] Benchmark baseline과 support/failure matrix

## 현재 재기준선

| 영역 | 현재 상태 | 남은 production gap |
|---|---|---|
| Domain, layout, record, protocol, validation | Production 통합 | M5 WebKit E2E에서 production endpoint 경로 검증 |
| Ring, flow, channel, fragmentation | Production 통합 | M5 signed WebKit process에서 같은 public transport 사용 |
| IOSurface, Darwin, polling/hybrid | Production 통합 | M5 WebKit sandbox/수명 failure matrix 검증 |
| Bootstrap schema/codec | Production 통합 | M5 renderer plist가 동일 descriptor bundle을 전달 |
| Peer core/facade | Production 통합 | M5 peer kill/replacement를 public facade로 검증 |
| Session, session machine, runtime | Production 통합 | M5 reload/WebContent kill generation replacement 검증 |
| Renderer core, JSC, WebKit control plane | Production 통합 | M5 bundle에서 public renderer factory 호출 |
| WebKit process smoke | Provider 통합 | Raw echo frame 대신 public renderer↔peer transport 사용 |
| Diagnostics, metrics, top-level facade | Production 통합 | M6 failure/wakeup 세부 counter와 release schema 확정 |

독립 provider와 signed WebKit smoke는 실제 환경 가능성을 검증했지만 전체 product call graph의 완료를
의미하지 않는다. 다음 milestone은 새로운 provider나 framework 확장보다 production 경로 폐쇄를 우선한다.

## Production vertical slice 통합 milestone

### M0 — Baseline 안정화

- [x] 완료된 fragmentation을 포함한 현재 workspace를 format/clippy/test/fuzz가 통과하는 baseline으로 고정
- [x] JSC lifecycle test의 반복 실행 안정성 확보
- [x] Roadmap 상태와 committed code/CI gate 동기화

고정된 검증 경로는 `.github/workflows/ci.yml`의 Rust/TypeScript/architecture gate와 macOS JSC lifecycle
25회 반복, `.github/workflows/hardening.yml`의 Miri/ASan/record·bootstrap·layout·fragment fuzz smoke다.
Hardening은 PR, `main` push, 주간 schedule에서 같은 범위를 실행한다. JSC test는 process-wide 직렬화 후
64번의 document/context 생성·연결·close·teardown을 검증해 병렬 test runner와 context 재사용의 영향을
분리한다.

완료: Core/TypeScript/hardening CI가 clean worktree에서 반복 가능하게 통과하고 flaky failure가 없다.

### M1 — Protocol과 validation 폐쇄

- [x] `nwipc-protocol`에 version/capability 협상과 HELLO/ACK state machine 구현
- [x] `nwipc-validation`에 layout/cursor/record/payload 검증 단일 진입점 구현
- [x] Peer와 renderer가 공통 handshake와 stable error mapping 사용
- [x] Protocol/bootstrap golden fixture, property test, arbitrary-input fuzz 추가

완료: 두 crate의 production path에서 placeholder `Unsupported`가 제거되고 malformed input이 mapping
범위 접근 전에 stable typed error로 거부된다.

검증 경로는 protocol/bootstrap golden fixture, version overlap 전수 property test, protocol/validation fuzz
target, 공통 peer/renderer handshake contract test와 workspace clippy/test gate다.

### M2 — Session과 runtime ownership

- [x] `nwipc-session`에 identity, generation, prepared resource, endpoint 상태와 idempotent cleanup 구현
- [x] `nwipc-session-machine`에 transition별 resource/lifecycle side effect 구현
- [x] `nwipc-runtime`에 registry, ID/generation 발급, provider selection, replacement routing 구현
- [x] Partial attach, endpoint exit, duplicate close, multi-session isolation 검증

완료: Runtime이 generation의 준비부터 교체/종료까지 control plane을 소유하고 old generation의 mapping,
signal, port, callback이 다음 generation에서 재사용되지 않는다.

검증 경로는 generation-bound resource의 역순·단일 cleanup, partial attach와 duplicate close lifecycle test,
endpoint exit 뒤 replacement/stale handle 거부, 실패한 generation 번호의 비재사용, multi-session/runtime drop
contract test와 workspace format/clippy/test 및 architecture gate다.

### M3 — Provider-neutral production transport

- [x] OS `MappedRegion`을 atomic cursor와 ring reader/writer에 안전하게 연결
- [x] 방향별 IOSurface와 Darwin/hybrid signal을 channel adapter로 조립
- [x] Fake provider와 IOSurface/Darwin provider에 동일 transport contract suite 적용
- [x] Fragmentation capability와 negotiated message limit을 production handshake에 연결

완료: 실제 provider 위에서 FIFO, boundary, backpressure, atomic batch publication, close/reset, dropped-signal
recovery가 같은 channel API로 동작한다.

검증 경로는 `nwipc-channel-transport`의 공통 contract suite다. Fake mapping과 drop signal 조합 및 실제
IOSurface/Darwin 조합에 동일한 FIFO, zero/exact/fragmented boundary, saturation/writable recovery,
close/reset 시나리오를 적용한다. Mapped cursor는 provider의 acquire/release atomic API만 사용하며 region
identity/layout/length 검증 후에 ring 범위에 연결된다. Fragmentation은 handshake에서 협상된 capability가
있을 때만 활성화되고 logical message limit은 협상 결과의 작은 값으로 제한된다.

### M4 — Public facade와 endpoint 통합

- [x] `nwipc` facade에 configuration, runtime/session, diagnostics 접근 API 구현
- [x] `nwipc-peer`가 inherited bootstrap에서 실제 memory/signal descriptor를 attach
- [x] stdin/pipe는 bootstrap에만 사용하고 application payload에서는 제거
- [x] Renderer transport factory가 bootstrap resource로 같은 production channel 생성
- [x] Endpoint core는 executor/thread/process를 소유하지 않는 synchronous contract를 유지하고 runtime adapter가
  wire protocol을 재구현하지 않도록 한다.

완료: Public renderer와 peer API가 provider 세부사항을 노출하지 않고 send/receive/backpressure/close를
수행하며 top-level facade의 지원 경로에서 placeholder `Unsupported`가 없다. 새로운 runtime adapter는
endpoint contract와 lifecycle/wakeup bridge만 구현하면 된다.

검증 경로는 `nwipc` facade의 actual-provider renderer↔peer contract test와
`nwipc-native-peer-example`의 public two-process production echo다. Facade resource preparer가 방향별
IOSurface와 Darwin descriptor bundle을 generation에 묶고, peer의 inherited stdin은 one-shot bootstrap 뒤
닫힌다. 이후 HELLO/ACK와 application frame은 모두 `nwipc-channel-transport`를 통과한다. Process-test stream은
하위 process testkit provider에서만 유지되며 production provider 선택 시 호출되지 않는다.

### M5 — Production WebKit E2E

- [x] Raw `EchoFrame`/mapping polling을 production protocol/channel transport로 교체
- [x] Zero/exact/max/fragmented binary payload와 saturation/writable recovery 검증
- [ ] Dropped/duplicate/delayed notification과 polling recovery 검증
- [ ] Commit 전후 writer crash, peer/WebContent kill, reload와 generation replacement 검증
- [x] Host가 lifecycle과 completion만 관찰하고 payload byte에는 접근하지 않음을 구조적으로 검사

1차 통합은 facade가 발급한 canonical renderer bootstrap을 host가 opaque property-list 값으로 전달하고,
injected bundle이 `RendererBootstrap`과 `MacosRendererTransportFactory`로 public transport를 여는 경로다.
Native peer helper도 `Peer::initialize`와 bootstrap-only stdin을 사용한다. Signed harness는 zero, 16 KiB,
16 KiB+1, 1 MiB payload 및 high/low watermark recovery를 실제 IOSurface/Darwin-hybrid process에서
검증하며 AppKit source와 architecture gate는 raw memory/payload frame 의존을 거부한다.

2026-07-22 macOS 26.5.2 (25F84) arm64에서 ad-hoc hardened `cargo xtask webkit-e2e`가 통과했다.
생성 artifact와 실행 로그 위치는 `target/NWIPC-E2E.app`, `target/webkit-e2e/`다. 이 증거는 아직
notification fault와 crash/generation matrix를 포함하지 않으므로 M5 완료 선언에는 사용하지 않는다.

완료: Signed/hardened AppKit harness에서 public renderer↔peer 경로가 host relay 없이 통과하고 stale
message, callback, resource가 새 generation에 전달되지 않는다.

### M6 — Observability와 release gate

- Session/generation/state/topology/backend/capability snapshot 구현
- Bytes/messages/backpressure/wakeup/coalescing/polling recovery/failure counter 구현
- Payload, secret, native handle을 제외하는 redaction schema와 snapshot compatibility 규칙 확정
- arm64/x86_64 fixture, sanitizer/Miri/fuzz, actual-provider benchmark, signed E2E CI 범위 명시

완료: Failure matrix의 각 case에서 diagnostics만으로 backend/state/stable error/cleanup 결과를 식별할 수
있고 전체 release gate가 통과한다.

## Vertical slice 완료 정의

Foundation:

1. Core가 macOS SDK 없이 compile/test된다.
2. Layout/protocol/bootstrap fixture가 arm64/x86_64에서 동일하다.
3. Ring이 FIFO, boundary, backpressure, fragmentation, crash tests를 통과한다.
4. Arbitrary layout/record/protocol/bootstrap input이 panic/OOB를 만들지 않는다.
5. IOSurface/Darwin이 두 process 사이에서 독립 검증된다.
6. Signal 유실을 polling이 회복한다.

Lifecycle/API:

7. Runtime이 multi-session registry, generation 교체, resource cleanup을 수행한다.
8. Peer가 WebView dependency와 payload stream fallback 없이 동작한다.
9. Renderer core가 JSC/WebKit 없이 테스트된다.
10. JSC teardown 뒤 callback/protected object가 남지 않는다.
11. Bundle은 main frame normal world에만 binding을 설치한다.
12. Public facade가 지원 provider에서 session 생성부터 close까지 수행한다.

Production E2E:

13. Renderer와 peer가 공통 protocol/channel 및 IOSurface/Darwin transport를 사용한다.
14. Renderer↔peer가 host relay 없이 binary echo와 backpressure recovery를 수행한다.
15. E2E는 testkit 전용 frame, raw mapping 또는 payload stream으로 production 계층을 우회하지 않는다.
16. Reload/peer crash/WebContent crash 뒤 stale generation message와 callback이 전달되지 않는다.
17. Commit 전후 crash와 dropped/duplicate/delayed signal의 failure matrix가 실제 process에서 통과한다.

Operations:

18. Failure는 structured error와 redacted operational diagnostics로 보인다.
19. Silent fallback, 성공하는 no-op, 지원 경로의 placeholder `Unsupported`가 없다.
20. Unsafe/dependency/license/format/lint/test/package/fuzz/sanitizer CI가 통과한다.
21. 각 완료 항목은 test 이름, CI job, 지원 matrix와 추적 가능하다.

## 완료 증거와 추적성

각 milestone은 완료 선언과 함께 다음 표를 갱신한다.

| 완료 항목 | Production artifact | Contract/process test | CI job | 지원 조합/제한 |
|---|---|---|---|---|
| 예: dropped signal recovery | hybrid channel adapter | provider contract + process fault test | hardening | macOS arm64 |
| M4 public endpoint integration | `nwipc`, `nwipc-macos-transport`, `nwipc-peer` | `public_facade_connects_renderer_and_peer_without_payload_stream`, `public_endpoints_use_bootstrap_pipe_only_for_production_echo` | Rust workspace test | macOS IOSurface + Darwin/hybrid |
| M5 production WebKit transport (partial) | `nwipc-macos-bundle-shim`, signed AppKit/peer artifact | `renderer_bootstrap_is_canonical_and_one_shot`, `cargo xtask webkit-e2e` | manual signed E2E | macOS 26.5.2 arm64 ad-hoc; fault/crash matrix pending |

- Unit test만 있는 기능은 단위 완료 이상으로 올리지 않는다.
- 실제 provider 독립 test만 있는 기능은 Provider 통합 이상으로 올리지 않는다.
- Public facade에서 도달할 수 없는 구현은 Production 통합으로 보지 않는다.
- Manual E2E는 실행 OS/build/signing identity와 artifact/log 위치를 남긴다.
- 완료 정의를 변경하면 해당 protocol/layout/schema version과 support matrix 영향을 함께 기록한다.

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

## 현재 권장 착수 순서

1. Clean CI baseline과 JSC flaky 제거
2. Protocol/validation/handshake 구현
3. Session/session-machine/runtime ownership 구현
4. IOSurface/Darwin production channel adapter 구현
5. Public facade와 peer/renderer transport 연결
6. WebKit E2E를 production path로 교체
7. Diagnostics/metrics와 failure snapshot 구현
8. Cross-architecture/property/fuzz/process CI 강화
9. 남은 Phase 8 확장 재개
