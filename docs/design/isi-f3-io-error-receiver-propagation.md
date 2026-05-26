# ISI.f.3 - io_error receiver propagation verification spec

Tracking: ISI.f.3 (#2975). Parent series: ISI (#2737). Siblings:
ISI.f.1 (#2973, failure-mode catalog), ISI.f.2 (#2974, failure-injection
tests). Dependency: ISI.f.1 defines the sender-side io_error accumulation
mechanism; this spec verifies the receiver end of that round-trip.

## 1. Scope

ISI.f.3 verifies that the io_error bitfield accumulated by the sender
during file-list enumeration propagates correctly through the wire
protocol to the receiver, and that the receiver derives the correct exit
code and transfer statistics from the accumulated flags.

The verification covers three distinct wire-level propagation channels:

1. **Flist end-marker io_error** - embedded in the end-of-list marker
   for the initial flist segment (both varint and non-varint encodings).
2. **SAFE_FILE_LIST error sentinel** - two-byte sentinel
   (`XMIT_EXTENDED_FLAGS | XMIT_IO_ERROR_ENDLIST << 8`) followed by a
   varint error code, used for protocol 30+ with safe file list
   negotiated.
3. **MSG_IO_ERROR multiplex frames** - 4-byte LE payload OR'd into the
   receiver's multiplex reader accumulator, forwarded to the generator
   per upstream `io.c:1521-1528`.

Out of scope: receiver-side I/O errors (EACCES on destination open,
disk full) that produce `IOERR_GENERAL` locally. Those are covered by
the general receiver error handling suite. ISI.f.3 is strictly about
sender-originated io_error reaching the receiver intact.

## 2. Wire format reference

### 2.1 Protocol < 30 (fixed encoding)

After the file list entries and the zero end-of-list byte, the sender
writes `write_int(f, ignore_errors ? 0 : io_error)` as a 4-byte
little-endian integer, followed by the UID/GID id lists.

Sender path: `generator/protocol_io.rs::send_io_error_flag()` -
writes `self.io_error` as 4-byte LE when
`self.protocol.uses_fixed_encoding()`.

Receiver path: `receiver/file_list/receive.rs` lines 62-72 - reads
4 bytes LE, OR's non-zero value into `self.flist_io_error` (unless
`ignore_errors`).

upstream: `flist.c:2517-2518` (sender), `flist.c:2738-2742` (receiver).

### 2.2 Protocol 30+ with SAFE_FILE_LIST (non-varint)

The sender embeds the error in the end-of-list marker itself. Instead of
a single zero byte, it writes a two-byte sentinel followed by a varint
error code:

```
[XMIT_EXTENDED_FLAGS (0x04)] [XMIT_IO_ERROR_ENDLIST (0x10)] [varint: error_code]
```

The sentinel reuses bit positions: `XMIT_EXTENDED_FLAGS = 0x04` (primary
byte), `XMIT_IO_ERROR_ENDLIST = 0x10` (extended byte, same bit as
`XMIT_HLINK_FIRST`). Context distinguishes them - the sentinel only
appears where an end-of-list marker is expected.

Sender path: `protocol/flist/write/encoding.rs::write_end()` - when
`io_error` is `Some` and `use_safe_file_list()` is true.

Receiver path: `protocol/flist/read/flags.rs::check_error_marker()` -
detects the sentinel, reads the varint error code, returns
`FlagsResult::IoError(code)`.

upstream: `flist.c:recv_file_entry()` safe_flist error check.

### 2.3 Protocol 30+ with VARINT_FLIST_FLAGS

The end-of-list marker is two varints: `flags=0` followed by `error=N`.
When `error != 0`, the reader returns `FlagsResult::IoError(N)`.

Sender path: `protocol/flist/write/encoding.rs::write_end()` - when
`use_varint_flags()` is true, writes `write_varint(0)` then
`write_varint(io_error.unwrap_or(0))`.

Receiver path: `protocol/flist/read/flags.rs::read_flags()` lines 75-91
- reads second varint after zero flags; returns `IoError(N)` when
non-zero.

upstream: `flist.c:recv_file_list()` varint mode end handling.

### 2.4 MSG_IO_ERROR multiplex frames

The sender (or any process) can send `MSG_IO_ERROR` (code 22) as a
multiplexed message with a 4-byte LE payload. The receiver's multiplex
reader accumulates these via `io_error |= val`.

