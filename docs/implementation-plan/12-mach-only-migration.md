# Mach-only production provider 전환 계획

## 결정 방향

macOS product provider의 목표 구성을 다음으로 단순화한다.

- Memory: Mach VM memory entry
- Primary signal: Mach port
- Correctness fallback: provider-neutral bounded polling
- Test provider: fake/in-process/process-test

IOSurface와 Darwin Notify는 Mach production vertical slice가 완료될 때까지 비교 기준과 rollback
artifact로 유지한 뒤 product implementation에서 제거한다. Polling은 signal provider가 아니라 shared
cursor를 기준으로 progress를 복구하는 correctness 계층이므로 제거하지 않는다. Fake와 process-test도
portable CI 및 공통 contract 검증을 위해 유지한다.

최종 제품은 runtime에서 IOSurface/Darwin으로 자동 fallback하지 않는다. Mach provider를 준비하거나
attach할 수 없으면 stable typed error로 fail closed한다.

## 현재 기준선

현재 production WebKit call graph는 `nwipc-macos-transport`를 통해 IOSurface memory와 Darwin
Notify/hybrid signal을 사용한다. 이 경로는 signed/hardened WebKit E2E, crash/reload matrix와
actual-provider benchmark의 기준선이다.

Mach provider는 다음 독립 contract를 완료했다.

- `nwipc-memory-mach`: VM allocation, memory-entry right 전달, native access protection, two-process
  byte visibility
- `nwipc-signal-mach`: send/receive right 전달, 단일 listener, coalescing, no-senders/port-death
  lifecycle, two-process notification
- Bootstrap schema와 runtime: experimental Mach provider tag와 selection boundary

아직 Mach provider는 public `nwipc::Session`에서 WebKit renderer와 native peer가 사용하는 production
transport에 연결되지 않았다. 현재 right 전달 rendezvous는 `bootstrap_register`를 사용하므로 이를
그대로 sandboxed WebContent production contract로 승격하지 않는다.

## 전환 원칙

1. 기존 provider 삭제보다 Mach vertical slice 폐쇄를 먼저 수행한다.
2. Task-local Mach port 번호를 bootstrap bytes나 diagnostics에 기록하지 않는다.
3. Right는 인증된 control plane에서 Mach message descriptor로 전달한다.
4. Host는 capability lifecycle만 소유하고 application payload를 relay하지 않는다.
5. HELLO/ACK, authenticated encryption, fragmentation, backpressure 의미론은 provider 교체로
   변경하지 않는다.
6. Signal은 hint로만 취급하고 정확한 상태는 shared cursor에서 확인한다.
7. Provider 실패 시 silent fallback이나 부분 attach를 허용하지 않는다.
8. 기존 wire provider 숫자는 구현 제거 뒤에도 재사용하지 않는다.

## 단계별 전환

### MMP0 — 기준선과 동등성 목록 고정

- IOSurface/Darwin production path의 contract, fault matrix, benchmark 결과를 candidate 기준선으로 고정
- Mach가 통과해야 하는 memory/signal/channel/public facade contract 목록을 한 곳에 기록
- 양쪽 provider의 diagnostics 필드와 cleanup ownership 차이를 명시
- Canonical checklist와 증거 형식은
  [`mach-migration-baseline.md`](../mach-migration-baseline.md)에 고정

완료 조건:

- 동일 commit에서 기존 production E2E와 Mach 독립 two-process contract가 모두 통과한다.
- Mach 전환 때문에 변경 가능한 항목과 변경하면 안 되는 protocol/data-plane 항목이 구분된다.

### MMP1 — Production capability transfer

- 전역 bootstrap name lookup 대신 host와 endpoint 사이의 선행 인증된 Mach control endpoint를 준비
- Memory-entry send right와 방향별 signal send/receive right를 Mach message descriptor로 전달
- One-shot transfer, endpoint role, session/generation과 descriptor metadata를 함께 검증
- Partial transfer 실패 시 이미 받은 right와 mapping을 역순으로 단일 cleanup
- Duplicate listener, replayed transfer, stale generation과 wrong endpoint role을 attach 전에 거부

완료 조건:

- `bootstrap_register`/`bootstrap_look_up`가 production call graph에 없다.
- Numeric port name이 wire, log, diagnostics, property list에 나타나지 않는다.
- Renderer와 peer가 host payload relay 없이 필요한 right를 소유한다.

구현 증거:

- `nwipc-mach-transfer`가 사전 인증된 control receive right를 입력으로 받아 memory-entry send
  right 2개와 방향별 signal send/receive right를 하나의 complex Mach message로 이동한다.
- `transfers_exact_capabilities_once_without_bootstrap_lookup`,
  `wrong_role_and_stale_generation_fail_before_transfer`,
  `failed_native_send_cleans_capabilities_and_closes_one_shot_endpoint`가 canonical descriptor set,
  one-shot/replay, role/session/generation 검증, 역순 실패 cleanup과 redaction을 고정한다.
- `cargo xtask architecture-check`가 transfer crate에 `bootstrap_register`, `bootstrap_look_up`,
  `bootstrap_port` token이 다시 들어오는 것을 차단한다.
