# Signed/Hardened WKWebView E2E 검증 계획과 실행 절차

## 목적

실제 macOS `WKWebView`와 `WebContent` process가 hardened runtime에서 NWIPC injected bundle을
로드하는지 검증한다. Portable unit test가 대신할 수 없는 SPI availability, bundle packaging,
code signing, process replacement 경계를 대상으로 한다.

이 검증은 payload를 host가 중계하지 않는다는 구조를 유지한다. Host harness가 관찰하는 것은
navigation/lifecycle, bundle load, echo completion notification뿐이며 payload byte는 받지 않는다.

## 검증 단계

### Tier 1 — 계약 테스트

- SPI allowlist와 required selector probe
- Main-frame/normal-world 판별
- Renderer bootstrap fail-closed와 partial attach cleanup
- Reload/kill generation replacement와 stale generation 거부
- Bundle manifest, ABI panic boundary, artifact layout

`cargo test --workspace`에서 모든 플랫폼에 대해 실행한다.

### Tier 2 — Ad-hoc hardened smoke

- 실제 AppKit host executable과 `.app` 생성
- 실제 injected bundle assemble과 `WKBundleInitialize` export 검사
- App과 bundle을 ad-hoc identity(`-`) 및 `--options runtime`으로 서명
- `codesign --verify --strict`와 hardened runtime flag 검사
- 실제 `WKWebView` navigation과 `WebContent` injected-bundle load 확인
- Signed native-peer helper와 `WebContent` 사이의 직접 `IOSurface` binary echo 확인
- `WebContent` 강제 종료 후 새 process identity와 navigation 확인

로컬 기본 모드다. Hardened runtime과 nested-code 구조는 검증하지만 Team ID와 배포 인증서
trust chain은 검증하지 않는다.

### Tier 3 — Trusted identity hardened E2E

Tier 2와 같은 process matrix를 Apple Development 또는 Developer ID Application identity로
서명한다. CI secret 또는 개발자 keychain에서 identity를 주입하며 저장소에 고정하지 않는다.

```sh
NWIPC_CODESIGN_IDENTITY="Apple Development: Example (TEAMID)" \
NWIPC_REQUIRE_TRUSTED_SIGNING=1 \
cargo xtask webkit-e2e
```

## 선행 조건

- macOS 26.2 arm64와 repository SPI allowlist가 일치해야 한다.
- Xcode command line tools, macOS SDK, `clang`, `codesign`, `security`가 필요하다.
- Trusted mode는 `security find-identity -v -p codesigning`에 identity가 보여야 한다.
- App sandbox entitlement는 사용하지 않는다. `WebContent` sandbox는 WebKit이 소유한다.

## 실행

```sh
cargo xtask webkit-e2e
```

명령은 다음 순서로 실행한다.

1. `nwipc-macos-bundle-shim`을 빌드한다.
2. `target/NWIPC.bundle`을 assemble하고 manifest/layout을 검사한다.
3. Objective-C AppKit E2E harness를 컴파일하고 `target/NWIPC-E2E.app`을 조립한다.
4. Nested bundle을 먼저, outer app을 나중에 hardened runtime으로 서명한다.
5. Native-peer helper를 hardened runtime으로 서명하고 outer app의 nested code로 봉인한다.
6. 서명 구조, runtime flag, entitlement, `WKBundleInitialize` export를 검사한다.
7. Host가 `IOSurface` descriptor를 renderer와 peer에 전달하고 payload path에서는 빠진다.
8. App executable과 peer helper를 실행해 binary echo, initial bundle marker, process replacement를 기다린다.

생성물과 child stdout/stderr는 `target/webkit-e2e/`에 보존한다. 제한 시간은
`NWIPC_E2E_TIMEOUT_SECONDS`로 조정하며 기본값은 20초다.

## 성공 조건

- App과 injected bundle 모두 `codesign --verify --strict`를 통과한다.
- App과 injected bundle 모두 hardened runtime flag를 가진다.
- Native-peer helper도 hardened runtime으로 서명되고 outer app signature에 봉인된다.
- JIT, unsigned executable memory, library validation 해제, debugger entitlement가 없다.
- App harness가 required SPI class/selector를 모두 확인한다.
- 첫 navigation에서 bundle load marker를 한 번 관찰한다.
- Renderer가 `[0x00, 0x01, 0xff, 0x02, ...]` payload를 쓰고 peer가 동일 bytes를 echo한다.
- Echo 동안 host harness와 `xtask`는 payload bytes를 읽거나 복사하지 않는다.
- `_killWebContentProcessAndResetState` 뒤 process ID가 바뀌고 새 navigation이 완료된다.
- App harness가 제한 시간 안에 exit code 0으로 종료한다.

## Failure matrix

| 실패 | 기대 결과 |
|---|---|
| 지원하지 않는 OS/SPI | `Unsupported`, WebView 생성 전 종료 |
| bundle executable/manifest 누락 | artifact inspection 실패 |
| `WKBundleInitialize` export 누락 | 실행 전 검사 실패 |
| signing identity 없음 | trusted mode에서 실행 전 실패 |
| hardened runtime flag 누락 | signing inspection 실패 |
| initial bundle load timeout | harness exit 3 |
| renderer↔peer echo timeout/mismatch | harness exit 4 또는 peer non-zero exit |
| replacement process/navigation timeout | harness exit 5 |
| child signal/crash | `xtask`가 exit status와 log 경로 보고 |

## Entitlement 정책

기본 entitlement 파일은 의도적으로 비어 있다. JIT, unsigned executable memory, library
validation 비활성화 entitlement를 추가하지 않는다. 필요성이 발견되면 silent relaxation 대신
새 ADR과 negative test를 먼저 추가한다.

## 현재 범위 밖

- Developer ID notarization/stapling
- Intel/x86_64와 macOS minor-release matrix
- Production ring/record handshake와 backpressure를 사용하는 WebKit echo
- Origin별 binding policy

이 제한은 Phase 7 [지원 매트릭스](support-matrix.md)와 [위협 모델](security.md)에 반영되어
있으며, 지원 범위를 넓히기 전에 조합별 signed process 결과를 추가해야 한다.
