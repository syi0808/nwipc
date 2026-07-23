# Authentication and encryption

## 적용 범위

macOS production transport는 bootstrap에서 생성한 32-byte secret으로
`nwipc-crypto::EndpointProtection`을 구성한다. 보호는 renderer와 peer가 shared ring에 넣는 모든
complete transport frame에 적용되며 HELLO/ACK도 평문으로 기록하지 않는다. Handshake에서
`AUTHENTICATED_ENCRYPTION` capability를 필수로 협상하므로 보호되지 않은 production endpoint로
downgrade하지 않는다.

Ring cursor, record length, fragmentation metadata와 notification은 routing/progress를 위해 평문이다.
이 metadata는 기존 validation을 통과해야 하고, 조작으로 재구성된 ciphertext는 AEAD 검증을
통과하지 못한다. 따라서 공격자는 generation을 종료시키는 denial of service는 일으킬 수 있지만
application payload를 읽거나 유효한 payload로 변조할 수 없다.

## Key schedule

- 입력 key material: host가 generation마다 CSPRNG로 생성하는 32-byte bootstrap secret
- salt: 128-bit session ID와 64-bit generation
- KDF: HKDF-SHA-256
- 방향 분리: `renderer-to-peer`, `peer-to-renderer` info label
- 결과: 방향별 256-bit XChaCha20-Poly1305 key와 128-bit nonce prefix

Endpoint는 bootstrap secret의 local copy를 key derivation 뒤 폐기한다. 새 generation은 salt와 새
bootstrap secret이 모두 바뀌므로 이전 ciphertext와 key를 재사용하지 않는다. 이 방식은 bootstrap
PSK에 기반한 authenticated key establishment이며 certificate, 장기 peer identity 또는 forward
secrecy를 제공하지 않는다.

## Protected frame

```text
counter_le: u64 | ciphertext: N bytes | Poly1305 tag: 16 bytes
```

Nonce는 방향별 prefix와 64-bit counter로 구성한다. AAD는 session ID, generation, counter를
포함한다. Sender counter는 underlying channel이 cursor를 publish한 뒤에만 증가하므로
backpressure와 crash-before-commit이 sequence gap을 만들지 않는다. Receiver는 정확히 다음
counter만 허용하고 authentication 성공 뒤 증가한다. Tamper, wrong key/generation은
`AuthenticationFailed`, replay/reorder/gap은 `ReplayDetected`와 `Security` category로 endpoint
replacement를 요구한다. Counter가 소진되면 nonce를 재사용하지 않고 generation을 종료한다.

## 완료 증거

- 방향별 key 분리와 양방향 confidentiality
- payload/tag/counter tamper 및 wrong secret/generation 거부
- replay/reorder 거부와 failed authentication 뒤 counter 불변
- unpublished pending frame의 counter 재사용
- key/secret 비노출 `Debug`와 기존 diagnostics redaction
- macOS actual-provider round trip 및 workspace architecture/clippy/test gate

