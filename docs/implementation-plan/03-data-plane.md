# Data-plane 모듈 구현계획

## 범위

대상은 `nwipc-atomic`, `nwipc-ring-core`, `nwipc-ring-writer`, `nwipc-ring-reader`, `nwipc-flow-control`, `nwipc-channel-core`다. In-process fake provider로 먼저 완성한다.

## `nwipc-atomic`

구현:

- Shared region의 aligned `u32` atomic wrapper
- Acquire load, release store, 필요한 acq-rel operation만 노출
- Pointer provenance, alignment, mapping lifetime safety contract

검증:

- arm64/x86_64 conformance
- Safe constructor의 misalignment 거부
- Unsafe audit와 가능한 Miri subset

## `nwipc-ring-core`

구현:

- Wrapping cursor distance
- Used/free/contiguous capacity
- Wrap padding plan과 exact-fit 계산
- Record가 capacity보다 클 때 deterministic backpressure/error

검증:

- 축소된 cursor domain exhaustive/property test
- Exact fit, one-byte short, wrap boundary
- Producer가 unconsumed bytes를 덮지 않는 invariant

## `nwipc-ring-writer`

구현 순서:

1. Capacity와 padding 계산
2. Header/payload 작성
3. 필요한 tag/reserved field 작성
4. Release-store로 commit cursor publish
5. Empty→non-empty signal 필요 여부 반환

검증:

- Commit 전 crash record는 reader에 보이지 않음
- Commit 후에는 완전한 record만 보임
- Payload copy와 publication의 happens-before

## `nwipc-ring-reader`

구현:

- Acquire-load 뒤 committed range만 읽음
- Validation을 통과한 borrowed view
- View lifetime과 consume/ack의 safe API
- Padding 자동 처리와 drain boundary

검증:

- Consume 전 overwrite 방지
- Malformed record에서 session failure
- Empty/non-empty, wrap, padding

## `nwipc-flow-control`

구현:

- Byte 기준 high/low watermark와 hysteresis
- Backpressured→writable transition
- Producer wake-up 필요 여부와 buffered amount/counter

검증:

- 반복 wake-up suppression
- Watermark boundary
- Fixed capacity invariant

## `nwipc-channel-core`

구현:

- 반대 ownership의 TX/RX ring 조합
- Handshake와 send/receive 연결
- Signal을 hint로 취급하고 drain-until-empty
- Close/reset/generation failure 전파

검증:

- In-process 양방향 FIFO와 message boundary
- Lost/coalesced/duplicate signal에도 cursor 기반 진행
- Backpressure 해소 후 writable edge 1회
- Crash-before/after-commit fault injection

## 후순위

Fragmentation, chunk pool, authentication/encryption, borrowed receive는 첫 slice 이후다. 그 전에는 inline maximum을 초과한 send를 거부한다.

## 완료 기준

- WebView/OS provider 없이 양방향 echo가 동작한다.
- FIFO, bounded capacity, no-overwrite, crash-safe publication invariant를 자동 검증한다.
- Signal 유실이 correctness를 깨지 않는다.

