# WPG-7.c - prioritised IOCP gap list

Audit-only synthesis of the four io_uring -> IOCP gaps surfaced by
`docs/design/wpg-7-iouring-opcode-inventory.md` (WPG-7.a) and
`docs/design/wpg-7b-iouring-iocp-mapping.md` (WPG-7.b). This document
ranks the gaps by user-visible impact, sketches the recommended Win32
workaround for each, and points at the follow-up task that owns the fix.
No source changes are made by this task.

Inputs:

- WPG-7.a inventory: 23 distinct opcodes in use, 14 SQE-side plus 9
  registration / setup ops (WPG-7.a lines 105-117).
- WPG-7.b mapping: 4 confirmed gaps - `NOP`, `READ_FIXED`,
  `WRITE_FIXED`, `LINK_TIMEOUT` (WPG-7.b lines 78-83). `SEND_ZC` is
  handed to WPG-8 because `TransmitFile` / `RIOSend` cover the
  zero-copy semantics with a different shape rather than a missing peer
  (WPG-7.b lines 86-90, 107-112).

## Priority legend

- **P0** - blocks data-path parity on Windows. The hot path pays for
  this on every byte.
- **P1** - control-path gap with a closed-form workaround. Paid per
  batch or per file, not per byte.
- **P2** - no-op or test-only. Closing it has no production impact.
- **P3** - already routed to another work-package (`SEND_ZC` ->
  WPG-8); deferred from this list.

## Prioritised gap table

| # | io_uring opcode | Priority | User-visible impact today | Recommended Win32 workaround | Effort | Owner / follow-up |
|---|---|---|---|---|---|---|
| 1 | `IORING_OP_READ_FIXED` / `IORING_OP_WRITE_FIXED` (file side) | **P0** | Every file `ReadFile` / `WriteFile` pays a per-call page-lock and IRP allocation. Without a registered-buffer fast path the steady-state read/write loop is bounded by per-SQE pinning cost that Linux eliminates. RIO covers sockets but not files, so the data-path delta is structural, not tunable (WPG-7.b lines 45-46, 81-82). | Front-end the existing `BufferPool` with a fixed-size pinned arena; allocate from it for every overlapped read/write so the kernel sees the same pages repeatedly and can keep them resident. Where the source is a socket and the sink is a file (network -> disk), pre-register the socket side via RIO (`RIORegisterBuffer` -> `RIO_BUFFERID`) and let the file side fall back to overlapped `WriteFile`. Document the asymmetry. | **L** | **WPG-9** (registered-buffer scheme on Windows), per WPG-7.b lines 113-120. |
| 2 | `IORING_OP_LINK_TIMEOUT` | **P1** | Back-pressured `WSASend` cannot be bounded atomically in the kernel. A stuck socket would today hold the batched-send path indefinitely; the Linux side bounds it via the linked timeout on every batched send poll gate (WPG-7.a lines 50, 93). | Per overlapped send: arm a `CreateWaitableTimerExW` via `SetThreadpoolWait`; on timer fire call `CancelIoEx(handle, lpOverlapped)` against the in-flight overlapped. Each operation needs its own bookkeeping pair; the atomic kernel linkage io_uring provides is unavailable (WPG-7.b lines 56-57). | **M** | **New: WPG-10** (Windows linked-timeout shim). |
| 3 | `IORING_OP_NOP` | **P2** | None in production. Used only by `io_uring/registered_buffers/tests/drop_contract.rs:31` as a stub SQE for drop-contract assertions (WPG-7.a line 35). | If the equivalent IOCP-side drop-contract harness ever needs an inert completion to round-trip the port, post one via `PostQueuedCompletionStatus(iocp, 0, KEY_NOP, NULL)` with a sentinel completion key. | **S** | **New: WPG-11** (IOCP test harness parity). Optional; only file if WPG-9 requires drop-contract coverage symmetric to Linux. |
| 4 | `IORING_OP_SEND_ZC` (handed off, not a true gap) | **P3** | Default builds already skip `SEND_ZC` (feature-gated behind `iouring-send-zc` and floored by `MIN_SEND_ZC_PAYLOAD`); on Windows the practical zero-copy peers exist (`TransmitFile`, `TransmitPackets`, `RIOSend`) but with differing notification shapes (single overlapped completion vs value + release CQE) (WPG-7.a line 42, WPG-7.b line 49). Not a missing peer - a design decision deferred to WPG-8. | None in WPG-7.c. Captured for completeness so the gap-list reads against the same opcode set as WPG-7.a. | **L** | **WPG-8** (zero-copy socket send on Windows), per WPG-7.b lines 106-112. |

## Severity rationale

