# Multiplex framing: all tags audit

Scope: catalog every multiplex tag defined by the protocol crate, identify the
numeric value, the directional intent (sender, receiver, or bidirectional),
the production emission sites, and the production parse / dispatch sites.
Test-only and bench / fuzz usage is intentionally excluded from the per-tag
table; a separate section flags variants whose only references live in tests.

This is a static audit only. It does not consult the upstream C reference; the
reader is expected to cross-check semantics against `target/interop/upstream-src`
when wire compatibility is in question.

## Frame layout

The on-the-wire envelope is a 32-bit little-endian word followed by `length`
bytes of payload. The high byte carries the tag, the low three bytes carry the
payload length. Decoded by `MessageHeader::from_raw` in
`crates/protocol/src/envelope/header.rs:46-57`:

```text
raw = (tag << 24) | (length & 0x00FF_FFFF)
tag = MPLEX_BASE + MessageCode::as_u8()
```

The corresponding emission path is `MessageHeader::encode_raw`
(`crates/protocol/src/envelope/header.rs:79-82`):

```rust
let tag = (MPLEX_BASE as u32) + (self.code as u32);
(tag << 24) | (self.payload_len & PAYLOAD_MASK)
```

Constants:

- `MPLEX_BASE = 7` (`crates/protocol/src/envelope/constants.rs:15`).
- `MAX_PAYLOAD_LENGTH = 0x00FF_FFFF` (`crates/protocol/src/envelope/constants.rs:5`).
- `PAYLOAD_MASK = 0x00FF_FFFF` (`crates/protocol/src/envelope/constants.rs:19`).
- `HEADER_LEN = 4` (`crates/protocol/src/envelope/constants.rs:1`).

Decoder rejects tags below `MPLEX_BASE` (`InvalidTag`) and unknown
`MessageCode` values above the base (`UnknownMessageCode`).

## Tag table

`MessageCode` is defined in `crates/protocol/src/envelope/message_code.rs:19-75`.
Numeric values are the eight-bit message code; the wire tag is `MPLEX_BASE + code`.

The "Direction" column reflects observed usage in this codebase, not the upstream
spec. "S->R" = sender-to-receiver, "R->S" = receiver-to-sender, "G->R" =
generator-to-receiver (sibling pipe), "Bidir" = either side may emit, "Unused"
= no production emit/parse site found.

| Tag | code | wire tag | Direction | Production emission | Production parse / dispatch |
|---|---:|---:|---|---|---|
| `MSG_DATA` | 0 | 7 | Bidir | `crates/protocol/src/multiplex/writer.rs:189`, `crates/protocol/src/multiplex/writer.rs:263`, `crates/transfer/src/writer/multiplex.rs:54`, `crates/transfer/src/writer/multiplex.rs:105`, `crates/transfer/src/writer/multiplex.rs:156` | `crates/transfer/src/reader/multiplex.rs:168`, `crates/daemon/src/daemon/multiplex_stream.rs:78`, `crates/protocol/src/multiplex/reader.rs:205` |
| `MSG_ERROR_XFER` | 1 | 8 | S->R, G->R | (none direct; routed via logging-sink layer) | `crates/transfer/src/reader/multiplex.rs:183`, `crates/daemon/src/daemon/multiplex_stream.rs:98` |
| `MSG_INFO` | 2 | 9 | Bidir | `crates/protocol/src/multiplex/writer.rs:352`, `crates/transfer/src/generator/protocol_io.rs:194`, `crates/transfer/src/writer/msg_info.rs:39` | `crates/transfer/src/reader/multiplex.rs:169`, `crates/daemon/src/daemon/multiplex_stream.rs:84` |
| `MSG_ERROR` | 3 | 10 | Bidir | `crates/protocol/src/multiplex/writer.rs:336`, `crates/daemon/src/daemon/sections/session_runtime.rs:401-404` | `crates/transfer/src/reader/multiplex.rs:182`, `crates/daemon/src/daemon/multiplex_stream.rs:97` |
| `MSG_WARNING` | 4 | 11 | Bidir | `crates/protocol/src/multiplex/writer.rs:344` | `crates/transfer/src/reader/multiplex.rs:176`, `crates/daemon/src/daemon/multiplex_stream.rs:91` |
| `MSG_ERROR_SOCKET` | 5 | 12 | G->R | (none direct) | `crates/transfer/src/reader/multiplex.rs:184`, `crates/daemon/src/daemon/multiplex_stream.rs:99` |
| `MSG_LOG` | 6 | 13 | Daemon log | (none direct) | `crates/transfer/src/reader/multiplex.rs:176`, `crates/daemon/src/daemon/multiplex_stream.rs:91` |
| `MSG_CLIENT` | 7 | 14 | S->client | (none direct) | `crates/transfer/src/reader/multiplex.rs:169`, `crates/daemon/src/daemon/multiplex_stream.rs:84` |
| `MSG_ERROR_UTF8` | 8 | 15 | G->R | (none direct) | `crates/transfer/src/reader/multiplex.rs:185`, `crates/daemon/src/daemon/multiplex_stream.rs:100` |
| `MSG_REDO` | 9 | 16 | R->S | `crates/transfer/src/writer/server.rs:203` | `crates/transfer/src/reader/multiplex.rs:216` |
| `MSG_STATS` | 10 | 17 | S->R | (none direct) | (none direct; falls into `_ => {}` in transfer/daemon dispatchers) |
| `MSG_IO_ERROR` | 22 | 29 | S->R | (none direct) | `crates/transfer/src/reader/multiplex.rs:208` |
| `MSG_IO_TIMEOUT` | 33 | 40 | Daemon -> peer | `crates/transfer/src/lib.rs:552` | (none direct; falls into `_ => {}`) |
| `MSG_NOOP` | 42 | 49 | Bidir | `crates/protocol/src/multiplex/writer.rs:328` (`write_keepalive`), `crates/protocol/src/multiplex/io/send.rs:115` (`send_keepalive`) | (no explicit dispatch; receivers consume and ignore via default arm) |
| `MSG_ERROR_EXIT` | 86 | 93 | Bidir | `crates/daemon/src/daemon/sections/session_runtime.rs:407` | `crates/transfer/src/reader/multiplex.rs:191`, `crates/daemon/src/daemon/multiplex_stream.rs:106` |
| `MSG_SUCCESS` | 100 | 107 | R->G | (none direct) | (none direct; default arm) |
| `MSG_DELETED` | 101 | 108 | R->G | (none direct) | (none direct; default arm) |
| `MSG_NO_SEND` | 102 | 109 | S->R | `crates/transfer/src/writer/server.rs:185` | `crates/transfer/src/reader/multiplex.rs:212` |

