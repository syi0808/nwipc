# NWIPC 구현계획 문서 모음

이 디렉터리는 [`1차설계.md`](../../1차설계.md)를 실행 가능한 프로젝트로 옮기기 위한 계획을 책임 경계별로 나눈다. 최초 목표는 macOS AppKit/WKWebView renderer와 별도 native peer 사이의 host-relay 없는 양방향 shared-memory IPC vertical slice다.

## 문서 구조

| 문서 | 범위 |
|---|---|
| [00-scaffold.md](00-scaffold.md) | 저장소, Cargo/pnpm workspace, crate skeleton, feature, CI |
| [01-domain.md](01-domain.md) | types, error, capabilities, state |
| [02-protocol.md](02-protocol.md) | layout, record, protocol, validation |
| [03-data-plane.md](03-data-plane.md) | atomic, ring, flow control, channel |
| [04-memory.md](04-memory.md) | memory API, region, IOSurface |
| [05-signal.md](05-signal.md) | signal API, coalescing, Darwin Notify, polling |
| [06-bootstrap-runtime.md](06-bootstrap-runtime.md) | bootstrap, session, session machine, runtime |
| [07-peer.md](07-peer.md) | native peer core/facade와 process bootstrap |
| [08-renderer-client.md](08-renderer-client.md) | renderer core, JSC, TypeScript client |
| [09-macos-webkit.md](09-macos-webkit.md) | WebKit SPI, injected bundle, host, AppKit |
| [10-observability-testing.md](10-observability-testing.md) | diagnostics, metrics, testkit, failure matrix |
| [11-roadmap.md](11-roadmap.md) | 단계, 백로그, 완료 정의, ADR 결정사항 |

## 공통 구현 원칙

- 전체 경계는 먼저 정의하되 기능은 vertical slice로 완성한다.
- Wire contract를 provider와 public facade보다 먼저 고정한다.
- 상대 process가 작성한 shared memory는 전부 untrusted input으로 취급한다.
- Platform code는 protocol 의미론을 재구현하지 않는다.
- Host는 control plane만 담당하고 payload API를 갖지 않는다.
- 미구현 경로는 no-op이나 암묵적 fallback 대신 typed `Unsupported`를 반환한다.
- `unsafe`는 지정된 low-level crate에만 허용하고 각 block의 safety 근거를 기록한다.

## 첫 vertical slice 범위

포함:

- 양방향 SPSC record ring, FIFO, message boundary
- byte-capacity backpressure와 crash-safe publication
- session/generation, HELLO/ACK, close/reset
- IOSurface, Darwin Notify, safety polling
- native peer, renderer core, JSC binding, TypeScript client
- injected bundle, macOS host, AppKit reference application
- structured error, diagnostics, crash/reload 검증

후순위:

- fragmentation/chunk pool, crypto, Mach provider
- async/Tokio/Bun, Wry/Tauri
- borrowed send/receive, Windows/Linux backend

Fragmentation이 없는 동안 inline maximum을 넘는 message는 `MessageTooLarge`로 거부한다.

## 초기 설계 subset 보완

Vertical slice 의존 관계를 닫기 위해 `1차설계.md`의 초기 subset에 `nwipc-capabilities`, `nwipc-state`, `nwipc-protocol`, `nwipc-signal-coalescing`, `nwipc-bootstrap-codec`, `nwipc-renderer-bootstrap`, `nwipc-session-machine`, 최소 diagnostics/metrics와 testkit을 포함한다.

Phase 2 종료 시 ring/protocol/signal/session의 세부 crate가 지나치게 잘게 나뉘었는지 검토할 수 있다. Wire contract, provider API, renderer/WebView, host/adapter 경계는 유지한다.

## 권장 읽기 순서

1. [스캐폴드 계획](00-scaffold.md)
2. [Protocol](02-protocol.md)과 [Data plane](03-data-plane.md)
3. [Memory](04-memory.md), [Signal](05-signal.md), [Bootstrap/Runtime](06-bootstrap-runtime.md)
4. [Peer](07-peer.md), [Renderer/Client](08-renderer-client.md), [macOS/WebKit](09-macos-webkit.md)
5. [검증 계획](10-observability-testing.md)과 [로드맵](11-roadmap.md)

