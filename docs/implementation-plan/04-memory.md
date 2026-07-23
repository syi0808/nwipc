# Memory 모듈 구현계획

## 범위

대상은 `nwipc-memory-api`, `nwipc-region`, `nwipc-memory-iosurface`, `nwipc-memory-mach`다. API는 provider-neutral하고 platform provider만 macOS FFI를 소유한다.

## `nwipc-memory-api`

구현:

- Owned region, mapping, renderer/peer descriptor associated type
- Create/attach/map/drop ownership contract
- Mapping length와 intended access 표현
- Raw native descriptor를 API 밖으로 노출하지 않는 wrapper

검증:

- Fake provider 공통 contract suite
- Length/access mismatch 거부
- Descriptor transfer/clone/drop semantics

## `nwipc-region`

구현:

- `Renderer`/`Peer` owner
- `ReadOnly`/`ReadWrite` access
- 방향별 region pair와 logical layout
- Descriptor와 mapping lifecycle을 분리한 safe model

## `nwipc-memory-iosurface`

구현:

- 방향별 IOSurface 두 개 생성
- Byte length/alignment/base address 검증
- Renderer와 peer에 전달할 descriptor 생성
- Attach/map/lock/unlock/drop lifecycle
- Sandbox/entitlement/capability diagnostics

선행 실험:

- IOSurface ID와 Mach representation 중 실제 WebContent/peer 환경에 적합한 방식 비교
- Read-only/read-write mapping 가능성
- Process boundary와 hardened runtime에서 descriptor 전달 검증

Phase 4 결정:

- 최초 구현은 IOSurface global ID를 bootstrap descriptor로 사용한다.
- 읽기 전용 권한은 safe mapping API에서 강제하며 native mapping 자체의 권한 분리는 후속 hardened-runtime 실험 대상으로 남긴다.
- Descriptor와 mapping diagnostics에서는 native ID와 base address를 노출하지 않는다.

검증:

- 동일 process create/attach/raw byte visibility
- 두 native process attach/read/write
- arm64/x86_64
- Invalid/stale descriptor와 wrong size 거부
- Protocol/ring 없이 provider만 독립 검증

## 안전성 규칙

- Descriptor length와 mapping 범위를 pointer 생성 전에 검증한다.
- Raw pointer lifetime은 owned mapping보다 길 수 없다.
- Native handle/ID는 Debug/diagnostics에서 redaction한다.
- Generation replacement 때 기존 mapping을 재사용하지 않는다.

## `nwipc-memory-mach`

- `mach_vm_allocate`와 memory entry를 분리해 owner mapping과 capability lifetime을 관리한다.
- Task-local port 번호는 직렬화하지 않고 generation-bound bootstrap rendezvous에서 send right를
  Mach message descriptor로 전달한다.
- Attach 시 `mach_vm_map`의 native protection으로 read-only/read-write를 구분한다.
- Safe byte API는 atomic byte 접근을 사용하고 cursor는 acquire/release atomic으로 접근한다.
- 동일 process contract와 별도 process의 entry attach/read/write visibility를 검증한다.

## 완료 기준

- Provider-neutral contract suite를 fake와 IOSurface가 함께 통과한다.
- 두 native process가 IOSurface raw bytes를 교환한다.
- Attach failure가 typed error/capability로 표현되고 silent fallback하지 않는다.
