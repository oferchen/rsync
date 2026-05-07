# Multiplex MSG_* tag audit vs upstream rsync 3.4.1

Tracking issue: oc-rsync task #2107.

Last verified: 2026-05-06 against `docs/multiplex-msg-tags-2107`. Files
spot-checked:

- `crates/protocol/src/envelope/message_code.rs`
- `crates/protocol/src/envelope/header.rs`
- `crates/protocol/src/envelope/constants.rs`
- `crates/protocol/src/multiplex/io/send.rs`
- `crates/protocol/src/multiplex/io/recv.rs`
- `crates/protocol/src/multiplex/reader.rs`
- `crates/protocol/src/multiplex/writer.rs`
- `crates/protocol/src/multiplex/frame.rs`
- `crates/transfer/src/reader/multiplex.rs`
- `crates/transfer/src/writer/multiplex.rs`
- `crates/transfer/src/writer/server.rs`
- `crates/transfer/src/writer/msg_info.rs`
- `crates/transfer/src/lib.rs`
- `crates/daemon/src/daemon/multiplex_stream.rs`
- `crates/daemon/src/daemon/sections/session_runtime.rs`

Upstream cross-references (resolved against
`target/interop/upstream-src/rsync-3.4.1/`):

- `rsync.h` lines 180, 198, 250-278 (`MPLEX_BASE`, `MSG_FLUSH`,
 `enum logcode`, `enum msgcode`).
- `io.c` lines 688, 1050, 1486-1706 (header pack/unpack and the read
 dispatch switch in `read_a_msg()`).
- `io.c` lines 1080-1115, 1418-1460 (`successful_send`,
 `maybe_send_keepalive`).
- `sender.c` lines 367-396, `receiver.c` lines 461-981 (sender / receiver
 emitters for `MSG_NO_SEND`, `MSG_REDO`).
- `log.c` line 863 (`MSG_DELETED` from generator-side `log_delete`).
- `main.c` line 1066 (`MSG_STATS` after `NDX_DONE`).
- `cleanup.c` lines 221-250 (`MSG_ERROR_EXIT` on exit synchronisation).

## Scope

This audit answers, for every multiplexed tag defined by upstream rsync
3.4.1 (`MPLEX_BASE = 7`):

1. Which numeric byte value the tag occupies on the wire.
2. Which upstream call sites emit it, with role and protocol-version
 preconditions.
3. How oc-rsync sends and/or receives it today, including whether the
 frame is acted on or silently consumed.
4. The current Rust test coverage by file.
5. Wire-compatibility risks: tags upstream sends that we drop, and tags
 we emit that older protocol peers will not understand.

Out of scope: `enum logcode` values that never escape the local
process (`FCLIENT` is included since it is also reachable via
`MSG_CLIENT` on the receiver/generator pipe), the `MSG_FLUSH=2`
preprocessor macro that aliases `MSG_INFO`, and process-internal
bookkeeping such as `iobuf.in_multiplexed`.

## Wire format

Multiplexed frames use a single 4-byte little-endian header followed by
the payload bytes (`io.c:1050` `SIVAL(hdr, 0, ((MPLEX_BASE + (int)code)<<24) + len)`).

```
 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10  9  8  7  6  5  4  3  2  1  0
+-----------------------------+-----------------------------------------------------------------+
| MPLEX_BASE + code  (1 byte) | payload length in bytes (3 bytes, little-endian, max 16777215)  |
+-----------------------------+-----------------------------------------------------------------+
```

- `MPLEX_BASE = 7` (`rsync.h:180`,
 `crates/protocol/src/envelope/constants.rs:15`). The constant is the
 historical separation point between raw I/O and multiplexed frames; an
 incoming high byte less than 7 is rejected as
 `EnvelopeError::InvalidTag` (`envelope/header.rs:48-50`).
- `MAX_PAYLOAD_LENGTH = 0x00FF_FFFF` (`envelope/constants.rs:5`). Both
 sender helpers fail with `io::ErrorKind::InvalidInput` when a payload
 exceeds the 24-bit ceiling (`multiplex/io/send.rs:18-22`,
 `multiplex/helpers.rs::ensure_payload_length`).
