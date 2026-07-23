# Mach-only 전환 기준선과 동등성 계약

이 문서는 [Mach-only 전환 계획](implementation-plan/12-mach-only-migration.md)의 MMP0
canonical checklist다. 이후 단계는 항목을 삭제하거나 의미를 완화하지 않고, 같은 candidate
commit에서 legacy production path와 Mach replacement의 결과를 비교한다.

## Candidate와 증거 고정

기준선은 branch 이름이나 최근 성공 결과가 아니라 full commit SHA로 식별한다. MMP0 candidate는
다음 세 실행을 동일 SHA에서 통과해야 한다.

```sh
cargo test \
  -p nwipc-memory-iosurface -p nwipc-memory-mach \
  -p nwipc-signal-darwin -p nwipc-signal-mach \
  -p nwipc-signal-hybrid -p nwipc-mach-transfer \
  -p nwipc-channel-transport
cargo test --release -p nwipc-macos-transport \
  --test actual_provider_benchmark -- --ignored --nocapture
cargo xtask webkit-e2e
```

Release candidate 증거는 `Release Gate`의 `actual-provider-arm64`,
`actual-provider-x86_64`, `signed-webkit-e2e`, `release-record` artifact로 고정한다. 각 artifact는
full candidate SHA, OS/build, architecture, runner class와 실행 명령을 포함해야 한다. 로컬 실행은
개발 기준선으로 사용할 수 있지만 immutable release 증거를 대신하지 않는다.

Actual-provider benchmark의 고정 configuration은 방향별 2 MiB ring, 16 KiB inline limit,
1 MiB logical-message limit, 512 KiB/1536 KiB low/high watermark다. 측정 case는 64 B × 2,000,
1,024 B × 1,000, 16,384 B × 200이며 결과에는 mean round-trip, throughput과 transport
diagnostics를 남긴다. 숫자는 같은 runner class와 power profile끼리만 비교하고, MMP4에서
regression budget을 합의하기 전에는 pass/fail threshold로 사용하지 않는다.

## 동등성 계약

아래 목록은 Mach production vertical slice가 전부 통과해야 하는 단일 source of truth다.

| 계층 | 고정 contract | 현재 증거 |
|---|---|---|
| Memory API | zero/overflow range 거부, generation 일치, read/write와 acquire/release atomic access, logical length 검증, read-only write 거부, descriptor/debug redaction | `create_attach_and_access_contract`, `create_attach_and_native_protection_contract`, 두 provider descriptor test |
| Cross-process memory | 별도 process의 raw byte/cursor visibility, owner mapping 종료 전 attach, mapping drop 후 bounded native cleanup | `two_process_raw_byte_visibility`, `two_process_memory_entry_visibility` |
| Signal API | generation/direction 일치, single-listener ownership, cancellation, coalescing, timeout, closed provider의 typed error, descriptor/debug redaction | Darwin/Mach same-process tests, Mach `broker_exit_delivers_no_senders_notification` |
| Cross-process signal | 별도 process notification, duplicate/coalesced hint가 delivery 의미론을 바꾸지 않음 | 양 provider `two_process_notification_delivery` |
| Correctness polling | primary hint 유실과 delay에도 shared cursor drain으로 bounded progress, idle busy-loop 금지 | `dropped_primary_is_recovered_by_bounded_poll`, `backs_off_and_resets_without_busy_loop` |
| Common channel | 양방향 FIFO, fragmentation/reassembly, atomic fragmented backpressure, writable edge, close FIFO/terminal, incomplete fragment 폐기, dropped/duplicate hint recovery | `nwipc-channel-core`, `nwipc-channel-transport` contract suite |
| Protocol/security | layout/record/bootstrap fixed-width validation, HELLO/ACK, session/generation/role binding, 방향별 AEAD, tamper/replay/wrong secret 거부 | protocol fixture, validation, crypto와 production transport tests |
| Public facade | bootstrap pipe는 control plane만 전달, renderer↔peer direct echo, payload host relay 금지, stale handle 거부, replacement와 cleanup diagnostics | `public_facade_connects_renderer_and_peer_without_payload_stream`, facade lifecycle tests |
| Production WebKit | zero/exact-inline/fragmented/maximum payload, saturation/writable recovery, notification drop/duplicate/delay, commit 전후 writer exit, peer kill, reload/WebContent replacement, stale completion 차단 | signed/hardened `cargo xtask webkit-e2e` matrix |
| Unsupported path | provider 준비/attach 실패, SPI/signing/sandbox denial과 malformed descriptor를 silent fallback 없이 stable typed error로 거부 | provider, transport, WebKit failure tests |

Mach provider가 아직 common channel/public facade/production WebKit 행에 연결되지 않았다는 사실은
MMP1–MMP3의 작업 범위다. MMP0 통과는 이 미구현을 동등성 목록에서 제외한다는 뜻이 아니다.

## Fault matrix

