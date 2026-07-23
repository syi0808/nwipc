# Data-plane 모듈 구현계획

> 구현 상태: Phase 2의 in-process provider와 Phase 8의 bounded fragmentation, atomic batch
> publication, single-message reassembly를 완료했다. OS-backed mapping과 실제 signal provider
> 연결은 후속 phase에서 같은 cursor/channel contract를 사용한다.

## 범위

대상은 `nwipc-atomic`, `nwipc-ring-core`, `nwipc-ring-writer`, `nwipc-ring-reader`,
`nwipc-fragment`, `nwipc-flow-control`, `nwipc-channel-core`다. In-process fake provider로 먼저 완성한다.

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

## `nwipc-fragment`

구현:

- `FRAGMENTED`/`END_OF_MESSAGE` 기반 START/CONTINUE/END
- 동일 message ID와 한 방향당 단일 incomplete message
- 설정 가능한 inline/logical maximum과 checked 누적 크기
- Close/Reset/generation replacement 시 partial message 폐기
- 전체 fragment batch 검증 후 producer cursor 단일 publication

검증:

- Inline 경계, exact multiple, 마지막 short fragment
- Interleaving, 잘못된 flag/ID, 누적 크기 초과 거부
- Capacity 부족과 writer crash에서 partial publication 없음
- Arbitrary state transition fuzzing

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

Authentication/encryption은 Phase 8에서 complete transport frame 보호로 production macOS 경로에
연결했다. Ring metadata는 validation 대상으로 남고 application payload와 HELLO/ACK는
generation-bound AEAD로 보호한다. Chunk pool과 borrowed receive는 후속 Phase 8 범위다.

## 완료 기준

- WebView/OS provider 없이 양방향 echo가 동작한다.
- FIFO, bounded capacity, no-overwrite, crash-safe publication invariant를 자동 검증한다.
- Signal 유실이 correctness를 깨지 않는다.