- `HEADER_LEN = 4` (`envelope/constants.rs:2`), encoded via
 `MessageHeader::encode_raw` and decoded via `MessageHeader::from_raw`
 (`envelope/header.rs:46-82`).
- Payload encoding is tag-specific - see the per-tag table below for
 the upstream byte layout; all integer payloads are little-endian
 32-bit on the wire (`io.c:1054` `send_msg_int`).
- Empty `MSG_DATA` frames double as the modern keepalive
 (`io.c:1431-1452`); `read_a_msg` short-circuits on length 0
 (`recv.rs` callers tolerate it - see `transfer/src/reader/multiplex.rs:301`).

## MSG_* table

| Name | Value | Upstream roles + preconditions | oc-rsync sender | oc-rsync receiver | Rust test coverage |
|------|------:|--------------------------------|-----------------|-------------------|--------------------|
| `MSG_DATA` | 0 | Any role; carries raw protocol bytes after `io_start_multiplex_*`. Empty frame doubles as keepalive when `protocol_version >= 31` (`io.c:1431-1452`, `1497-1506`). | `protocol::send_msg(MessageCode::Data, ...)` from `MultiplexWriter::write` and the vectored fast path (`crates/transfer/src/writer/multiplex.rs:54-156`); daemon stream writer wraps every byte (`daemon/multiplex_stream.rs:140-149`). | `MultiplexReader::read` returns the bytes to the caller; zero-length frame is skipped to avoid a spurious EOF (`crates/transfer/src/reader/multiplex.rs:289-303`, `crates/protocol/src/multiplex/reader.rs:204-213`). | `crates/protocol/src/multiplex/tests/*`, `crates/protocol/tests/mplex_io_integration.rs`, `crates/protocol/tests/golden_protocol_v28_mplex_delta_stats.rs`, `crates/transfer/src/reader/tests.rs`, `crates/transfer/src/writer/tests.rs`. |
| `MSG_ERROR_XFER` | 1 | Sender, receiver, or generator; aliased to `FERROR_XFER`. Counts toward partial-transfer exit code (`io.c:1639-1661`). | Not emitted directly today; reached only via the multiplexed log forwarding helpers in `crates/transfer/src/writer/msg_info.rs` and `crates/protocol/src/multiplex/writer.rs::write_message`. | Routed to `stderr` alongside other error-class tags (`crates/transfer/src/reader/multiplex.rs:182-190`, `crates/daemon/src/daemon/multiplex_stream.rs:97-105`). | `crates/protocol/src/envelope/tests/codes.rs`, `crates/protocol/src/envelope/tests/properties.rs`, `crates/protocol/tests/multiplex_validation.rs`. |
| `MSG_INFO` (`MSG_FLUSH` alias) | 2 | Logging from any role (`io.c:1637-1661`). `MSG_FLUSH` is a preprocessor alias used historically to flush the message buffer; same numeric value. | `MultiplexWriter::send_msg_info` for daemon greeting / motd traffic (`crates/transfer/src/writer/msg_info.rs:35-40`); generator status text via `writer.send_message(MessageCode::Info, ...)` (`crates/transfer/src/generator/protocol_io.rs:194`). | Routed to `stdout`, paired with `MSG_CLIENT` (`crates/transfer/src/reader/multiplex.rs:169-175`, `crates/daemon/src/daemon/multiplex_stream.rs:83-90`). | `crates/protocol/tests/keepalive.rs`, `crates/protocol/tests/golden_handshakes.rs`, `crates/protocol/src/envelope/tests/codes.rs` (FLUSH alias coverage), `crates/protocol/src/multiplex/reader.rs` doctests. |
| `MSG_ERROR` | 3 | Logging for protocol >= 30 (`io.c:1637-1661`). | `MessageFrame::new(MessageCode::Error, ...)` during binary-only handshake refusal (`crates/daemon/src/daemon/sections/session_runtime.rs:401-405`). | Routed to `stderr` (same arm as `MSG_ERROR_XFER`). | `crates/protocol/tests/multiplex_validation.rs`, `crates/protocol/src/envelope/tests/codes.rs`. |
| `MSG_WARNING` | 4 | Logging for protocol >= 30 (`io.c:1637-1661`). | Not emitted directly. | Routed to `stderr` (`crates/transfer/src/reader/multiplex.rs:176-181`, `crates/daemon/src/daemon/multiplex_stream.rs:91-96`). | `crates/protocol/tests/multiplex_validation.rs`. |
| `MSG_ERROR_SOCKET` | 5 | Sibling-only over the receiver -> generator pipe (`io.c:1628-1634`). Sets `msgs2stderr = 1`. | Not emitted; oc-rsync does not reproduce the upstream sibling-pipe topology. | Treated as a generic error and surfaced to `stderr` (`crates/transfer/src/reader/multiplex.rs:182-189`). | `crates/protocol/src/envelope/tests/codes.rs`. |
| `MSG_LOG` | 6 | Sibling-only daemon-log channel (`io.c:1628-1634`). | Not emitted. | Forwarded to `stderr` alongside `MSG_WARNING` (`crates/transfer/src/reader/multiplex.rs:176-181`). | `crates/protocol/src/envelope/tests/codes.rs`. |
| `MSG_CLIENT` | 7 | `FCLIENT`; never sent over the socket by the upstream sender (`io.c:1628-1634`); only appears on the sibling pipe. | Not emitted. | Routed to `stdout` together with `MSG_INFO`. | `crates/protocol/src/envelope/tests/codes.rs`, `crates/protocol/tests/multiplex_validation.rs`. |
| `MSG_ERROR_UTF8` | 8 | Sibling-only UTF-8 conversion failure (`io.c:1628-1634`). | Not emitted. | Routed to `stderr` (`crates/transfer/src/reader/multiplex.rs:182-189`). | `crates/protocol/src/envelope/tests/codes.rs`. |
| `MSG_REDO` | 9 | Receiver -> generator on whole-file checksum failure (`receiver.c:973`, `io.c:1514-1519`). 4-byte little-endian flist index payload. Generator-only consumer. | `Writer::send_redo` emits 4 bytes (`crates/transfer/src/writer/server.rs:202-204`). | Indices accumulated in `MultiplexReader::redo_indices`, drained via `take_redo_indices` (`crates/transfer/src/reader/multiplex.rs:89-97, 134-143, 216-219`). | `crates/transfer/src/reader/tests.rs` (`MSG_REDO` accumulation, drain, multi-frame). |
| `MSG_STATS` | 10 | Generator -> client after `NDX_DONE` (`main.c:1066`). Payload is a host-sized `OFF_T` (8 bytes on modern builds) of `stats.total_read`. Generator-only consumer. | Not emitted - oc-rsync does not yet send `MSG_STATS`. | Not handled - the reader hits the catch-all `_ => {}` arm (`crates/transfer/src/reader/multiplex.rs:220-221`, `crates/daemon/src/daemon/multiplex_stream.rs:124-127`); the payload is consumed without updating `Stats::total_read`. | Wire decode round-trip via `crates/protocol/src/envelope/tests/header.rs:202`; no behavioural test for the generator-side stats hand-off. |
| `MSG_IO_ERROR` | 22 | Sender (`io.c:1521-1528`) when source-side I/O fails. Receiver mirrors to generator with `send_msg_int(MSG_IO_ERROR, val)`. 4-byte LE flag word. | Forwarded by `MultiplexReader::dispatch_message` only as an internal accumulator; oc-rsync does not yet relay the flags upstream the way upstream's receiver does. | Accumulator on `MultiplexReader::io_error`, drained via `take_io_error` (`crates/transfer/src/reader/multiplex.rs:75-77, 118-128, 208-211`). | `crates/transfer/src/reader/tests.rs` (single, multiple, OR-merge, malformed payload), `crates/protocol/tests/multiplex_validation.rs`. |
| `MSG_IO_TIMEOUT` | 33 | Daemon -> client to advertise its `io_timeout` (`io.c:1530-1539`). 4-byte LE seconds. Receiver/sender consumer. | `writer.send_message(MessageCode::IoTimeout, ...)` from `crates/transfer/src/lib.rs:552`. | Catch-all arm (`_ => {}`); the value is read off the wire but oc-rsync does not currently call `set_io_timeout` on receipt. | `crates/protocol/tests/timeout_handling.rs`. |
| `MSG_NOOP` | 42 | Legacy keepalive emitted by sender on protocol-30 peers (`io.c:1541-1547`). Empty payload. | `MultiplexWriter::write_keepalive` and `protocol::send_keepalive` both emit `MessageCode::NoOp` regardless of negotiated protocol (`crates/protocol/src/multiplex/writer.rs:327-329`, `crates/protocol/src/multiplex/io/send.rs:114-116`). | Catch-all arm; an inbound `MSG_NOOP` is read and discarded (`crates/transfer/src/reader/multiplex.rs:220-221`). | `crates/protocol/tests/keepalive.rs`, `crates/protocol/tests/network_interruption.rs`, `crates/protocol/src/multiplex/codec.rs:209-216` (header round-trip). |
| `MSG_ERROR_EXIT` | 86 | Synchronised exit (`io.c:1663-1701`, `cleanup.c:247-250`). Protocol >= 31. Empty payload from sender side, 4-byte LE exit-code from generator/receiver. | Daemon emits a 4-byte payload on binary-only handshake failure (`crates/daemon/src/daemon/sections/session_runtime.rs:407`). The transfer-side writer does not yet emit `MSG_ERROR_EXIT` symmetrically. | Captured into `MultiplexReader::error_exit_code`; the next read returns `io::ErrorKind::ConnectionAborted` so the transfer aborts (`crates/transfer/src/reader/multiplex.rs:99-112, 191-207, 250`). Daemon stream raises the same error (`crates/daemon/src/daemon/multiplex_stream.rs:106-122`). | `crates/transfer/src/reader/tests.rs` exit-code propagation, `crates/protocol/src/envelope/tests/codes.rs`. |
| `MSG_SUCCESS` | 100 | Generator <-> sender hand-shake for `successful_send` and dev/inode dedupe (`io.c:1080-1115, 1602-1617`). 4-byte LE flist index normally; 20 bytes (`4+8+8`) when the local-server dev/inode is included. | Not emitted. | Catch-all arm; payload is consumed without acting on it. The current sender does not piggy-back on `successful_send`, so the lost signal only matters when a daemon peer expects acknowledgements for hardlink dedupe over the local-server short-circuit. | Wire encode/decode parity via `crates/protocol/src/envelope/tests/codes.rs:344`. |
| `MSG_DELETED` | 101 | Generator side log of a delete event (`log.c:863`, forwarded via `io.c:1549-1601`). UTF-8 path with optional trailing NUL for directories. iconv-aware on receive. | Not emitted. | Catch-all arm; the payload is read and dropped without updating `DeleteStats` or replaying the log line. | Wire encode/decode parity via `crates/protocol/src/envelope/tests/codes.rs:345`. |
| `MSG_NO_SEND` | 102 | Sender or receiver -> generator when a file open fails (`sender.c:367-396`, `receiver.c:461-881`, `io.c:1618-1626`). Protocol >= 30. 4-byte LE flist index. | `Writer::send_no_send` emits 4 bytes (`crates/transfer/src/writer/server.rs:184-186`). | Indices accumulated in `MultiplexReader::no_send_indices`, drained via `take_no_send_indices` (`crates/transfer/src/reader/multiplex.rs:79-87, 146-160, 212-215`). | `crates/transfer/src/reader/tests.rs` (`MSG_NO_SEND` accumulation, drain, malformed payload), `crates/protocol/tests/multiplex_validation.rs`. |