- **Row 1 is P0** because it is the only gap on the data path. WPG-7.a
  classifies `READ_FIXED` and `WRITE_FIXED` as the conditional fast
  path engaged whenever the buffer registry has free slots (WPG-7.a
  lines 38-39, 81-82); WPG-7.b confirms Win32 has no file-side
  registered-buffer scheme at all (WPG-7.b lines 45-46, 65-66, 81-82).
  Every transferred byte traverses this opcode pair when the slot
  table is warm, so the gap taxes throughput linearly with payload
  size.
- **Row 2 is P1** because it is a control-path safety net rather than
  a per-byte cost. The linked timeout fires only when a back-pressured
  send poll gate stalls (WPG-7.a line 50). A workaround exists and is
  closed-form, but it requires per-operation bookkeeping the Linux
  side gets atomically (WPG-7.b lines 56-57).
- **Row 3 is P2** because the opcode is test-only. WPG-7.a's
  dispatch-classification table flags `NOP` as `test-only` in the
  conditional column with no default-on, feature-gated, or probe entry
  (WPG-7.a lines 78). No production caller exists.
- **Row 4 is P3** because WPG-7.b's gap summary explicitly excludes
  `SEND_ZC` from the gap count - "the delta is a WPG-8 design task,
  not a missing peer" (WPG-7.b lines 86-90). Listing it here keeps
  the synthesis aligned with the inventory's opcode universe.

## Open issues to file

- **WPG-10 (new)** - Windows linked-timeout shim. Owner: `fast_io` IOCP
  path. Design: per-overlapped waitable timer + threadpool wait +
  `CancelIoEx` glue; reuses the back-pressure timeout budget the
  io_uring path already configures. Acceptance: parity test that a
  stalled `WSASend` is cancelled within the configured deadline and
  reported through the IOCP completion path with the expected error
  shape.
- **WPG-11 (new, optional)** - IOCP test harness parity for
  drop-contract assertions. Only required if WPG-9 lands a Windows
  drop-contract test analogous to
  `io_uring/registered_buffers/tests/drop_contract.rs`. Scope: a
  helper that posts a sentinel completion via
  `PostQueuedCompletionStatus` and drains it through
  `GetQueuedCompletionStatusEx`.
- No new issue for **rows 1 and 4** - they are already covered by
  WPG-9 and WPG-8 respectively (WPG-7.b lines 22-26, 106-124).

## Recommendation to sprint planning

Budget priority order, given finite cycles:

1. **WPG-9 first.** Row 1 is the only P0 and it is the only gap that
   touches the steady-state data path. Every other gap has either a
   cheap workaround (WPG-10) or no production impact (WPG-11).
   Closing it removes the structural file-side throughput delta vs
   Linux io_uring.
2. **WPG-8 next.** Row 4 is P3 in this list but it is the second-
   highest cycle-budget item across the WPG-7 family; the zero-copy
   socket-send path matters on the network -> disk pipeline once the
   file-side fast path lands. Sequencing WPG-8 after WPG-9 lets the
   pinned arena from WPG-9 feed `RIOSend` cleanly.
3. **WPG-10 third.** Row 2's workaround is mechanical and small. It
   blocks no other work but is needed before the IOCP path can claim
   parity with the back-pressure safety net Linux already has.
4. **WPG-11 last (or skip).** File only if WPG-9 acceptance criteria
   require symmetric drop-contract coverage on Windows. Otherwise
   leave the gap open and document it in the WPG-9 deliverable.

If the cycle budget is **N=1**, do **WPG-9**. If **N=2**, do **WPG-9**
and **WPG-8**. If **N=3**, add **WPG-10**. **WPG-11** is contingent on
WPG-9's test plan, not on cycle budget.

## Cross-references

- WPG-7.a (opcode inventory): `docs/design/wpg-7-iouring-opcode-inventory.md`
  - SQE-op table: lines 33-50.
  - Dispatch classification: lines 76-101.
  - Counts: lines 105-117.
- WPG-7.b (IOCP mapping): `docs/design/wpg-7b-iouring-iocp-mapping.md`
  - SQE-op mapping table: lines 40-57.
  - Registration / setup mapping table: lines 61-70.
  - Gap summary: lines 72-90.
  - Cross-references to WPG-8 / WPG-9: lines 104-124.
- WPG-8 (zero-copy socket send on Windows): to be opened; primary
  input is the `IORING_OP_SEND_ZC` row of WPG-7.b.
- WPG-9 (registered-buffer scheme on Windows): to be opened; primary
  inputs are the `IORING_OP_READ_FIXED` / `IORING_OP_WRITE_FIXED` and
  `IORING_REGISTER_BUFFERS` / `IORING_REGISTER_PBUF_RING` rows of
  WPG-7.b.
- WPG-10 (Windows linked-timeout shim): to be opened by this document.
- WPG-11 (IOCP test harness parity): contingent on WPG-9.
