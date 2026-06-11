# EDG-GOODBYE: daemon-sender goodbye NDX_DONE contract

## Background

The daemon-sender goodbye phase is the last byte sequence the generator
emits before the socket closes. Upstream rsync's `read_final_goodbye()`
(`main.c:875-906`) expects exactly one of two shapes on the wire:

- **Plain goodbye**: a single `NDX_DONE` sentinel.
  - Protocol < 30: 4-byte LE `0xFF 0xFF 0xFF 0xFF` (i.e. `write_int(-1)`).
  - Protocol >= 30: single byte `0x00` (modern varint encoding).
- **Extended goodbye** (protocol >= 31, `--delete --stats`): an
  `NDX_DEL_STATS` sentinel followed by five deletion-count varints
  (files, dirs, symlinks, devices, specials), then `NDX_DONE`.

Any other final byte sequence -- including a missing `NDX_DONE`, a
trailing payload frame, or a sentinel emitted after `NDX_DONE` -- causes
the receiver to either error out with "connection unexpectedly closed"
(UTS-9.REOPEN) or hang waiting for goodbye (UTS-15.c).

This audit records the contract that the EDG-GOODBYE.1/.2/.3 test set
locks in. Future work on UTS-9.REOPEN and UTS-15.c can rely on the
invariant being machine-checked.

## Contract

For every supported `(protocol, delete-flag, late-delete, compression)`
combination on the daemon-sender path:

1. The goodbye byte stream is non-empty.
2. The stream parses cleanly back through the modern or legacy NDX codec
   matching the negotiated protocol version.
3. The last protocol-frame in the stream is `NDX_DONE`.
4. There are no bytes after `NDX_DONE`.
5. Compression (`-z`) does not alter the goodbye bytes -- it operates on
   pre-goodbye file payload only.
6. `NDX_DEL_STATS`, when emitted, is always followed by exactly five
   varints and then `NDX_DONE`. There is no path that emits del-stats
   without a trailing `NDX_DONE`.
7. Protocol < 31 never emits `NDX_DEL_STATS` even when `--delete` is
   active, matching the `supports_extended_goodbye()` gate. The wire
   stream is byte-identical to a plain push.

## Test coverage

All three tasks live in `crates/protocol/tests/goodbye_contract.rs`.

### EDG-GOODBYE.1 -- wire-byte golden tests

Five `#[test]` cases cover the canonical scenarios:

- `golden_plain_push_proto32_ends_with_ndx_done` -- single `0x00` byte.
- `golden_plain_push_proto29_ends_with_legacy_ndx_done` -- 4-byte LE `-1`.
- `golden_push_delete_proto32_emits_del_stats_then_ndx_done` -- full
  `NDX_DEL_STATS + 5 varints + NDX_DONE` sequence with a non-trivial
  deletion payload.
- `golden_compression_does_not_alter_goodbye` -- pins the invariant that
  the `-z` flag does not couple into the goodbye byte stream.
- `golden_proto30_delete_does_not_emit_del_stats` -- gate check that
  pre-protocol-31 stays byte-identical to a plain push even with
  `--delete` requested.

Golden bytes are built in-process via the same codec primitives that the
production `GeneratorContext::handle_goodbye` uses (see
`crates/transfer/src/generator/transfer/goodbye.rs`). No external `.bin`
fixtures -- the test is deterministic and self-contained.

### EDG-GOODBYE.2 -- 100-iteration stress test

`sequential_daemon_transfers_dont_drop_goodbye` rotates through five
scenarios (`plain-32`, `delete-32`, `delete-31`, `plain-30`, `plain-29`)
for 100 iterations. Each iteration emits and parses the goodbye stream
through an in-process buffer, asserts the final NDX is `NDX_DONE`, and
counts any drops. A single drop fails the test, giving ~1% sensitivity
to flakes.

Per the EDG-GOODBYE.2 brief, full-daemon integration is intentionally
out of scope here -- the emit / parse pair exercises the same codec
invariants without socket setup or process spawn cost. UTS-9.REOPEN and
UTS-15.c can add full-stack stress on top of this base check.

### EDG-GOODBYE.3 -- proptest

Three `proptest!` cases:

- `daemon_sender_always_emits_ndx_done_before_close` -- the headline
  contract. For arbitrary `(protocol in 28..=32, send_del_stats, stats)`
  the last frame must be `NDX_DONE` and the stream must be fully
  consumed.
- `ndx_del_stats_is_always_followed_by_ndx_done` -- on protocol 31..=32
  with `send_del_stats = true`, the parsed sequence is exactly
  `NDX_DEL_STATS, five varints, NDX_DONE`.
- `pre_proto31_never_emits_del_stats` -- on protocol 28..=30 the wire
  bytes are byte-identical regardless of whether `send_del_stats` is
  set.

## Failure modes this contract guards against

- **Missing `flush()` before close** -- the stress test asserts the
  parse loop sees `NDX_DONE` as the final NDX. A code path that drops
  the goodbye byte during a partial socket close fails immediately.
- **Compression accidentally wrapping the goodbye** -- the golden
  compression test pins the byte sequence so a refactor that routes the
  sentinel through a multiplexed compressed frame fails loud.