The Rust enum (`crates/protocol/src/envelope/message_code.rs:18-75`)
covers every numeric value in the table above; tags 11-21, 23-32,
34-41, 43-85, 87-99 and 103+ are unassigned upstream and decoded as
`EnvelopeError::UnknownMessageCode` (`envelope/header.rs:53-55`),
matching upstream's `default: rprintf(FERROR, "unexpected tag %d ...")`
(`io.c:1702-1705`).

## Tags upstream sends that oc-rsync silently drops

These tags are recognised on the wire (the decoder accepts them and
hands them to `MultiplexReader::dispatch_message` /
`MultiplexStreamReader`), but the catch-all `_ => {}` arm consumes the
payload without taking any action:

- `MSG_STATS` (10) - upstream's generator emits this once after
 `NDX_DONE` to update `stats.total_read` on the generator/client side
 (`main.c:1066`, `io.c:1508-1513`). Without handling it,
 oc-rsync's reported `--stats` totals diverge from upstream when we
 act as the receiver-side client of an upstream generator. Also
 affects the `--info=stats2` printout.
- `MSG_IO_TIMEOUT` (33) - upstream's `read_a_msg()` calls
 `set_io_timeout(val)` so a connecting client honours the daemon's
 advertised timeout (`io.c:1530-1539`). oc-rsync reads the value off
 the wire but never propagates it into the local timeout state - the
 transfer continues to use the locally configured `--timeout` even
 when the daemon asked for a stricter bound.