Sender path: upstream `io.c` sends `send_msg_int(MSG_IO_ERROR, val)`
from the sender process. oc-rsync mirrors this in the generator's error
reporting path.

Receiver path: `transfer/reader/multiplex.rs::handle_io_error_msg()` -
OR's 4-byte LE payload into `self.io_error`. Callers retrieve via
`take_io_error()` which resets the accumulator after reading.

upstream: `io.c:1521-1528` - `io_error |= val; if (am_receiver)
send_msg_int(MSG_IO_ERROR, val);`.

### 2.5 INC_RECURSE sub-list segments

Under INC_RECURSE, the sender emits multiple flist segments. Each
sub-list ends with its own end-of-list marker:

- **Initial segment**: carries `io_error` in the end marker per
  sections 2.2-2.3 (via `send_file_list()`).
- **Sub-list segments**: currently write `None` as io_error in the
  end marker (via `encode_and_send_segment()` at
  `generator/protocol_io.rs` line 488).

This means sub-list I/O errors encountered after the initial segment
are NOT propagated via the flist end marker. They rely on
`MSG_IO_ERROR` frames instead, or on the global `io_error` field in
`GeneratorContext` which is only written to the initial segment's end
marker.

This is a potential gap: if the sender encounters I/O errors while
walking a subdirectory that maps to a sub-list segment, those errors
are accumulated in `GeneratorContext.io_error` but only written to the
initial segment's marker (which has already been sent). The sub-list
end marker passes `None`. ISI.f.3 must verify whether this gap exists
and whether upstream rsync handles it the same way.

upstream: `flist.c:send_extra_file_list()` - sub-list emission. The
upstream code re-checks `io_error` after each sub-list and sends
`MSG_IO_ERROR` if non-zero, rather than embedding it in the sub-list
end marker.

## 3. Receiver accumulation logic

### 3.1 FileListReader.io_error (protocol crate)

`protocol/flist/read/mod.rs` line 123: `io_error: i32` field. Accumulated
via `self.io_error |= code` at line 501 when `read_entry_with_flist()`
encounters `FlagsResult::IoError(code)`. Exposed via `pub const fn
io_error(&self) -> i32`.

The `FileListReader` is cached in the receiver as `flist_reader_cache`.
The receiver reads it during transfer stats assembly.

### 3.2 ReceiverContext.flist_io_error (transfer crate)

`receiver/mod.rs` line 226: `flist_io_error: i32` field. Set at
`receiver/file_list/receive.rs` line 70 for protocol < 30 (the 4-byte LE
read-after-id-lists path). This field captures the pre-protocol-30
io_error channel.

### 3.3 MultiplexReader.io_error (transfer crate)

`reader/multiplex.rs` line 34: `pub(super) io_error: i32` field.
Accumulated via `self.io_error |= val` at line 126 when
`handle_io_error_msg()` processes a `MSG_IO_ERROR` frame. Exposed via
`take_io_error()` which atomically reads and resets.

### 3.4 Receiver TransferStats.io_error (transfer crate)

`receiver/stats.rs` line 45: `pub io_error: i32` field. Assembled from
multiple sources at the end of the transfer:

```
io_error: self.flist_reader_cache.as_ref().map_or(0, |r| r.io_error())
          | self.flist_io_error,
```

This merges:
- `FileListReader.io_error()` - errors from flist end markers (protocol
  30+ SAFE_FILE_LIST / varint paths)
- `flist_io_error` - errors from the protocol < 30 explicit read

This assembly appears in three transfer paths:
- `receiver/transfer/sync.rs` line 416-417 (synchronous transfer)
- `receiver/transfer/pipelined.rs` line 73-74 (pipelined transfer)
- `receiver/transfer/pipelined_incremental.rs` line 43-44 (INC_RECURSE
  pipelined transfer)

### 3.5 Missing channel: MSG_IO_ERROR not merged into TransferStats

The `MultiplexReader.io_error` (from MSG_IO_ERROR frames) is exposed via
`ServerReader::take_io_error()` but is NOT currently merged into the
receiver's `TransferStats.io_error`. The `take_io_error()` method resets
the accumulator, so the value must be taken exactly once and forwarded.

ISI.f.3 must verify:
1. Whether upstream rsync expects the receiver to forward MSG_IO_ERROR
   to the generator (yes, per `io.c:1528`: `if (am_receiver)
   send_msg_int(MSG_IO_ERROR, val)`).
