# Vertical slice completion evidence — `3fecc42`

## 판정

Production vertical slice 구현은 2026-07-23에 완료로 판정한다. 후보 코드는
`3fecc42715a5873e003c0c666b8cca3cdb530c02`이며 portable CI, hardening,
arm64/x86_64 provider gate, 로컬 trusted-identity hardened WebKit E2E가 같은 SHA를 검증했다.

이 기록은 vertical slice milestone의 완료 증거다. GitHub-hosted trusted signing 자동화와
`Release Gate / release-evidence` 성공을 대신하지 않으며, 해당 자동 gate가 통과하기 전에는
release candidate 완료를 선언하지 않는다.

## 원격 검증

| Gate | 결과 | 실행 |
|---|---|---|
| CI | 성공 | [run 29974969010](https://github.com/syi0808/nwipc/actions/runs/29974969010) |
| Hardening | 성공 | [run 29974969024](https://github.com/syi0808/nwipc/actions/runs/29974969024) |
| Release preflight | 성공 | [job 89105538395](https://github.com/syi0808/nwipc/actions/runs/29975251134/job/89105538395) |
| macOS 15 arm64 fixtures/providers/benchmark | 성공 | [job 89105557687](https://github.com/syi0808/nwipc/actions/runs/29975251134/job/89105557687) |
| macOS 15 x86_64 fixtures/providers/benchmark | 성공 | [job 89105557767](https://github.com/syi0808/nwipc/actions/runs/29975251134/job/89105557767) |

Release Gate run 29975251134는 위 provider jobs가 성공한 뒤 self-hosted signing runner 대기를
중단하기 위해 취소했다. 따라서 이 run 전체를 성공한 release gate로 인용하지 않는다.

## 로컬 trusted WebKit E2E

- 실행 시각: 2026-07-23 11:45 KST
- 환경: macOS 26.2 (25C56), arm64
- 명령: `NWIPC_CODESIGN_IDENTITY=… NWIPC_REQUIRE_TRUSTED_SIGNING=1 cargo xtask webkit-e2e`
- 서명: Apple Development trust chain, Team ID `PDRAQZHYD3`
- 검증: App, injected bundle, native peer 모두 `codesign --verify --strict` 성공
- 보안 속성: 세 artifact 모두 hardened runtime이며 제한 entitlement 검사 성공
- 로그: `target/webkit-e2e/`의 scenario log 28개
- 정렬된 log SHA-256 manifest의 SHA-256:
  `88aa11d181c328c29aea26951f98b22d9e8e35068ac35ccd6bceb3bf82d53d5b`

| Artifact | Identifier | CDHash |
|---|---|---|
| AppKit harness | `dev.nwipc.webkit-e2e.run-41645` | `04b3f524af45e61a495d3731d88cd098d0399705` |
| Injected bundle | `dev.nwipc.injected-bundle` | `b11ef231ec9bdc72cd71604bbb5b1f357be514b4` |
| Native peer | `nwipc-webkit-e2e-peer` | `b6a3e54e1caed4cdfb94ba6a4bc74b0969a1d441` |

확인된 matrix는 zero/exact/fragmented/maximum payload, backpressure/writable recovery,
notification drop/duplicate/delay, commit 전후 writer exit, peer kill, WebContent replacement,
stale generation 격리다. Host는 lifecycle/completion만 관찰하고 payload byte를 중계하지 않았다.

## 보류 범위

- GitHub-hosted runner의 임시 keychain과 CI 전용 signing certificate 구성
- `Release Gate / signed-webkit-e2e`와 `release-evidence` 자동 성공
- macOS minor release 확대와 Developer ID notarization/stapling
