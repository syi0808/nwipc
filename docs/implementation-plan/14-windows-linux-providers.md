# Windows/Linux native provider 계획

## 분리 원칙

Windows/Linux 지원은 Bun native integration의 부속 작업이 아니다. 공통 wire, validation, channel,
peer core는 재사용하지만 OS resource 생성과 process 간 capability 전달, wake-up, cleanup은 별도의
provider vertical slice로 완성한다.

현재 지원 상태는 다음과 같다.

- 공통 protocol과 in-process data plane은 Linux CI에서 검증된다.
- process-test stream peer는 Linux/macOS에서 실험적으로 검증된다.
- public production facade는 IOSurface/Darwin macOS provider만 선택한다.
- Windows/Linux shared-memory/signal production provider는 없다.

즉 공통 crate가 해당 target에서 compile되는 사실을 native provider 지원으로 표시하지 않는다.

## 공통 선행 작업

### P0 — Provider-neutral production assembly

- `Nwipc`의 resource preparation과 endpoint assembly를 macOS concrete type에서 분리한다.
- bootstrap schema의 provider kind와 descriptor validation을 OS provider별로 확장한다.
- provider handle은 wire 정수로 복사하지 않고 OS가 보장하는 capability-transfer 경로로 전달한다.
- polling correctness fallback, generation binding, AEAD와 diagnostics contract는 동일하게 유지한다.

완료 기준은 fake provider로 public session create, peer/renderer attach, replace와 close가 concrete OS
crate 없이 검증되는 것이다.

## Windows 계획

### W1 — Memory와 capability transfer

- file-mapping object 기반 양방향 region provider를 구현한다.
- child process에 필요한 최소 handle만 제한된 access로 duplicate한다.
- renderer/peer 방향별 read/write protection과 generation descriptor를 검증한다.

### W2 — Signal과 lifecycle

- waitable kernel object 기반 coalesced hint를 구현한다.
- Bun 또는 host event loop가 thread를 소유하지 않고 wait registration을 연결할 수 있게 한다.
- peer exit, handle revoke와 repeated close를 bounded cleanup으로 수렴시킨다.

### W3 — Production integration

- Windows preparer와 endpoint transport를 public facade에 연결한다.
- x86_64/arm64 별도 process raw-byte/notification contract를 수행한다.
- backpressure, duplicated/lost hint, stale handle, peer kill과 replacement matrix를 닫는다.

## Linux 계획

### L1 — Memory와 capability transfer

- anonymous file descriptor와 `mmap` 기반 양방향 region provider를 구현한다.
- Unix-domain control plane의 descriptor passing으로 필요한 descriptor만 전달한다.
- seal/protection, size, offset, generation과 access direction을 attach 전에 검증한다.

### L2 — Signal과 lifecycle

- pollable counter/event descriptor 기반 coalesced hint를 구현한다.
- `epoll` 계열 host loop와 runtime-neutral readiness contract를 연결한다.
- peer exit 감지와 descriptor close가 stale generation delivery보다 먼저 수렴하도록 한다.

### L3 — Production integration

- Linux preparer와 endpoint transport를 public facade에 연결한다.
- x86_64/arm64 별도 process raw-byte/notification contract를 수행한다.
- backpressure, descriptor exhaustion, lost/duplicate hint, peer kill과 replacement matrix를 닫는다.

## 플랫폼별 완료 정의

각 OS는 다른 OS의 진행 상태와 독립적으로 아래 게이트를 모두 통과해야 `Provider 통합` 이상으로
표시한다.

1. Native mapping의 실제 read/write protection과 범위 검증
2. Process 간 capability transfer와 stale generation 거부
3. Signal 유실/중복/지연에도 cursor drain으로 progress
4. Bootstrap partial/invalid input과 peer crash의 bounded cleanup
5. Public facade에서 create, attach, traffic, replace와 close
6. 지원 architecture별 CI 및 실제 two-process evidence

Windows 완료를 Linux 지원의 근거로 사용하거나 그 반대도 허용하지 않는다. Bun integration은 해당
OS provider가 이 게이트를 닫은 뒤 별도의 Bun E2E 증거를 추가해야 한다.
