# Bootstrap·Runtime 모듈 구현계획

## 범위

대상은 `nwipc-bootstrap-schema`, `nwipc-bootstrap-codec`, `nwipc-peer-bootstrap`, `nwipc-renderer-bootstrap`, `nwipc-session`, `nwipc-session-machine`, `nwipc-runtime`이다.

## Bootstrap schema/codec

구현:

- Schema version, session/generation, protocol range, endpoint role
- Provider-tagged opaque memory/signal descriptor
- Envelope maximum, required/unknown field 규칙
- Peer binary encoding과 renderer plist adapter가 공유하는 domain model

검증:

- Golden fixture와 schema mismatch
- Truncated/oversized/duplicate field 거부
- Descriptor/secret debug redaction

## `nwipc-peer-bootstrap`

구현:

- Inherited anonymous pipe의 length-prefixed one-shot read
- Exact-length, maximum length, timeout, close-on-exec
- Consume 후 descriptor ownership 이전과 secret zeroization

검증:

- Partial write, early EOF, oversize, timeout
- Unrelated child process로 descriptor가 누출되지 않음

## `nwipc-renderer-bootstrap`

구현:

- Property-list compatible envelope decode
- Session/generation/provider 검증 뒤 attach
- Memory/signal attach 전 JS binding open 금지

검증:

- Missing/invalid parameter
- Unsupported provider/schema mismatch의 fail-closed 처리

## `nwipc-session`

구현:

- Identity/state/prepared resource/endpoint status aggregate
- Resource ownership과 idempotent cleanup
- Thread/process 생성은 소유하지 않음

## `nwipc-session-machine`

구현:

- Renderer/peer attach, handshake, document replacement, exit, violation, close event
- State transition 실행
- Old endpoint invalidate → resource close → generation N+1 생성

검증:

- Event sequence table
- Renderer/peer exit, protocol violation, document replacement
- Duplicate close와 partial preparation cleanup

## `nwipc-runtime`

구현:

- Session ID/generation 발급기와 registry
- Provider 조합과 capability report
- Lifecycle routing과 diagnostics snapshot
- Platform backend용 control-plane API

금지:

- Payload slice 처리
- Process spawn 정책
- Wry/Tauri/WebKit SPI 직접 의존

검증:

- Multi-session isolation
- ID/generation 중복 방지
- Runtime/session drop cleanup
- API review로 payload path 부재 확인

## 완료 기준

- Partial bootstrap과 lifecycle failure가 bounded timeout 안에 정리된다.
- Old generation resource/message는 새 generation에서 사용되지 않는다.
- Runtime은 control plane만 소유한다.

