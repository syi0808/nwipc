# Domain 모듈 구현계획

## 범위와 의존성

대상은 `nwipc-types`, `nwipc-error`, `nwipc-capabilities`, `nwipc-state`다. 모든 모듈은 platform-independent, allocation-minimal, safe Rust여야 한다. Protocol과 runtime은 이 모듈들에 의존하지만 반대 의존은 금지한다.

## `nwipc-types`

구현:

- `SessionId`, `Generation`, `DocumentGeneration`, `MessageId`, `Sequence`, `PortId`
- Checked constructor와 conversion boundary
- Wrapping 가능한 sequence와 단조 증가 generation의 의미 분리
- Display/Debug redaction 규칙

검증:

- Size/alignment assertion
- Overflow와 invalid conversion boundary test
- Platform dependency 금지 검사

## `nwipc-error`

구현:

- Error category, stable code, recoverability, endpoint/context
- Domain-specific source error 보존과 public `ErrorReport` 변환
- Local rich error와 제한된 wire error 분리
- Secret/native descriptor가 출력되지 않는 redaction

검증:

- Public 실패 경로의 typed error
- Stable error code 중복 검사
- Debug/Display redaction test

## `nwipc-capabilities`

구현:

- Supported/requested/required/negotiated capability 구분
- `TransportCapabilities`와 `TransportTopology`
- Unknown capability bit의 forward-compatible 처리
- Host/browser process의 payload/signal path 표현

검증:

- Capability 교집합과 required 누락 property test
- `host_in_payload_path = false` invariant
- Unknown optional/required bit 처리

## `nwipc-state`

구현:

- `Created`부터 `Closed`까지 `SessionState`
- 허용 transition 표와 terminal state
- Replacement/reconnect 가능 상태와 transition violation

검증:

- 전체 transition matrix table test
- Terminal state의 추가 event 거부
- Invalid transition의 stable error mapping

## 완료 기준

- 네 crate가 OS/FFI dependency 없이 compile/test된다.
- Protocol/runtime이 필요한 value와 transition contract를 다른 타입으로 우회하지 않는다.
- Error와 diagnostics에 payload/secret이 노출되지 않는다.