- `MSG_NOOP` (42) - benign; upstream uses it only as a protocol-30
 keepalive shim (`io.c:1541-1547`). Dropping the empty payload is
 correct.
- `MSG_SUCCESS` (100) - upstream sender uses it to flag dev/inode
 hardlink dedupe acknowledgements and successful flist-index sends
 (`io.c:1080-1115, 1602-1617`). oc-rsync does not relay the
 acknowledgement, so a peer that genuinely depends on
 `got_flist_entry_status(FES_SUCCESS, ...)` for ordering will not see
 our progress signal. In practice the upstream local-server short-
 circuit handles the hardlink case before the network path, so this
 has not surfaced as an interop bug, but it is a wire-visible gap.
- `MSG_DELETED` (101) - upstream forwards the deleted path through
 the generator so the operator sees a single `*deleting` itemize line
 even when the receiver and generator are separate processes
 (`log.c:863`, `io.c:1549-1601`). oc-rsync drops the payload, so when
 we are the generator-side client of an upstream delete pass we do
 not echo the upstream-delivered itemize trail.
- `MSG_ERROR_SOCKET` / `MSG_LOG` / `MSG_CLIENT` / `MSG_ERROR_UTF8`
 (5, 6, 7, 8) - sibling-pipe codes. Upstream gates them on
 `am_generator` and routes via `rwrite()` (`io.c:1628-1661`); our
 fallback writes them to `stderr` / `stdout` like the rest of the
 logging family which is acceptable for a single-process layout but
 loses the daemon-log routing distinction `MSG_LOG` would have given.

