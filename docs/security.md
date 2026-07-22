# 보안 모델과 unsafe 감사

## 신뢰 경계

NWIPC는 동일 사용자가 시작한 AppKit host, WebKit renderer, native peer를 하나의 신뢰
도메인으로 본다. Renderer 또는 peer가 손상되었을 때 상대 endpoint의 기밀성이나 무결성을
보장하는 sandbox escape 방어 채널은 아니다. Shared region의 모든 cursor, header, record와
bootstrap bytes는 파싱 시에는 신뢰하지 않는다.

보호 대상은 process memory safety, session/generation 격리, binary payload, bootstrap secret,
native handle이다. Host는 descriptor와 lifecycle만 전달하며 payload data plane에 참여하지 않는다.

## 위협과 통제

| 위협 | 현재 통제 | 잔여 위험 |
|---|---|---|
| 잘못된 cursor/length로 OOB 접근 | checked arithmetic, capacity/alignment 검사, committed range 이후 decode | OS mapping adapter의 FFI 정확성에 의존 |
| 부분 record 또는 producer crash | payload/header 작성 후 release cursor commit, 미commit bytes 비노출 | 손상된 producer가 의도적으로 cursor를 위조하면 generation 교체 필요 |
| 이전 document의 stale delivery | session/generation 검증과 renderer document invalidation | 애플리케이션 수준 재전송 정책은 없음 |
| bootstrap 가로채기/재사용 | inherited one-shot pipe, 16 KiB 상한, secret HELLO/ACK, bounded timeout | secret은 암호학적 channel authentication이 아님 |
| signal 유실/중복/지연 | signal은 hint로만 사용, cursor polling으로 progress 회복 | polling interval만큼 latency 증가 |
| IOSurface ID 노출 | descriptor 크기/generation/mapping 범위 검사 | 같은 trust domain 밖 process에 대한 confidentiality/authentication 없음 |
| payload/secret log 유출 | typed/redacted error operation만 노출, E2E log에 payload 미기록 | 애플리케이션 callback logging은 범위 밖 |
| private WebKit SPI 변경 | OS allowlist와 required selector runtime probe, fail closed | allowlisted release의 patch update도 실제 E2E 재검증 필요 |

서로 신뢰하지 않는 process, 다른 사용자, 공격자가 descriptor 또는 inherited pipe에 접근할 수 있는
배포에서는 현재 transport를 사용하지 않는다. 이 배포 모델을 지원하려면 Phase 8의 authenticated
key agreement와 record integrity/confidentiality를 먼저 구현해야 한다.

## Unsafe 감사 기준선

`cargo xtask unsafe-audit`는 unsafe 허용 crate와 아래 감사 당시 token 수를 고정한다. 수가 바뀌면
검사가 실패하며 변경된 FFI/pointer invariant를 재검토하고 기준선을 명시적으로 갱신해야 한다.
Safe crate에서 unsafe token을 쓰는 것은 `architecture-check`가 차단한다.

| Crate | 감사 token | 경계와 필수 invariant |
|---|---:|---|
| `nwipc-atomic` | 5 | aligned/live atomic mapping, 단일 producer/consumer, acquire/release publication |
| `nwipc-memory-iosurface` | 24 | CoreFoundation ownership, IOSurface lock lifetime, alloc size 전 범위 검사 |
| `nwipc-signal-darwin` | 5 | NUL-terminated names, valid notify token, cancel-after-registration |
| `nwipc-renderer-jsc` | 76 | live JSC context, callback panic containment, protect/unprotect pairing, typed-array copy-before-return |
| `nwipc-macos-spi` | 4 | non-null Objective-C class/selector/method probe before invocation |
| `nwipc-macos-bundle-shim` | 7 | ABI entry panic containment, WebKit object lifetime, checked callback arguments |

감사 범위는 `src/**/*.rs`이며 test-only JSC FFI도 포함한다. 공개 unsafe 함수는 `# Safety` 계약을
제공하고, 각 dereference/FFI 묶음의 전제는 해당 low-level crate에서 유지한다.

## Signing 정책

macOS E2E 산출물은 nested code부터 outer app 순서로 `--options runtime` 서명 후 strict 검증한다.
기본 entitlement는 비어 있으며 JIT, unsigned executable memory, debugger, library-validation 해제를
허용하지 않는다. Ad-hoc 서명은 packaging/hardened flag만 검증하고 배포 identity 신뢰를 뜻하지
않는다. Trusted identity 실행법과 failure semantics는 [`webkit-e2e.md`](webkit-e2e.md)에 있다.

## 도구 범위

- Stable CI: workspace test/clippy, architecture/unsafe baseline, process crash/replacement matrix
- Fuzz: record/bootstrap/layout arbitrary bytes와 committed regression corpus
- Miri: platform-independent atomic/ring/channel crates; macOS FFI와 child-process E2E 제외
- AddressSanitizer: protocol/bootstrap/data-plane/testkit의 Linux test target; JSC/WebKit FFI 제외
- Hardened E2E: allowlisted macOS arm64에서 실제 bundle, IOSurface echo, WebContent replacement

Miri와 sanitizer가 cross-process memory ordering이나 WebKit sandbox 동작을 증명하지는 않는다.
그 경계는 provider contract와 signed process E2E를 함께 통과해야 한다.