- **Premature `NDX_DEL_STATS` without trailing `NDX_DONE`** -- the
  proptest pair (`ndx_del_stats_is_always_followed_by_ndx_done` plus the
  headline test) ensures no scenario reaches the wire where del-stats
  are emitted in isolation.
- **Cross-protocol regression on the extended-goodbye gate** -- a future
  change that accidentally enables `NDX_DEL_STATS` emission on protocol
  30 (or below) is caught by `pre_proto31_never_emits_del_stats` and
  `golden_proto30_delete_does_not_emit_del_stats`.

## Timeout + partial-cutoff (EDG-GOODBYE.4)

`crates/transfer/src/receiver/tests/errors_and_timeouts/goodbye_partial_cutoff.rs`
locks in the *receiver*-side counterpart of this contract. While
`crates/protocol/tests/goodbye_contract.rs` pins what the sender must
emit, EDG-GOODBYE.4 pins what the receiver must do when the sender
misbehaves. None of the cases below are permitted to panic; every
adverse stream shape must surface a typed `io::Error`.

Protocol coverage note: `handle_goodbye` only *reads* a goodbye echo on
protocols >= 31 (the extended-goodbye gate). On protocol 28/29 the
read-side counterpart is `read_expected_ndx_done`, which
`exchange_phase_done` invokes for each per-phase, per-segment, and final
NDX_DONE. The protocol-29 tests therefore drive
`read_expected_ndx_done` directly; the protocol-32 tests drive
`handle_goodbye` directly.

Eight receiver-side cases are covered:

- `read_expected_ndx_done_proto29_eof_mid_legacy_ndx_done` -- legacy
  codec short-reads the 4-byte NDX_DONE; `UnexpectedEof` must propagate
  from `read_exact`.
- `handle_goodbye_proto32_eof_mid_modern_negative_prefix` -- modern
  codec reads the `0xFF` prefix and then EOFs on the follow-up byte;
  `UnexpectedEof` must propagate.
- `handle_goodbye_proto32_immediate_eof` /
  `read_expected_ndx_done_proto29_immediate_eof` -- sender closes
  before emitting any goodbye byte. `UnexpectedEof` must propagate; the
  receiver must not declare success.
- `handle_goodbye_proto32_rejects_garbage_in_place_of_ndx_done` --
  sender emits a well-framed positive NDX (5) instead of `NDX_DONE`;
  receiver must surface `io::ErrorKind::InvalidData` with a message
  that references `NDX_DONE` and the role trailer. Upstream equivalent:
  `main.c:919-923` exits with `RERR_PROTOCOL`.
- `read_expected_ndx_done_proto29_rejects_garbage` -- same shape on the
  legacy protocol, exercising the `read_expected_ndx_done` helper that
  the phase-transition exchange calls.
- `handle_goodbye_does_not_silently_complete_on_hung_sender` --
  receiver is driven against a reader that parks forever (mimicking a
  sender that hangs without emitting goodbye). The test bounds its own
  wall-clock with `mpsc::recv_timeout(2s)`. A regression that lets
  `handle_goodbye` return `Ok(())` on zero input fails the test loudly.
  If a future change wires an internal read deadline, the surfaced
  error must still be `TimedOut` or `UnexpectedEof` -- the test accepts
  either kind so it does not over-specify the implementation.
- `handle_goodbye_proto32_eof_inside_del_stats_payload` -- sender emits
  `NDX_DEL_STATS` and then cuts the socket inside the five-varint
  payload; `UnexpectedEof` must propagate from
  `DeleteStats::read_from`.

A happy-path sanity check
(`handle_goodbye_proto32_accepts_well_formed_ndx_done`) ensures the
error-path validators do not regress the well-formed case.

### What this guards against

- A future refactor of `read_expected_ndx_done` that converts the
  `InvalidData` branch into a `debug_assert!` or a panic.
- A future refactor of `handle_goodbye` that consumes the `NDX_DEL_STATS`
  branch without propagating short-read errors from
  `DeleteStats::read_from`.
- A regression that lets the receiver silently declare goodbye complete
  on a hung or socket-closed sender -- the upstream behaviour is to
  block until bytes arrive, EOF arrives, or the user-configured
  `--timeout` fires; never to fabricate success.

## Upstream references

- `main.c:875-906` -- `read_final_goodbye()`.
- `main.c:883` -- protocol < 29 path: `read_int(f_in)`.
- `main.c:885-886` -- protocol >= 29 path: `read_ndx_and_attrs()`.
- `rsync.c:337-342` -- `NDX_DEL_STATS` handling inside `read_ndx_and_attrs()`.
- `generator.c:2376-2381` -- early del-stats path (`delete_during` /
  `delete_before` timing).
- `generator.c:2420-2425` -- late del-stats path (`delete_delay` /
  `delete_after` timing).
- `io.c:2243-2287` -- modern `write_ndx()` wire format.
- `io.c:2289-2318` -- modern `read_ndx()` wire format.

## Related tasks

- **UTS-9.REOPEN** and **UTS-15.c** will fix the daemon-sender failure
  modes this contract describes. With EDG-GOODBYE in place, those tasks
  can land regressions confidently against a stable, machine-checked
  contract.
- **UTS-6** (PR #5586) established the equivalent receiver-side
  `pending_del_stats` + `handle_goodbye` pattern. EDG-GOODBYE is the
  sender-side mirror that pins the wire contract both sides depend on.