`MessageCode::FLUSH` is not a distinct variant. It is a const alias for
`MessageCode::Info` (code 2, wire tag 9), declared at
`crates/protocol/src/envelope/message_code.rs:108` and accepted by `FromStr`
under the legacy mnemonic `"MSG_FLUSH"` at `crates/protocol/src/envelope/message_code.rs:306`.

## Defined-but-not-emitted tags

The following variants exist in `MessageCode` and are accepted by the decoder
but have no production code path that constructs them on the sending side
within this workspace. They appear only in tests, fuzz targets, benches, or
upstream-compatibility tables:

- `MSG_ERROR_XFER` (code 1)
- `MSG_ERROR_SOCKET` (code 5)
- `MSG_LOG` (code 6)
- `MSG_CLIENT` (code 7)
- `MSG_ERROR_UTF8` (code 8)
- `MSG_STATS` (code 10)
- `MSG_IO_ERROR` (code 22)
- `MSG_SUCCESS` (code 100)
- `MSG_DELETED` (code 101)

Most of these are framed by upstream peers (sender, generator, daemon child
processes) and our role here is to parse / classify. `MSG_LOG`, `MSG_CLIENT`,
and `MSG_ERROR_UTF8` flow exclusively from the upstream peer; `MSG_STATS`,
`MSG_SUCCESS`, and `MSG_DELETED` are receiver-to-generator notifications that
the current Rust receiver does not yet originate.

## Parsed-but-not-explicitly-dispatched tags

The dispatch tables in `crates/transfer/src/reader/multiplex.rs:166-223` and
`crates/daemon/src/daemon/multiplex_stream.rs:71-128` use a final `_ => {}`
arm. The following decoded variants therefore reach the dispatcher but are
silently consumed:

- `MSG_STATS` (code 10)
- `MSG_IO_TIMEOUT` (code 33)
- `MSG_NOOP` (code 42)
- `MSG_SUCCESS` (code 100)
- `MSG_DELETED` (code 101)

`MSG_NOOP` falling through is intentional, since `MessageCode::is_keepalive`
(`crates/protocol/src/envelope/message_code.rs:193`) documents the contract
that receivers silently discard keepalives. The other four indicate features
that are partially wired: the codec round-trips them and tests cover the byte
layout, but no production handler reads the payload.

## Tags defined and tested but never wired into a runtime path

`MSG_STATS`, `MSG_SUCCESS`, and `MSG_DELETED` are exercised only by:

- Header round-trip tests in `crates/protocol/src/envelope/tests/`.
- Frame round-trip tests in `crates/protocol/src/multiplex/tests/`.
- Golden byte tests in `crates/protocol/tests/golden_handshakes.rs` and
  `crates/protocol/tests/golden_protocol_v28_mplex_delta_stats.rs`.
- Fuzz target seeds in `crates/protocol/fuzz/fuzz_targets/multiplex_frame.rs`.

`MSG_IO_TIMEOUT` has a single production emission site in
`crates/transfer/src/lib.rs:552` but no production parse site that decodes the
4-byte payload; the timeout-handling tests live entirely in
`crates/protocol/tests/timeout_handling.rs`.

## Tags whose only emission sites are convenience helpers

`MSG_ERROR`, `MSG_WARNING`, and `MSG_INFO` have direct in-crate emission only
through the `MplexWriter::write_error / write_warning / write_info` helpers in
`crates/protocol/src/multiplex/writer.rs:331-353`. Outside the protocol crate,
the only callers are the example program in
`crates/protocol/examples/mplex_usage.rs` and an inline `Info` send in the
generator's protocol-io path. End-user log routing into these tags happens
through the logging-sink layer rather than direct `MessageCode::*` references,
which is why per-tag emission counts above appear small.

## Summary

- 18 distinct codes plus the `FLUSH = Info` alias. Round-trip table at
  `crates/protocol/src/envelope/message_code.rs:148-167`.
- Single envelope formula at
  `crates/protocol/src/envelope/header.rs:46-82`.
- 6 of 18 tags have no production emit path; 5 of 18 reach the dispatcher
  default arm with no explicit handler. These map to features that are wire-
  compatible (decoder accepts them) but not yet behaviourally complete.