2. Whether oc-rsync currently performs this forwarding (search for
   `take_io_error()` callsites outside of tests).
3. Whether the missing merge is a bug or an intentional design choice
   (MSG_IO_ERROR frames are expected to arrive during the transfer
   phase, not the flist phase, so they may be handled differently).

## 4. Exit code derivation

The `io_error_flags::to_exit_code()` function maps the accumulated
bitfield to rsync exit codes:

| Bit | Constant | Value | Exit code | Upstream constant |
|-----|----------|-------|-----------|-------------------|
| 0 | `IOERR_GENERAL` | `1 << 0` | 23 | `RERR_PARTIAL` |
| 1 | `IOERR_VANISHED` | `1 << 1` | 24 | `RERR_VANISHED` |
| 2 | `IOERR_DEL_LIMIT` | `1 << 2` | 25 | `RERR_DEL_LIMIT` |

Priority: `DEL_LIMIT` > `GENERAL` > `VANISHED`. When multiple bits are
set, the highest-priority exit code wins. This matches upstream
`log.c:log_exit()`.

upstream: `rsync.h:168-170` defines the constants; `log.c` maps them.

## 5. Test cases

### TC-1: Single-segment error via SAFE_FILE_LIST (protocol 30+, non-varint)

Setup: Protocol 30 with `SAFE_FILE_LIST` compat flag (no
`VARINT_FLIST_FLAGS`). Writer produces an end marker with
`io_error = IOERR_GENERAL`. Reader consumes the stream.

Assertions:
- `FileListReader.io_error()` == `IOERR_GENERAL` (1)
- `to_exit_code(reader.io_error())` == 23

Wire bytes: `[0x04, 0x10, varint(1)]` - two-byte sentinel + error code.

### TC-2: Single-segment error via varint end marker

Setup: Protocol 32 with `SAFE_FILE_LIST | VARINT_FLIST_FLAGS`. Writer
produces an end marker with `io_error = IOERR_VANISHED`.

Assertions:
- `FileListReader.io_error()` == `IOERR_VANISHED` (2)
- `to_exit_code(reader.io_error())` == 24

Wire bytes: `[varint(0), varint(2)]`.

### TC-3: Single-segment error via protocol < 30 explicit write

Setup: Protocol 29 (fixed encoding). Sender writes 4-byte LE
`IOERR_GENERAL` after end-of-list and id-lists.

Assertions:
- `ReceiverContext.flist_io_error` == `IOERR_GENERAL` (1)
- Assembled `TransferStats.io_error` includes the bit

Wire bytes: `[0x01, 0x00, 0x00, 0x00]` after id-list.

### TC-4: Multi-segment error accumulation (INC_RECURSE)

Setup: Protocol 32 with INC_RECURSE. Three flist segments:
- Segment 0 (initial): end marker carries `io_error = IOERR_GENERAL`
- Segment 1 (sub-list): end marker carries `io_error = None` (current
  behavior) or `io_error = IOERR_VANISHED` (if we fix the gap)
- Segment 2 (sub-list): end marker carries `io_error = None`

Assertions:
- After segment 0: `FileListReader.io_error()` == 1
- After all segments: io_error includes at least `IOERR_GENERAL`
- `to_exit_code()` == 23

This test documents the current behavior where sub-list segments do not
carry io_error in their end markers. If the implementation is updated
to propagate sub-list errors, the test must be updated to verify
accumulation across segments via bitwise OR.

### TC-5: Mixed clean and error segments

Setup: Three INC_RECURSE segments where only the initial segment has
io_error. Sub-lists complete without error.

Assertions:
- `io_error` == initial segment's error (no spurious bits from clean
  segments)
- Files from clean sub-list segments transfer correctly
- Exit code reflects only the initial segment's error

### TC-6: MSG_IO_ERROR frame round-trip

Setup: Construct a multiplex stream containing two `MSG_IO_ERROR` frames
with payloads `IOERR_GENERAL` and `IOERR_VANISHED` interleaved with
`MSG_DATA` frames.

Assertions:
- After reading all frames: `MultiplexReader.take_io_error()` ==
  `IOERR_GENERAL | IOERR_VANISHED` (3)
- Second `take_io_error()` call returns 0 (reset)
- `to_exit_code(3)` == 23 (GENERAL takes priority over VANISHED)

### TC-7: Combined flist and multiplex io_error

Setup: Protocol 32 flist with `io_error = IOERR_VANISHED` in end marker,
plus a `MSG_IO_ERROR` frame carrying `IOERR_GENERAL` during the transfer
phase.