## Tags oc-rsync emits that older protocol peers will not understand

The capability gates upstream applies to outbound messages
(`io.c:1080-1115, 1622-1626, 1684-1696`) are summarised below alongside
each emitter we have today. Negotiated protocol versions of 28 or 29
will reject any of the tags marked with a check below.

| Tag | Emitter (oc-rsync) | Upstream gate | Risk on protocol 28-29 |
|-----|--------------------|---------------|------------------------|
| `MSG_DATA` (0) | Any time we are multiplexed | Always permitted post-handshake | None. |
| `MSG_INFO` (2) | Daemon greeting/motd, transfer status output | Always permitted | None. |
| `MSG_ERROR` (3) | Daemon binary-handshake refusal (`session_runtime.rs:402`) | Sent only at protocol >= 30 (`io.c:1637-1661` reuses the same routing as `MSG_INFO`, but `am_server` callers gate emission). | Protocol 28/29 peers expect `FERROR_XFER` (1) for inline error logs; an `MSG_ERROR` tag is reachable today only when the local daemon downgrades to a binary-only mode and the negotiated protocol is already >= 30, so this stays inside the upstream contract. |
| `MSG_IO_TIMEOUT` (33) | Daemon timeout advertisement (`transfer/src/lib.rs:552`) | Sent unconditionally by upstream daemons (`io.c:1530-1539`) | None - upstream has accepted this tag since the multiplexed stream existed. |
| `MSG_REDO` (9) | `Writer::send_redo` (`writer/server.rs:202`) | Always available | None. |
| `MSG_NO_SEND` (102) | `Writer::send_no_send` (`writer/server.rs:184`) | Upstream gates on `protocol_version >= 30` (`sender.c:366-368`). | Yes - emitting `MSG_NO_SEND` to a protocol-29 peer triggers the upstream `default: ... unexpected tag` arm (`io.c:1702-1705`) and hard exit `RERR_STREAMIO`. The current `Writer::send_no_send` does not check the negotiated protocol version. |
| `MSG_NOOP` (42) | `protocol::send_keepalive`, `MultiplexWriter::write_keepalive` (`io/send.rs:114`, `multiplex/writer.rs:327`) | Protocol 30 only; protocol 31+ uses an empty `MSG_DATA` instead (`io.c:1425-1452`). | Yes - calling `send_keepalive` on a protocol >= 31 peer still works because upstream still recognises the tag, but it differs from the upstream wire (which emits `MSG_DATA`/0/0). On protocol 29 a `MSG_NOOP` triggers the unexpected-tag exit. |
| `MSG_ERROR_EXIT` (86) | Daemon binary-handshake refusal (`session_runtime.rs:407`) | `protocol_version >= 31` (`io.c:1684-1696`). | Yes for any peer that negotiated < 31. The current daemon path emits `MSG_ERROR_EXIT` ahead of the protocol negotiation result, before a peer version is known. |

