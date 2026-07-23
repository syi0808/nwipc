# Native peer 모듈 구현계획

## 범위

대상은 `nwipc-peer-core`, `nwipc-peer`와 native peer example이다. Peer는 WebView/framework dependency 없이 독립 실행 가능해야 한다.

## `nwipc-peer-core`

구현:

- Bootstrap consume → provider attach → handshake → `NativePort`
- `try_send`, `try_receive`, `close`
- Port state, capability, diagnostics
- Backpressure를 정상 status로 반환하고 failure와 구분
- Thread/runtime-neutral synchronous core

## `nwipc-peer`

구현:

- Inherited descriptor에서 bootstrap을 얻는 public facade
- Safe defaults와 public error conversion
- Provider 선택/attach의 platform adapter 연결
- Minimal native peer echo example

## Process 통합

- Parent가 one-shot pipe와 region/signal descriptors를 준비한다.
- Child만 bootstrap descriptor를 상속한다.
- Child attach 완료 후 HELLO/ACK를 수행한다.
- Exit/timeout 때 runtime으로 lifecycle event를 전달한다.

## 검증

- Fake provider in-process port test
- 두 native process 양방향 binary echo
- Backpressure와 writable recovery
- Partial bootstrap, stale generation, invalid provider
- Peer kill/restart, before/after commit crash
- Graceful close timeout과 resource cleanup
- WebView/framework dependency 부재 검사

## 확장

- Borrowed receive는 `Peer::try_receive_borrowed`로 제공한다.
- Bun-specific adapter crate는 만들지 않는다. Bun Rust runtime은 공개된 bootstrap codec,
  `NativePort`와 native transport를 직접 조립하며 구현 게이트는
  [`13-bun-native-integration.md`](13-bun-native-integration.md)를 따른다.

## Async/Tokio 확장

- `nwipc-peer-async`는 synchronous `PeerPort`와 edge-loss 없는 `Readiness` registration을 조합한다.
- `send`, `receive`, `close`는 readiness 등록 뒤 nonblocking operation을 다시 확인해 lost wake-up을
  방지하고 `Backpressured`만 writable 대기로 변환한다.
- `nwipc-peer-tokio`는 `Notify` 기반 readable/writable hint와 bounded correctness poll을 제공한다.
- 두 adapter는 executor, task, thread를 소유하지 않으며 기존 `Peer`와 production transport를 그대로
  사용한다.

검증은 fake port의 backpressure/event 계약, callback hint와 polling recovery, zero interval 거부,
production macOS transport의 nonblocking receive 경계를 포함한다.

## 완료 기준

- 별도 native process가 attach/send/receive/close를 수행한다.
- Crash와 restart에서 stale message가 전달되지 않는다.
- Peer public API가 platform native descriptor와 core implementation detail을 노출하지 않는다.