Assertions:
- `FileListReader.io_error()` == 2 (from flist)
- `MultiplexReader.take_io_error()` == 1 (from MSG frame)
- Combined: 2 | 1 == 3
- `to_exit_code(3)` == 23

This test verifies that both propagation channels contribute to the
final exit code. It also surfaces the gap noted in section 3.5 - if
the MSG_IO_ERROR value is not merged into `TransferStats.io_error`, the
test will fail.

### TC-8: Zero io_error passes through cleanly

Setup: End marker with `io_error = 0` (or `None`).

Assertions:
- `FileListReader.io_error()` == 0
- `TransferStats.io_error` == 0
- Exit code == 0

### TC-9: ignore_errors suppresses io_error (protocol < 30)

Setup: Protocol 29 with `ignore_errors = true`. Sender writes
`IOERR_GENERAL` as the 4-byte io_error.

Assertions:
- `ReceiverContext.flist_io_error` == 0 (suppressed)
- Exit code == 0

upstream: `flist.c:2517` - `write_int(f, ignore_errors ? 0 : io_error)`.

### TC-10: Bitwise OR accumulation across multiple read_entry calls

Setup: Craft a flist stream where two entries trigger `IoError` markers
with different flags (e.g., first `IOERR_GENERAL`, second
`IOERR_VANISHED`).

Assertions:
- After reading both: `FileListReader.io_error()` == 3 (1 | 2)
- Accumulation is idempotent: re-reading the same flag does not change
  the result

### TC-11: DEL_LIMIT priority over GENERAL and VANISHED

Setup: Accumulated io_error = `IOERR_GENERAL | IOERR_VANISHED |
IOERR_DEL_LIMIT` (7).

Assertions:
- `to_exit_code(7)` == 25 (`RERR_DEL_LIMIT` wins)

## 6. Interop verification approach

### 6.1 oc-rsync sender to upstream receiver

Trigger: run oc-rsync `--server --sender` with a source tree containing
a permission-denied subdirectory (FM1 from ISI.f.1). Pipe output to
upstream rsync 3.4.1 receiver.

Verify:
- Upstream receiver exits with code 23 or 24 (not 0)
- Upstream receiver stderr shows the io_error-derived diagnostic
- Readable files arrive byte-identical at the destination

This confirms oc-rsync's sender emits io_error in a format the upstream
receiver understands.

### 6.2 Upstream sender to oc-rsync receiver

Trigger: run upstream rsync 3.4.1 `--server --sender` with the same
fault tree. Pipe output to oc-rsync receiver.

Verify:
- oc-rsync receiver's `TransferStats.io_error` is non-zero
- oc-rsync receiver exits with the same code as the upstream receiver
  would (23 or 24)
- Readable files arrive byte-identical

This confirms oc-rsync's receiver correctly parses upstream's io_error
wire format.

### 6.3 Version matrix

Both directions must be tested against each supported upstream version:

| Upstream version | Protocol | io_error channel |
|-----------------|----------|-----------------|
| 3.0.9 | 30 | SAFE_FILE_LIST end marker |
| 3.1.3 | 31 | SAFE_FILE_LIST end marker |
| 3.4.1 | 32 | varint flist flags |
| 3.4.2 | 32 | varint flist flags |

Protocol 29 (rsync 2.x) is not tested because no supported upstream
version uses it. The protocol < 30 path is covered by unit tests (TC-3,
TC-9).

### 6.4 Byte-level wire validation

For at least one interop pair, capture the wire stream via strace or
a tee harness and verify:

- The end marker bytes match the expected encoding from section 2
- The io_error value is present (non-zero) when the sender encountered
  errors
- No MSG_IO_ERROR frame appears for errors already embedded in the end
  marker (avoiding double-counting)

## 7. Implementation plan

### Phase 1: Unit tests (TC-1 through TC-11)

Add tests to `crates/protocol/src/flist/read/tests.rs` and
`crates/transfer/src/reader/tests.rs` covering the wire-level
round-trips. Most of these are extensions of existing test patterns
(the read/write round-trip tests at lines 142-189 of the read tests
already cover TC-1, TC-2, TC-8 partially).

New tests needed:
- TC-4, TC-5: multi-segment accumulation (requires
  `FileListReader::reset_for_new_segment()` usage)
- TC-6: MSG_IO_ERROR multiplex round-trip (extends existing
  `multiplex_reader_accumulates_msg_io_error` test)
