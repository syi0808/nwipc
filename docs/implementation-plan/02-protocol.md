# Protocol 모듈 구현계획

## 범위

대상은 `nwipc-layout`, `nwipc-record`, `nwipc-protocol`, `nwipc-validation`이다. 이 계층이 shared-memory wire contract를 소유하며 platform, process, signal을 알지 못한다.

## 선행 결정

ADR로 먼저 고정한다.

- Region header size와 field offset
- Record alignment와 maximum inline message
- Cursor width, wrapping distance 제한, cache-line padding
- Endianness와 protocol/layout version
- Record publication/commit 규칙

## `nwipc-layout`

구현:

- Magic, layout version, byte order, total length, generation, owner role
- Region header, ring descriptor, cursor field의 정확한 offset/size
- Fixed-width integer만 사용하는 typed byte view
- Header/data alignment와 capacity 계산

검증:

- Compile-time size/alignment assertion
- Golden byte fixture와 cross-architecture 동일성
- Malformed length/offset, integer overflow property test

## `nwipc-record`

구현:

- Fixed-size prefix, `RecordKind`, flags와 reserved field
- HELLO/ACK, DATA, CLOSE, RESET, PING/PONG, PADDING
- Forward-compatible decode와 unknown value 처리
- 작성 중 bytes와 committed record를 cursor publication으로 분리

검증:

- Kind별 round-trip
- Unknown kind/flag 정책
- Zero/exact/max length와 padding boundary

## `nwipc-protocol`

구현:

- Version range와 major/minor negotiation
- HELLO/ACK handshake와 capability negotiation
- Session/generation/endpoint role 확인
- Close/reset reason과 error mapping
- Fragment metadata type은 두되 첫 slice capability는 비활성화

검증:

- Major mismatch와 required capability 누락 거부
- Handshake 순서 위반/중복 frame 거부
- Stale generation과 endpoint role mismatch 거부

## `nwipc-validation`

구현:

- Cursor, header, record, payload range의 단일 validation entry point
- 모든 offset/length의 checked arithmetic
- Region 밖 접근 전에 validation 중단
- Stable protocol violation code

검증:

- Arbitrary byte fuzzing에서 panic/OOB 없음
- Cursor wrap, alignment, overlap, truncation
- Corrupted fixture가 session failure로 변환됨

## 호환성 규칙

- Layout과 protocol version은 독립 관리한다.
- Major protocol 불일치는 연결을 거부한다.
- Fixture 변경에는 version 판단과 문서 변경이 필요하다.
- Provider 교체나 Cargo feature는 wire bytes를 바꾸지 않는다.

## 완료 기준

- macOS dependency 없이 compile/test된다.
- arm64/x86_64가 같은 fixture를 해석한다.
- 임의 input이 memory safety나 process panic으로 이어지지 않는다.
- Version/generation mismatch가 stable error로 반환된다.