| Fault | 불변 기대 결과 |
|---|---|
| partial/malformed bootstrap 또는 wrong role/generation | attach 전 typed failure, 부분 resource 역순 cleanup |
| commit 전 writer exit | reader cursor와 application delivery에 partial bytes 비노출 |
| commit 후 signal 전 writer exit | complete authenticated frame을 polling으로 한 번만 전달 |
| dropped/duplicate/coalesced/delayed signal | shared cursor 기준 progress, duplicate application delivery 없음 |
| full ring/port queue | bounded backpressure, low watermark 아래 writable edge 한 번 |
| listener/provider death | bounded wake/close, deadlock과 stale callback 없음 |
| peer/WebContent kill 또는 reload | 이전 generation cleanup, 새 generation만 routing |
| repeated close/partial attach | idempotent cleanup, double-free/right over-release 없음 |
| unsupported OS/SPI/entitlement | silent provider fallback 없이 typed fail-closed |

## Diagnostics 동등성과 차이

Public diagnostics schema v2의 다음 필드는 provider 전환 중 의미와 redaction을 유지한다.

- `session_id`, `generation`, `state`, `topology`, `capabilities`
- `memory_backend`, `signal_backend`
- `last_error`, `last_failure`, `resources_cleaned`, `cleanup`
- bytes/messages, backpressure/writable, primary/poll/coalesced/recovery/signal-failure,
  validation/authentication/failure/replacement counters

`memory_backend`은 `IoSurface`에서 `Mach`로, `signal_backend`은 `Hybrid`에서 `Mach`로 바뀔 수 있다.
Mach primary에도 provider-neutral correctness polling을 조합하므로 primary/polling counter 의미는
바뀌지 않는다. Payload, secret, notification/service name, IOSurface ID, Mach port name/right,
mapping address와 provider source error는 어느 diagnostics/log에도 추가하지 않는다.

Provider-local 차이는 다음과 같이 명시적으로 유지한다.

| 관찰 항목 | IOSurface + Darwin/hybrid | Mach memory + port |
|---|---|---|
| Memory descriptor 전달 | global IOSurface ID를 opaque bootstrap bytes로 전달 | memory-entry send right를 인증된 Mach control message descriptor로 전달 |
| Read-only 강제 | safe API access 검사; native mapping은 강제하지 않음 | `mach_vm_map` protection으로 native read-only 강제 |
| Signal endpoint | deterministic Darwin name과 process-local registration token | 방향별 send/receive right, receive right는 single-listener move |
| Coalescing | Darwin notification merge 가능 | full port queue에서 hint merge 가능 |
| Death 관찰 | cancel/provider failure와 polling 상태로 판정 | no-senders/port-death와 polling 상태로 판정 |
| 공개 provider 진단 | coalescing, correctness-poll-required, cross-process | coalescing, capability-transfer, cross-process |

## Cleanup ownership

| Owner | IOSurface + Darwin/hybrid 기준선 | Mach replacement 의무 |
|---|---|---|
| Host generation | 두 owner IOSurface mapping을 generation 종료까지 보유 | 두 owner VM allocation/mapping, memory-entry right와 방향별 signal resource를 보유 |
| Endpoint outbound/inbound | lookup한 IOSurface references와 Darwin sender/listener token | attach한 VM mappings, send rights와 이동된 receive right |
| Provider worker/control | 별도 broker 없음 | capability transfer 완료/실패까지 control receive right와 worker를 보유 |
| Partial attach | 이미 생성한 mapping/listener가 scope drop으로 정리 | 받은 right와 mapping을 획득 역순으로 한 번만 정리 |
| Normal close/replacement | listener cancel, mapping/reference drop, generation cleanup status 기록 | listener cancel/receive-right destroy, send-right deallocate, VM unmap, host resource teardown 후 cleanup status 기록 |

Host는 이 lifecycle capability만 소유하며 application payload를 읽거나 복사하거나 relay하지 않는다.
MMP1 이후에는 global bootstrap registration/lookup ownership이 production call graph에 남아서는 안
된다.

## 변경 경계

| 변경 가능 | 변경 금지 |
|---|---|
| provider tag와 descriptor payload 형식 | provider 숫자의 재사용 |
| memory allocation/mapping 및 signal primitive | region header, record layout와 cursor publication 순서 |
| capability transfer control plane과 cleanup 순서 | FIFO, fragmentation, backpressure와 close/reset 의미론 |
| backend diagnostics enum 값과 provider-local capability fields | diagnostics counter 의미와 redaction 정책 |
| provider-specific error가 매핑되는 stable stage/code | HELLO/ACK, session/generation/role 검증 |
| benchmark의 provider label과 native resource counts | 방향별 authenticated encryption과 replay protection |
| signing/entitlement 준비와 supported-system gate | direct renderer↔peer topology와 host payload non-relay |

이 표의 변경 금지 항목을 수정해야 한다면 Mach migration의 일부로 처리하지 않고 별도 protocol 또는
diagnostics version 변경, ADR과 양 provider 회귀 증거를 먼저 추가한다.
