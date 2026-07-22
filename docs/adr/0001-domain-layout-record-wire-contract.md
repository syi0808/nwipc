# ADR 0001: Domain, region layout, record wire contract

- Status: Accepted
- Date: 2026-07-22
- Scope: Phase 1 protocol foundation

## Context

NWIPC must exchange shared-memory bytes between arm64 and x86_64 processes without relying on
Rust ABI layout, provider types, or host byte order. A crashed or newer peer may leave malformed or
unknown values in the region, so the first wire version needs bounded lengths, explicit
compatibility rules, and a publication boundary that never exposes a partially written record.

## Decision

### Domain values

- `SessionId` is a non-zero canonical 16-byte value. Its `Debug` and `Display` output is redacted.
- `Generation` and `DocumentGeneration` are non-zero `u64` values and advance only with checked,
  non-wrapping arithmetic.
- `MessageId` and `PortId` are non-zero `u32` values. `Sequence` is a wrapping `u32`; a forward
  distance greater than `i32::MAX` is ambiguous and rejected.
- Public errors contain only a stable category, `u16` code, recoverability, endpoint role, and
  static operation label. Domain-specific sources can be retained in `ErrorWithSource`, but its
  formatting is redacted and conversion to `ErrorReport` drops the source.
- Capability supported/requested/required/negotiated roles use distinct types. Unknown optional
  bits survive intersection; an unsupported unknown required bit fails closed.
- The host and WebKit browser process are never in the payload path. Lifecycle transitions use the
  single table implemented by `nwipc-state`; `Failed` and `Closed` are terminal, while
  `Disconnected`, `Failed`, and `Closed` permit generation replacement.

### Region layout version 1

All multibyte integers use explicit little-endian encoding. Layout version and protocol version are
independent. One region has exactly one writer, either renderer or peer. A session uses one region
per direction.

| Offset | Width | Field |
|---:|---:|---|
| 0 | 8 | magic `NWIPC\0\r\n` |
| 8 | 2 | layout version (`1`) |
| 10 | 2 | byte-order marker (`0x4c45`) |
| 12 | 4 | fixed header length (`4096`) |
| 16 | 8 | total mapped length |
| 24 | 8 | generation |
| 32 | 16 | session ID bytes |
| 48 | 1 | owner role (`1` renderer, `2` peer) |
| 49 | 1 | cursor width (`4`) |
| 50 | 2 | record alignment (`8`) |
| 52 | 4 | ring data offset (`4096`) |
| 56 | 4 | ring capacity |
| 60 | 4 | maximum inline payload |
| 64 | 4 | producer cursor in a dedicated 64-byte cache line |
| 128 | 4 | consumer cursor in a dedicated 64-byte cache line |
| 192 | 3904 | reserved, zero on initialization |
| 4096 | capacity | ring bytes |

The cursor is an aligned wrapping `u32`. Ring capacity is 8-byte aligned and at most
`2,147,483,640`, keeping every valid producer-consumer distance within the unambiguous half-range.
The first-slice maximum inline payload is 1 MiB. The encoded per-region maximum may be smaller.

### Record prefix version 1

The fixed prefix is 24 bytes and every complete record is padded to 8-byte alignment.

| Offset | Width | Field |
|---:|---:|---|
| 0 | 4 | complete aligned record length |
| 4 | 4 | exact payload length |
| 8 | 4 | message ID; zero only for `PADDING` |
| 12 | 4 | wrapping sequence |
| 16 | 2 | record kind |
| 18 | 2 | flags |
| 20 | 4 | reserved, must be zero |

Kinds 1 through 9 are `HELLO`, `HELLO_ACK`, `DATA`, `CLOSE`, `RESET`, `PING`, `PONG`, `PADDING`,
and `ERROR`. Unknown kinds are preserved and skipped using the validated record length. Flag bits
0 through 7 are optional: unknown values are preserved. Bits 8 through 15 are required semantics:
an unknown value rejects the record. `PADDING` has no payload, ID, or flags and may have any
aligned length of at least 24 bytes.

### Publication

A writer reserves private ring bytes, zeroes the entire aligned record, writes prefix and payload,
then publishes the complete range with a **release** store of the producer cursor. A reader uses an
**acquire** load and decodes only bytes before the observed cursor. There is no committed flag in a
record and no cursor update for a partial write. Signal delivery is only a hint and does not change
this rule. `nwipc-record` therefore exposes unpublished encoding and committed decoding, but no
commit operation; the future ring/atomic layer owns publication.

## Compatibility

- Changing an offset, width, endianness, alignment, cursor rule, prefix field, or record kind
  meaning requires a new layout version and fixture review.
- Adding an optional flag or defining a previously unknown record kind does not by itself require
  a layout version, provided old readers can safely skip it.
- Reserved fields remain zero in version 1. Provider selection and Cargo features never alter wire
  bytes.
- `tests/protocol-fixtures` is normative. Fixture changes must name the affected version decision.

## Consequences

The codec performs explicit byte copies rather than transmuting native structs. This is slightly
more verbose, but removes architecture padding and endianness from the contract, keeps all core
code safe Rust, and gives validation a deterministic boundary before atomic and ring work begins.