- TC-7: combined channel test (new)
- TC-10: multi-marker accumulation (new)
- TC-11: priority test (extends existing `to_exit_code` tests)

### Phase 2: Investigate MSG_IO_ERROR merge gap

Audit whether `MultiplexReader.take_io_error()` is called during the
transfer loop and whether its value is merged into
`TransferStats.io_error`. If not, determine whether upstream rsync
expects the receiver to:

1. Forward `MSG_IO_ERROR` to the generator via
   `send_msg_int(MSG_IO_ERROR, val)` (yes, per `io.c:1528`).
2. Accumulate it locally for its own exit code (yes, the receiver
   process's own `io_error` global includes the OR'd value).

If a merge gap exists, file a follow-up to wire
`ServerReader::take_io_error()` into the receiver's stats assembly.

### Phase 3: Sub-list io_error propagation audit

Audit `encode_and_send_segment()` to determine whether sub-list
segments should carry `io_error` in their end markers. Compare against
upstream `flist.c:send_extra_file_list()`:

- If upstream embeds io_error per sub-list: update
  `encode_and_send_segment()` to pass `Some(self.io_error)` and add
  a reset-after-send mechanism.
- If upstream relies on `MSG_IO_ERROR` for sub-list errors: document
  this as intentional and ensure the MSG_IO_ERROR path is wired.

### Phase 4: Interop tests (section 6)

Add interop test cases to the ISI.f test suite, gated behind the
`sender-inc-recurse` feature flag. Reuse the existing pipe-driver
infrastructure from `tests/inc_recurse_sender_flist_io_error_isi_f.rs`.

## 8. Existing test coverage

### Protocol crate (flist read/write)

- `read_entry_detects_io_error_in_varint_mode` - varint io_error round-trip
- `read_entry_with_protocol_31_accepts_error_marker` - SAFE_FILE_LIST sentinel
- `read_write_round_trip_with_safe_file_list_error_nonvarint` - non-varint sentinel
- `read_write_round_trip_with_varint_end_marker` - varint zero and non-zero
- `read_flags_returns_io_error_in_varint_mode` - flags-level IoError variant
- `read_end_of_list_varint_with_error_returns_io_error` - varint error path
- `finish_with_io_error` - batched writer io_error forwarding

### Transfer crate (multiplex reader)

- `multiplex_reader_accumulates_msg_io_error` - OR accumulation
- `multiplex_reader_io_error_wrong_payload_length_ignored` - malformed payload
- `server_reader_take_io_error_plain_returns_zero` - plain mode fallback
- `server_reader_take_io_error_multiplex_accumulates` - server reader delegation
- `msg_io_error_round_trip_through_multiplex_layer` - full IOERR_GENERAL |
  IOERR_VANISHED round-trip with forwarding and exit code mapping

### Transfer crate (receiver)

- `proto_io_error.rs` - protocol < 30 io_error read path (4 tests)

### Gaps

- No test for multi-segment io_error accumulation (INC_RECURSE)
- No test combining flist io_error with MSG_IO_ERROR
- No interop test verifying upstream receiver parses oc-rsync io_error
- No interop test verifying oc-rsync receiver parses upstream io_error
- No test for `ignore_errors` interaction with protocol 30+ paths

## 9. Cross-references

- ISI.f.1 failure-mode catalog:
  `docs/design/isi-f-1-sender-inc-recurse-failure-modes.md` (section 7
  defines the receiver-side assertion framework this spec implements)
- ISI.a sender call graph:
  `docs/design/isi-a-sender-inc-recurse-call-graph.md` (section 3
  documents segment boundary logic)
- io_error_flags module:
  `crates/transfer/src/generator/io_error_flags.rs`
- Flist end marker write:
  `crates/protocol/src/flist/write/encoding.rs::write_end()`
- Flist end marker read:
  `crates/protocol/src/flist/read/flags.rs::read_flags()`
- Multiplex io_error:
  `crates/transfer/src/reader/multiplex.rs::handle_io_error_msg()`
- Receiver stats assembly:
  `crates/transfer/src/receiver/transfer/sync.rs` (line 416),
  `pipelined.rs` (line 73), `pipelined_incremental.rs` (line 43)
- Existing ISI.f test:
  `tests/inc_recurse_sender_flist_io_error_isi_f.rs`
- Memory note: `project_v061_daemon_push_increcurse_disable`
