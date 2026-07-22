# 스캐폴드·스켈레톤 코드 구축 계획

## 1. 목표

장기 모듈 경계를 저장소 구조와 의존성 규칙으로 먼저 고정하고, 첫 vertical slice에 필요한 crate만 compile 가능한 skeleton과 최소 contract까지 만든다. 후순위 crate는 실제 단계가 시작될 때 추가한다.

## 2. 목표 구조

```text
nwipc/
├─ Cargo.toml
├─ Cargo.lock
├─ rust-toolchain.toml
├─ rustfmt.toml
├─ clippy.toml
├─ deny.toml
├─ package.json
├─ pnpm-workspace.yaml
├─ README.md
├─ LICENSE-APACHE
├─ LICENSE-MIT
├─ crates/
│  ├─ facade/nwipc/
│  ├─ domain/{nwipc-types,nwipc-error,nwipc-capabilities,nwipc-state}/
│  ├─ protocol/{nwipc-layout,nwipc-record,nwipc-protocol,nwipc-validation}/
│  ├─ data-plane/{nwipc-atomic,nwipc-ring-core,nwipc-ring-writer,nwipc-ring-reader,nwipc-flow-control,nwipc-channel-core}/
│  ├─ memory/{nwipc-memory-api,nwipc-region,nwipc-memory-iosurface}/
│  ├─ signal/{nwipc-signal-api,nwipc-signal-coalescing,nwipc-signal-darwin,nwipc-signal-poll,nwipc-signal-hybrid}/
│  ├─ bootstrap/{nwipc-bootstrap-schema,nwipc-bootstrap-codec,nwipc-peer-bootstrap,nwipc-renderer-bootstrap}/
│  ├─ runtime/{nwipc-session,nwipc-session-machine,nwipc-runtime}/
│  ├─ peer/{nwipc-peer-core,nwipc-peer}/
│  ├─ renderer/{nwipc-renderer-api,nwipc-renderer-core,nwipc-renderer-jsc}/
│  ├─ platform/macos/{nwipc-macos-spi,nwipc-macos-host,nwipc-macos-bundle-api,nwipc-macos-bundle-shim,nwipc-macos-bundle,nwipc-macos-artifact}/
│  ├─ adapters/nwipc-appkit/
│  ├─ observability/{nwipc-diagnostics,nwipc-metrics}/
│  └─ testing/{nwipc-testkit,nwipc-process-testkit,nwipc-webkit-testkit}/
├─ packages/{nwipc-client-core,nwipc-client,nwipc-client-testkit}/
├─ native/macos/{bundle,shim,plist,entitlements}/
├─ examples/{macos-appkit,native-peer}/
├─ tests/{protocol-fixtures,process,webkit}/
├─ fuzz/
├─ benches/
├─ model/
├─ docs/
└─ xtask/
```

## 3. Cargo workspace

- Resolver 2와 Edition 2024를 사용한다.
- MSRV는 CI로 검증한 버전을 `rust-toolchain.toml`과 `[workspace.package]`에 고정한다.
- 공통 dependency version과 lint는 workspace에서 상속한다.
- 기본 `unsafe_code = "forbid"`를 적용한다.
- macOS dependency는 target-specific dependency와 `cfg(target_os = "macos")`를 함께 사용한다.
- Linux CI에서도 platform-independent subset이 compile되어야 한다.

`unsafe` 허용 대상:

- `nwipc-atomic`
- `nwipc-memory-iosurface`
- `nwipc-renderer-jsc`
- `nwipc-macos-spi`
- `nwipc-macos-bundle-shim`

각 허용 crate는 safety 문서와 `// SAFETY:` 근거를 갖는다.

## 4. Crate skeleton 규칙

각 crate의 최소 구조는 `Cargo.toml`과 `src/lib.rs`다.

- Crate-level 문서에 책임과 비책임을 기록한다.
- Public type과 module boundary만 먼저 선언한다.
- 내부 source error는 유지하고 public 경계에서 `ErrorReport`로 변환한다.
- `todo!()`, panic, 성공하는 no-op 대신 typed `Unsupported`를 사용한다.
- Platform-independent crate는 `std::os::*`, Objective-C, WebKit type을 사용하지 않는다.
- `missing_docs` deny는 public API가 안정되는 단계에서 활성화한다.

## 5. Feature 정책

첫 slice feature 후보:

```text
macos-iosurface
darwin-notify
poll-safety
appkit
diagnostics
```

후순위 feature:

```text
tracing, crypto, mach-memory, mach-signal, wry, tauri
```

- Feature는 additive해야 한다.
- Feature 조합은 layout/protocol을 암묵적으로 변경하지 않는다.
- Runtime capability는 Cargo feature가 아니라 handshake에서 협상한다.

## 6. TypeScript workspace

pnpm, ESM, TypeScript declaration/source map 생성을 기본으로 한다.

- `@nwipc/client-core`: 순수 port state machine
- `@nwipc/client`: application facade
- `@nwipc/client-testkit`: mock native binding

Node state-machine test와 browser compatibility type test를 분리한다.

## 7. macOS artifact skeleton

- Bundle shim과 Rust renderer orchestration을 분리한다.
- Info.plist, architecture, protocol/build manifest, signing metadata는 artifact 모듈이 관리한다.
- `xtask`에 bundle assemble, inspect, manifest generation, example embed 명령을 둔다.
- Signing identity는 환경에서 주입하고 저장소에 고정하지 않는다.

## 8. CI와 architecture check

| Job | 검사 |
|---|---|
| rust-core | fmt, clippy, unit/doc test, Linux/macOS core build |
| rust-macos | IOSurface, Darwin, SPI, bundle build/test |
| ts-client | lint, typecheck, unit test, package build |
| architecture | 금지 dependency, feature graph, unsafe 위치 |
| protocol | golden fixture, size/offset, backward decode |
| process-e2e | native two-process와 crash injection |
| webkit-e2e | bundle, send/receive, reload/replacement |
| security | cargo-deny, advisory, license |

`xtask architecture-check`는 platform dependency 역류, host의 data-plane 구현 의존, 허용 목록 밖 unsafe/FFI, protocol fixture/version 불일치를 탐지한다.

## 9. 구축 순서와 완료 기준

1. Root Cargo/pnpm workspace와 license/toolchain을 만든다.
2. 첫-slice crate/package skeleton을 만든다.
3. 공통 lint와 architecture check를 연결한다.
4. Fake memory/signal provider와 protocol fixture 위치를 만든다.
5. macOS artifact assemble skeleton을 만든다.

완료 조건:

- 모든 첫-slice crate가 skeleton 상태로 compile된다.
- Rust core와 TS package CI가 통과한다.
- 금지 dependency와 허용되지 않은 unsafe를 넣으면 CI가 실패한다.
- 미구현 기능은 명시적인 `Unsupported`를 반환한다.