- 실제 `PreparedMacosTransport`와 renderer/peer attach 연결은 MMP2에서 이 primitive를 소비하며,
  그 연결 전에는 MMP1 production call graph 완료로 판정하지 않는다.

### MMP2 — Provider-neutral transport 연결

- `PreparedMacosTransport`가 Mach memory 두 방향과 Mach signal 두 방향을 generation resource로 소유
- `MacosEndpointTransport`가 Mach descriptor를 attach해 기존 mapped channel adapter를 그대로 사용
- Peer와 injected bundle이 `ProviderKind::MachMemory`/`MachPort`를 실제 production path에서 소비
- Public facade와 runtime preparation을 `ProviderSelection::MACH`에 연결
- HELLO/ACK와 application frame 모두 기존 authenticated encryption 경로를 유지

완료 조건:

- Public `nwipc::Session` 양방향 echo가 Mach provider만 사용한다.
- IOSurface/Darwin과 동일한 FIFO, fragmentation, backpressure, close/reset contract suite가 통과한다.
- Production source와 diagnostics에 fallback provider 선택이 없다.

### MMP3 — Signed/hardened WebKit 검증

- AppKit host, injected bundle, native peer를 실제 signing/hardened runtime/sandbox 조건으로 실행
- Initial attach, reload, WebContent kill, peer kill, generation replacement와 stale callback을 검증
- Commit 전/후 writer crash, dropped/coalesced signal, port death와 no-senders fault를 검증
- 지원 macOS/architecture matrix에서 entitlement와 sandbox denial을 fail-closed로 확인

완료 조건:

- 기존 signed WebKit E2E fault matrix가 Mach-only 경로로 통과한다.
- Deadlock, leaked right, stale mapping/callback, host payload relay가 없다.
- Unsupported OS/entitlement는 typed error와 generation cleanup을 남긴다.

### MMP4 — 성능과 운영 동등성

- 동일 runner/configuration에서 IOSurface/Darwin과 Mach의 latency, throughput, saturation을 비교
- Mapping/right 수, primary/poll wake, recovery, cleanup failure를 diagnostics에 추가
- Idle polling, signal storm, full port queue와 multi-session stress를 측정
- Release gate의 actual-provider contract와 benchmark 대상을 Mach로 전환

완료 조건:

- 합의한 regression budget 안에서 baseline이 반복 가능하다.
- Diagnostics에 native handle/name/secret이 노출되지 않는다.
- arm64/x86_64 provider gate와 supported WebKit gate가 같은 candidate에서 통과한다.

### MMP5 — 기본값 전환과 안정화

- Public runtime의 production provider를 Mach로 변경
- IOSurface/Darwin은 명시적인 legacy build/test 경로에서만 유지
- 일정 기간 CI와 release candidate에서 Mach-only 결과를 관찰
- 문서, example, support matrix와 failure guidance를 Mach 기준으로 변경

완료 조건:

- Default build와 examples가 IOSurface/Darwin crate에 의존하지 않는다.
- 두 차례 이상의 release candidate에서 Mach-only required gate가 clean하다.
- Rollback은 runtime fallback이 아니라 이전 검증 release로 수행할 수 있다.

### MMP6 — Legacy provider 제거

- `nwipc-memory-iosurface`, `nwipc-signal-darwin` 구현과 macOS transport 분기를 제거
- Darwin 전용 hybrid wrapper를 제거하고 Mach primary와 generic polling 조합만 유지
- Runtime/diagnostics의 legacy selection을 제거
- 기존 provider wire 숫자는 reserved 처리하고 새 의미로 재사용하지 않음
- IOSurface/Darwin 전용 unsafe audit baseline, CI job, benchmark와 문서를 제거

완료 조건:

- Dependency graph와 production binary에 IOSurface/Darwin symbol이 없다.
- Mach-only workspace test, architecture check, unsafe audit와 release gate가 통과한다.
- Legacy descriptor 입력은 오해석 없이 typed `Unsupported` 또는 protocol error로 거부된다.

## 필수 검증 게이트

| Gate | Mach-only 전환 요구사항 |
|---|---|
| Provider contract | range/access/generation, right ownership, two-process visibility/notification |
| Common channel | FIFO, fragmentation, backpressure, close/reset, bounded polling recovery |
| Security | authenticated transfer, descriptor redaction, replay/wrong-role 거부, unsafe audit |
| Lifecycle | partial attach, duplicate close, reload/kill, no-senders/port death, stale generation |
| Production E2E | signed/hardened WebKit와 native peer의 host-relay 없는 양방향 echo |
| Cross architecture | macOS arm64/x86_64 actual-provider contract |
| Performance | 같은 runner에서 baseline 비교와 합의된 regression budget |
| Operations | stable diagnostics, cleanup evidence, explicit unsupported/fail-closed |

MMP1–MMP4 중 하나라도 충족하지 못하면 IOSurface/Darwin을 삭제하지 않는다. MMP5 이후의 rollback은
provider 자동 선택을 되살리는 방식이 아니라 마지막으로 검증된 release를 재배포하는 방식으로 수행한다.
