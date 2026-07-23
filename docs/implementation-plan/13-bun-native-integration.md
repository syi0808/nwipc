# Bun source 내부 native integration

## 의도와 비범위

목표는 `nwipc-bun` adapter, Node-API addon 또는 `bun:ffi` package를 배포하는 것이 아니다. Bun
source tree의 Rust crate graph에 NWIPC crate를 직접 의존시키고 Bun 자체 native binding과 event
loop가 peer endpoint를 소유하게 한다.

다음은 이 계획의 범위가 아니다.

- `nwipc-renderer-jsc`를 Bun에 그대로 주입
- JavaScript polling loop 또는 framework IPC를 payload fallback으로 사용
- Bun upstream과 독립적으로 로드되는 `cdylib`
- Bun 작업에 Windows/Linux provider 구현을 묶는 것

판정 기준 Bun upstream snapshot은 2026-07-23의
[`892b1dabc69e2a0a973244f772b84967c73ccad5`](https://github.com/oven-sh/bun/tree/892b1dabc69e2a0a973244f772b84967c73ccad5)다.
이 snapshot은 Cargo workspace와 최종 binary에 링크되는 `bun_bin` Rust `staticlib`을 가지므로
NWIPC를 Rust dependency로 직접 편입할 구조적 위치는 존재한다.

## 현재 SDK 지원성 판정

**결론: macOS prototype의 data plane과 protocol은 지원하지만, Bun source에 바로 연결할 수 있는
완성된 embedding SDK는 아직 아니다.**

| 경계 | 현재 상태 | 판정 |
|---|---|---|
| Wire/protocol/fragment/crypto | engine과 executor에 독립적 | 그대로 재사용 가능 |
| Native peer state machine | `nwipc-peer-core::NativePort`와 `PeerPort`가 sync/nonblocking contract 제공 | 그대로 재사용 가능 |
| macOS production transport | `MacosEndpointTransport::attach`와 Mach provider가 공개됨 | lower-level 조립으로 재사용 가능 |
| Runtime integration | executor를 소유하지 않는 `nwipc-peer-async` readiness contract 제공 | Bun 전용 readiness 구현 필요 |
| Public peer bootstrap | `nwipc-peer::Peer::initialize`가 env와 stdin을 직접 소비 | embedded Bun에는 부적합 |
| JavaScript binding | `nwipc-renderer-jsc`가 macOS JavaScriptCore C API와 renderer lifecycle에 결합 | Bun binding으로 대체 필요 |
| Distribution | crate version이 `0.0.0`이고 workspace path dependency 중심 | Bun fork에서 vendoring 또는 source pin 필요 |
| Windows/Linux native path | production facade가 Mach provider만 선택 | 지원하지 않음 |

따라서 NWIPC의 protocol/channel/peer/provider를 다시 작성할 필요는 없다. 반면 `Peer::initialize()`를
Bun startup에서 호출하거나 `nwipc-renderer-jsc`를 링크하는 방식은 native integration으로 인정하지
않는다. Bun이 bootstrap byte 수신, endpoint 생성, event-loop wake-up, JS object lifecycle을 직접
연결할 작은 embedding surface가 먼저 필요하다.

## 필요한 NWIPC 선행 변경

### B0 — Embedding bootstrap

- env/stdin 없이 owned bootstrap bytes와 명시적 `PeerExpectation`을 받는 peer constructor를 제공한다.
- constructor 내부에서 decode, generation 검증, Mach endpoint attach와 HELLO/ACK를 수행한다.
- bootstrap secret과 provider descriptor는 성공과 실패 모두에서 호출자에게 재노출하지 않는다.
- 기존 CLI용 `Peer::initialize()`는 새 constructor 위의 얇은 wrapper로 유지한다.

완료 기준은 in-memory bootstrap, malformed/stale bootstrap, duplicate consumption과 cleanup contract
test다. Bun이 `MacosEndpointTransport`와 `NativePort`를 직접 조립하는 임시 구현은 prototype 증거일
뿐 public embedding contract 완료로 보지 않는다.

### B1 — Host-driven progress

- Bun event loop가 소유할 수 있는 readiness registration 또는 bounded `drive()` contract를 제공한다.
- 등록 직후 operation을 다시 확인하여 signal edge와 등록 사이의 lost wake-up을 막는다.
- callback은 JS value를 만지지 않고 Bun JS thread에 generation-tagged task만 enqueue한다.
- correctness poll은 native hint 유실 복구용이며 busy loop나 JS timer가 아니다.

현재 `nwipc-peer-async::Readiness`는 필요한 의미론을 정의하지만 Mach transport의 receive right나
host callback registration을 노출하지 않는다. 이 경계를 닫기 전에는 Bun event-loop integration이
완료됐다고 판정하지 않는다.

### B2 — Stable ownership surface

- endpoint는 Bun VM/isolate 또는 equivalent runtime owner에 귀속하고 thread affinity를 명시한다.
- close, VM shutdown, process exit와 initialization failure가 모두 idempotent cleanup으로 수렴한다.
- JS에 노출하는 오류는 redacted `ErrorCode`/recoverability만 포함한다.
- NWIPC crate set과 revision을 하나의 source pin으로 소비할 수 있게 한다.

## Bun fork 구현 순서

1. Bun Cargo workspace에 필요한 NWIPC crate를 source-pinned dependency로 추가한다.
2. Bun runtime owner에 generation-scoped native peer registry를 둔다.
3. Bun native binding에서 binary send, receive callback, buffered state와 close만 노출한다.
4. Bun event loop에 B1 readiness/drive hook을 등록하고 JS thread로 delivery task를 보낸다.
5. VM teardown과 endpoint replacement에서 stale task를 generation으로 폐기한다.
6. macOS arm64/x86_64 two-process E2E와 crash/reload matrix를 수행한다.

정상 payload가 Zig/C++ stream, stdin/stdout, Node-API 또는 JS callback relay를 한 번 더 통과하면
목표 경로가 아니다. JS boundary의 `Uint8Array` copy는 허용하지만 native transport가 검증한 message
boundary와 backpressure를 Bun binding이 재해석해서는 안 된다.

## 완료 게이트

- Bun binary 안에 NWIPC가 정적으로 링크되고 별도 addon/adapter artifact가 없다.
- Bootstrap 이후 payload가 NWIPC shared-memory channel만 사용한다.
- Bun event loop thread를 block하지 않으며 idle 상태에서 busy polling하지 않는다.
- Backpressure, lost/duplicate hint, malformed bootstrap, stale generation이 typed failure로 끝난다.
- Bun VM shutdown, peer crash와 repeated close에서 native handle과 mapping이 남지 않는다.
- Bun upstream pin 변경 시 compile, contract test와 macOS process E2E를 다시 수행한다.

이 게이트 전의 정확한 상태 표기는 **경계 정의** 또는 **Provider 통합 prototype**이다. Bun native
production integration으로 표시하지 않는다.
