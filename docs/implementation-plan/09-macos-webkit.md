# macOS WebKit 모듈 구현계획

## 범위

대상은 `nwipc-macos-spi`, `nwipc-macos-bundle-api`, `nwipc-macos-bundle-shim`, `nwipc-macos-bundle`, `nwipc-macos-artifact`, `nwipc-macos-host`, `nwipc-appkit`이다.

## `nwipc-macos-spi`

구현:

- 필요한 symbol/selector의 최소 선언
- Runtime availability와 OS/build compatibility probe
- Injected bundle URL/process pool/bootstrap parameter 연결
- Required/optional SPI manifest

검증:

- 지원 OS matrix symbol/selector probe
- Missing SPI의 structured `Unsupported`
- Raw Objective-C object 노출 최소화

## Bundle API/shim

구현:

- Host에 link되지 않는 internal bundle contract
- `WKBundleInitialize` export
- C callback ABI, autorelease pool, panic boundary
- Rust bundle orchestration 호출

검증:

- Exported symbol/architecture 확인
- FFI callback panic이 boundary를 넘지 않음

## `nwipc-macos-bundle`

구현:

- Bundle/page/document lifecycle
- Main frame과 normal script world 판별
- Bootstrap attach와 renderer runtime 설치
- Signal callback dispatch
- Page/document 종료 시 generation-scoped cleanup

검증:

- Bundle load marker와 manifest
- Subframe에는 binding 미설치
- Reload, navigation, page destroy, WebContent kill
- Invalid bootstrap의 fail-closed 처리

## `nwipc-macos-artifact`

구현:

- Info.plist, architectures, bundle/build/protocol version
- Protocol/compatibility manifest embedding
- Signing metadata와 `xtask` assemble/inspect

## `nwipc-macos-host`

구현:

- WKWebView 생성 전 bundle/process pool/bootstrap configuration
- Bundle path와 SPI compatibility 검사
- Session과 renderer lifecycle mapping
- Payload를 받지 않는 control-plane API

## `nwipc-appkit`

구현:

- Reference WKWebView application
- Native peer spawn/bootstrap
- Capability/diagnostics 표시
- 양방향 binary echo

검증:

- Host payload send/receive API 부재
- Renderer ↔ peer end-to-end
- Reload/process replacement에서 새 generation
- Bundle/SPI/provider failure의 명시적 표시
- Signed/hardened packaged application

## 완료 기준

- Main frame normal world에서만 binding이 설치된다.
- WKWebView renderer와 peer가 host relay 없이 양방향 통신한다.
- Reload/kill 후 old generation을 폐기하고 재연결하거나 terminal error를 반환한다.

