# Renderer·TypeScript client 구현계획

## 범위

대상은 `nwipc-renderer-api`, `nwipc-renderer-core`, `nwipc-renderer-jsc`, `@nwipc/client-core`, `@nwipc/client`, `@nwipc/client-testkit`이다. WebView lifecycle과 JS engine binding을 분리한다.

## Renderer API/core

구현:

- JS engine-independent port state
- Connect/send/receive/writable/close/error dispatch
- Callback registry와 event queue
- Document generation-scoped invalidation
- Signal callback과 JS-thread dispatch 분리

검증:

- Mock binding state-machine test
- Stale document callback 무시
- Close 중 reentrancy와 callback 제거
- Signal loss/duplicate와 event ordering

## `nwipc-renderer-jsc`

구현:

- Frozen `globalThis.__nwipc` native binding
- `Uint8Array` argument 검증
- 첫 slice의 JS-owned receive buffer copy
- Callback protect/unprotect RAII
- JSC exception ↔ Rust error 변환
- JSC/main-thread affinity assertion

금지:

- WebKit page lifecycle
- WKBundle API
- IOSurface 생성과 Darwin registration

검증:

- Wrong type, detached/invalid buffer
- Context teardown 이후 callback 없음
- 반복 connect/close의 protect leak 없음
- Exception/panic이 FFI boundary를 넘지 않음

## TypeScript packages

`@nwipc/client-core`:

- `NativeMessagePort` state machine
- `postMessage`, `close`, event ordering
- `bufferedAmount`, `writable` promise
- Native binding contract

`@nwipc/client`:

- `globalThis.__nwipc.connect()` application facade
- Binding 부재 시 explicit unsupported error

`@nwipc/client-testkit`:

- Mock native binding
- Deterministic readable/writable/close/error driver
- Rust renderer behavior와 공유하는 contract fixture

## 검증

- Event ordering과 reentrancy
- Backpressure/writable promise
- Close/error terminal semantics
- Browser-compatible type surface
- Mock ↔ native binding contract 일치

## 완료 기준

- Renderer core와 TS state machine은 WebKit 없이 테스트된다.
- JSC context에서 connect/send/receive/close가 동작한다.
- Document teardown 뒤 stale port 사용과 callback을 차단한다.

