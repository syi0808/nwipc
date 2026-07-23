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
이 snapshot은 runtime, JSC binding, event loop와 process entry를 Cargo workspace의 Rust crate로
구성한다. 특히 `bun_runtime`이 `bun_event_loop`와 `bun_jsc`를 직접 의존하고 event loop가 task enqueue,
timer와 native poll 경계를 제공하므로 NWIPC를 별도 언어/ABI bridge 없이 Rust dependency로 편입할 수
있다.

## 현재 SDK 지원성 판정

**결론: 현재 NWIPC crate는 Rust로 포팅된 Bun의 macOS native integration을 구현하기에 충분하다.**

완성된 Bun용 고수준 facade는 없지만 이는 가능성을 막는 SDK 결함이 아니다. Bun 내부 crate가 공개된
lower-level contract를 조립하면 protocol부터 Mach transport까지 native 경로를 만들 수 있다. 남은
작업은 새로운 NWIPC adapter가 아니라 Bun runtime의 bootstrap, event-loop task와 JSC binding을
연결하는 product integration이다.

| 경계 | 현재 상태 | 판정 |
|---|---|---|
| Wire/protocol/fragment/crypto | engine과 executor에 독립적 | 그대로 재사용 가능 |
| Native peer state machine | `nwipc-peer-core::NativePort`와 `PeerPort`가 sync/nonblocking contract 제공 | 그대로 재사용 가능 |
| macOS production transport | `MacosEndpointTransport::attach`와 Mach provider가 공개됨 | lower-level 조립으로 재사용 가능 |
| Bootstrap decode | `nwipc-bootstrap-codec::decode`와 `PeerExpectation`/`NativePort::accept`가 공개됨 | Bun 내부에서 직접 조립 가능 |
| Runtime integration | `PeerPort`가 sync/nonblocking이고 `nwipc-peer-async`가 readiness 의미론 제공 | Bun Rust event loop에 직접 연결 가능 |
| JavaScript binding | Bun이 `bun_jsc`와 generated native binding을 Rust crate로 소유 | Bun 내부 native binding으로 구현 가능 |
| Distribution | crate version이 `0.0.0`이고 workspace path dependency 중심 | Bun fork에서 vendoring 또는 source pin 필요 |
| Windows/Linux native path | production facade가 Mach provider만 선택 | 지원하지 않음 |

`nwipc-peer::Peer::initialize()`가 env/stdin을 소비하는 것은 CLI facade의 제약일 뿐이다. Bun 내부
crate는 bootstrap bytes를 decode한 뒤 `MacosEndpointTransport::attach`와 `NativePort::accept`를
직접 호출할 수 있으므로 별도 NWIPC API를 기다릴 필요가 없다. `nwipc-renderer-jsc`도 사용할 필요가
없으며 Bun의 기존 Rust JSC binding/codegen 경계에 같은 `PeerPort`를 연결하면 된다.

## Bun 내부 구현 경계

### B0 — Native peer assembly

- `bun_runtime` 내부 NWIPC module이 owned bootstrap bytes와 identity를 받는다.
- `nwipc-bootstrap-codec::decode`, `MacosEndpointTransport::attach`,
  `NativePort::accept`를 한 번만 조립한다.
- bootstrap secret과 provider descriptor는 생성 직후 Bun JS object에서 접근할 수 없게 한다.
- endpoint registry는 VM과 generation을 key로 사용한다.

완료 기준은 in-memory bootstrap, malformed/stale bootstrap, duplicate consumption과 cleanup contract
test다. 이 조립을 반복할 consumer가 생기면 이후 `Peer::from_bootstrap` 같은 convenience API로
NWIPC에 올릴 수 있지만 Bun integration의 선행 조건은 아니다.

### B1 — Rust event-loop progress

- Bun `bun_event_loop`의 task enqueue와 native timer/poll 경계에서 `PeerPort`를 drive한다.
- 등록 직후 operation을 다시 확인하여 signal edge와 등록 사이의 lost wake-up을 막는다.
- concurrent callback은 JS value를 만지지 않고 Bun JS thread에 generation-tagged task만 enqueue한다.
- correctness poll은 Bun native timer가 수행하며 busy loop나 JavaScript timer를 사용하지 않는다.

현재 API만으로도 bounded native correctness poll을 사용한 integration이 가능하다. Mach receive
right를 Bun native poller에 직접 등록하는 zero-idle-poll 최적화는 별도 provider readiness handle이
필요하며, 이는 production 성능 개선 게이트이지 기능 prototype의 blocker는 아니다.

### B2 — Stable ownership surface

- endpoint는 Bun VM/isolate 또는 equivalent runtime owner에 귀속하고 thread affinity를 명시한다.
- close, VM shutdown, process exit와 initialization failure가 모두 idempotent cleanup으로 수렴한다.
- JS에 노출하는 오류는 redacted `ErrorCode`/recoverability만 포함한다.
- NWIPC crate set과 revision을 하나의 source pin으로 소비할 수 있게 한다.

## Bun fork 구현 순서

1. Bun Cargo workspace에 필요한 NWIPC crate를 source-pinned dependency로 추가한다.
2. `bun_runtime`에 generation-scoped native peer registry와 B0 assembly를 둔다.
3. `bun_jsc` codegen 경계에 binary send, receive callback, state와 close를 노출한다.
4. `bun_event_loop`에 B1 native readiness/drive hook을 등록한다.
5. VM teardown과 endpoint replacement에서 stale task를 generation으로 폐기한다.
6. macOS arm64/x86_64 two-process E2E와 crash/reload matrix를 수행한다.

정상 payload가 stdin/stdout, Node-API 또는 JavaScript callback relay를 한 번 더 통과하면 목표
경로가 아니다. JS boundary의 `Uint8Array` copy는 허용하지만 native transport가 검증한 message
boundary와 backpressure를 Bun binding이 재해석해서는 안 된다.

## 완료 게이트

- Bun binary 안에 NWIPC가 정적으로 링크되고 별도 addon/adapter artifact가 없다.
- Bootstrap 이후 payload가 NWIPC shared-memory channel만 사용한다.
- Bun event loop thread를 block하지 않으며 idle 상태에서 busy polling하지 않는다.
- Backpressure, lost/duplicate hint, malformed bootstrap, stale generation이 typed failure로 끝난다.
- Bun VM shutdown, peer crash와 repeated close에서 native handle과 mapping이 남지 않는다.
- Bun upstream pin 변경 시 compile, contract test와 macOS process E2E를 다시 수행한다.

현재 NWIPC 측 준비 상태는 **native integration 가능**이다. Bun fork에서 위 게이트를 닫기 전의 제품
상태는 **경계 정의** 또는 **Provider 통합 prototype**이며 production integration으로 표시하지 않는다.
