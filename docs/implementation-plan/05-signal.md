# Signal 모듈 구현계획

## 범위

대상은 `nwipc-signal-api`, `nwipc-signal-coalescing`, `nwipc-signal-darwin`, `nwipc-signal-poll`, `nwipc-signal-hybrid`다. Signal은 message count가 아니라 “shared state가 바뀌었을 수 있음”이라는 hint다.

## `nwipc-signal-api`

구현:

- `notify`, `try_wait`, timeout wait
- Sender/listener ownership과 cancellation contract
- Spurious, duplicate, coalesced, lost wake-up 허용 의미론

검증:

- Fake implementation 공통 contract suite
- Duplicate/lost/spurious event

## `nwipc-signal-coalescing`

구현:

- Empty→non-empty send edge
- Backpressured→writable return edge
- Signal epoch/suppression state

검증:

- Signal storm suppression
- Drain과 재-arm race
- 필요한 edge를 영구적으로 잃지 않음

## `nwipc-signal-darwin`

구현:

- Session/generation/direction별 충돌 방지 notification name
- Register/post/cancel lifecycle
- Dispatch callback을 endpoint event adapter로 변환
- Sandbox failure diagnostics

검증:

- 동일 process와 두 process notification
- Cancellation 이후 callback 없음
- Old generation event 무시

## `nwipc-signal-poll`

구현:

- Active/idle/max interval adaptive poller
- Busy loop 방지와 fake clock 지원
- Recovery latency/counter

## `nwipc-signal-hybrid`

구현:

- Primary Darwin event와 correctness polling 조합
- Wake source diagnostics
- Event 뒤 drain-until-empty, poll 뒤 같은 drain path 재사용

검증:

- Primary event를 drop해도 제한 시간 내 progress
- Idle CPU와 recovery latency
- Duplicate event에도 중복 delivery 없음

## 완료 기준

- Signal provider가 ring 없이 독립 검증된다.
- Darwin notification 유실이 message correctness를 깨지 않는다.
- Generation 교체 후 stale callback이 새 endpoint에 전달되지 않는다.