The remaining defined tags (`MSG_ERROR_XFER`, `MSG_WARNING`,
`MSG_ERROR_SOCKET`, `MSG_LOG`, `MSG_CLIENT`, `MSG_ERROR_UTF8`,
`MSG_STATS`, `MSG_IO_ERROR`, `MSG_SUCCESS`, `MSG_DELETED`) are not
emitted today, so they cannot regress wire-compat from oc-rsync.

## Action items

1. Plumb `MSG_STATS` into the receiver-side stats so `--stats` and
 `--info=stats2` totals match upstream when oc-rsync is the client.
 The reader already obtains the payload via the dispatch loop; only
 `Stats::total_read` and the related printout wiring are missing.
2. On `MSG_IO_TIMEOUT` adjust the local timeout to `min(local, peer)`,
 matching `set_io_timeout()` (`io.c:1535-1538`). Without this we
 ignore daemon-imposed limits and risk exceeding their watchdog.
3. Gate `MSG_NO_SEND`, `MSG_NOOP` and `MSG_ERROR_EXIT` emission on the
 negotiated protocol version. Ideally:
 - `MSG_NO_SEND` only when `protocol_version >= 30`; otherwise drop
 the file silently as upstream does on protocol 29.
 - Replace `MSG_NOOP` keepalives with empty `MSG_DATA` when
 `protocol_version >= 31` (`io.c:1431-1452`).
 - Suppress `MSG_ERROR_EXIT` when the negotiated version is < 31, or
 wait for the version to be known before sending it; until then
 fall back to `MSG_ERROR` + connection close.
4. Optional: replay `MSG_DELETED` payloads through the local
 itemize/`DeleteStats` accounting so a separate generator process can
 report deletes through the receiver, mirroring upstream daemon
 layouts.
5. Optional: forward `MSG_SUCCESS` indices into a hardlink-dedupe
 callback so oc-rsync can interoperate with peers that depend on the
 explicit acknowledgement instead of the local-server short-circuit.
