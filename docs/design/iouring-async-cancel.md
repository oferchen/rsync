# io_uring ASYNC_CANCEL for in-flight SQE cancellation (#2205)

## Status

Design analysis. No code changes proposed in this document. Recommendation:
**defer until an actual abort-leak bug is reported**, with the rationale and
trigger conditions captured below.

## Problem statement

Task #2205 asks whether the io_uring fast path issues
`IORING_OP_ASYNC_CANCEL` when a transfer is aborted mid-flight. The concern is
that in-flight SQEs run to completion inside the kernel even after the
caller has decided the transfer should abort; on a slow disk or a slow
destination network the kernel can keep writing to a file the user has
already abandoned (Ctrl-C, fatal protocol error, transport teardown).

This document records what the codebase actually does today, what
`ASYNC_CANCEL` would buy, the wiring sketch, the race window, what a
verification test would have to prove, and why the recommendation is to
defer.

## 1. Current abort path

### 1.1 Every io_uring submission is synchronous

The submission helpers in `crates/fast_io/src/io_uring/` all follow the
same pattern: push one or more SQEs, then immediately call
`submit_and_wait` and drain the matching CQEs before returning to the
caller. There is no path in which an SQE is left in-flight across the
return.

- `crates/fast_io/src/io_uring/file_writer.rs:227` (`write_at`):
  one write SQE, then `self.ring.submit_and_wait(1)?`.
- `crates/fast_io/src/io_uring/file_writer.rs:414` (`sync` / fsync):
  one fsync SQE, then `self.ring.submit_and_wait(1)?`.
- `crates/fast_io/src/io_uring/file_reader.rs:138`,
  `crates/fast_io/src/io_uring/file_reader.rs:260`: one or batched reads,
  then `self.ring.submit_and_wait(...)?`.
- `crates/fast_io/src/io_uring/disk_batch.rs:250`: fsync,
  `self.ring.submit_and_wait(1)?`.
- `crates/fast_io/src/io_uring/batching.rs:108` (`submit_write_batch`)
  and `crates/fast_io/src/io_uring/batching.rs:335`
  (`submit_send_batch`): inner loop submits up to `max_sqes` SQEs, then
  `ring.submit_and_wait(submitted)?` and drains every CQE before
  advancing.
- `crates/fast_io/src/io_uring/registered_buffers.rs:550` and
  `crates/fast_io/src/io_uring/registered_buffers.rs:670`: the
  `READ_FIXED` / `WRITE_FIXED` batch paths submit and wait inside the
  same call.
- `crates/fast_io/src/io_uring/linkat.rs:190`,
  `crates/fast_io/src/io_uring/renameat2.rs:174`: single-shot metadata
  operations that submit and wait.

The shared-ring API at `crates/fast_io/src/io_uring/shared_ring.rs:308`
exposes `submit_and_wait` and `reap` as separate calls, so it *could*
leave SQEs in-flight across calls. In practice the only caller submits
and drains in the same loop iteration; there is no consumer that
parks SQEs across an iteration boundary.

### 1.2 What happens when the caller aborts

Because every submission is `submit + wait + drain` inside one function,
the abort window is narrow: a Ctrl-C that arrives between SQE push and
`submit_and_wait` either (a) interrupts the syscall and `submit_and_wait`
returns `EINTR`, or (b) lands while the kernel is executing the
operation and the syscall completes normally. Either way the SQE has
already been consumed by the kernel and will produce a CQE; the kernel
will not "un-write" bytes that hit the disk.

If the caller decides to abort *after* the helper returned, the I/O is
already complete. There is nothing to cancel.

The Drop impls do exactly one thing - best-effort flush:

- `crates/fast_io/src/io_uring/file_writer.rs:444`
  (`impl Drop for IoUringWriter`): `let _ = self.flush_buffer();`.
- `crates/fast_io/src/io_uring/disk_batch.rs:293`
  (`impl Drop for IoUringDiskBatch`): `let _ = self.flush_current();`
  followed by `finalize_current_file`.

Neither Drop attempts to cancel anything, because there is nothing
queued to cancel: every prior write call returned only after its SQE's
CQE was reaped.

### 1.3 Where the signal flags are consulted

`core::signal` (defined in `crates/core/src/signal/mod.rs:79-209` and
`crates/core/src/signal/unix.rs:129-150`) sets `SHUTDOWN_REQUESTED` and
`ABORT_REQUESTED` from the SIGINT/SIGTERM/SIGHUP handlers. The
`CleanupManager` at `crates/core/src/signal/cleanup.rs:43-194` is the
only mechanism that runs from the abort path; it removes registered
temp files and runs cleanup callbacks.

A grep for `is_shutdown_requested` / `is_abort_requested` across the
workspace returns hits only in tests and examples; no production code in
`fast_io`, `engine`, or `protocol` consults the flags. The temp file
cleanup path is the *only* way the system reacts to a signal today. So
there is no spot inside an io_uring submission loop where adding an
`ASYNC_CANCEL` would be triggered today; the surrounding plumbing would
have to be added first.

## 2. `IORING_OP_ASYNC_CANCEL` semantics

The opcode (kernel `io_uring.h`, exposed in the `io_uring` Rust crate as
`opcode::AsyncCancel`) cancels one or more in-flight SQEs. Two match
modes exist:

- **Match by user_data** (the default). The cancel SQE carries the
  `user_data` value of the target SQE; the kernel walks its in-flight
  list and cancels the first match. Standard idiom: stamp every SQE
  with a unique tag, remember the tag, and submit a cancel for that
  tag.
- **Match by fd** (`IORING_ASYNC_CANCEL_FD` plus `IORING_ASYNC_CANCEL_ALL`,
  added in Linux 5.19). The cancel SQE names an fd and cancels all
  in-flight ops against it. Useful when the tracking layer cannot enum
  individual tags.

Possible CQE results:

- `0` - the target was found and cancelled before completion. The
  cancelled SQE then posts its own CQE with `-ECANCELED`.
- `-ENOENT` - the SQE already completed (or was never queued under
  that tag). This is the dominant race outcome on a fast system.
- `-EALREADY` - the kernel is in the middle of processing the SQE and
  cancellation could not be applied; the SQE will complete normally.

A cancel CQE therefore has to be checked against all three outcomes;
treating `-ENOENT` as an error would be incorrect because it just means
the I/O won the race.

## 3. Wiring sketch (hypothetical)

The sketch below assumes a future asynchronous io_uring path that
actually keeps SQEs in-flight across function returns. Today's path
does not, so this is academic until #4217 (async io_uring) or #2243
(per-thread rings with concurrent submitters) lands.

### 3.1 Tag every SQE

The `SharedRing` already partitions `user_data` into a 1-byte `OpTag`
plus a 56-bit `op_id`
(`crates/fast_io/src/io_uring/shared_ring.rs:25-42`). Today only the
`OpTag` enum's four variants are defined; an async cancel scheme would
add `OpTag::Cancel` and treat the 56-bit `op_id` as the cancel target's
`op_id`.

The per-channel paths in `file_writer.rs` and `disk_batch.rs` currently
emit `.user_data(0)` (e.g. `file_writer.rs:217`,
`file_writer.rs:404`, `disk_batch.rs:238`) because nothing demultiplexes
their CQEs - the helper drains exactly the CQE it just submitted. Those
paths would need to allocate unique tags before they could be cancellable.

### 3.2 Track in-flight tags

Add an `Inflight` registry to whatever owns the long-lived ring (the
session ring pool from #1937, the per-thread ring from #2243, or a
dedicated state object for an async submitter):

```text
struct Inflight {
    // tag -> { fd, kind, started_at }
    pending: HashMap<u64, InflightOp>,
}
```

On submit: insert the tag. On CQE drain: remove the tag. On abort:
iterate the `pending` map and submit a cancel SQE per entry, then drain
the cancel CQEs and the resulting `-ECANCELED` CQEs.

For a fd-bulk cancel the registry can collapse to a set of fds
(`HashSet<RawFd>`), at the cost of cancelling more than necessary if
multiple ops target the same fd.

### 3.3 Submit cancels

```text
let cancel = opcode::AsyncCancel::new(target_user_data)
    .build()
    .user_data(OpTag::Cancel.encode(target_op_id));
unsafe { ring.submission().push(&cancel)?; }
ring.submit()?;  // do not wait
```

The cancel itself is best-effort and must not block on completion - the
caller is already on the abort path. Use `submit()` rather than
`submit_and_wait`.

### 3.4 Buffer ownership

The borrowed-slice work in #2208 (see
`docs/design/iouring-borrowed-slice-consumer.md`) is the long-pole
constraint here. While an SQE is in-flight, its backing buffer cannot be
freed because the kernel still owns the page. After an `ASYNC_CANCEL`,
the SQE will eventually post `-ECANCELED`; the buffer can be released
only after that CQE is reaped. Cancellation does not buy faster buffer
reclamation; it only stops *further* bytes from being written to the
destination fd.

This is why the Drop impls today do not even attempt cancellation: the
synchronous wait pattern already guarantees the kernel has released the
buffer before Drop runs.

## 4. The race: completion before cancel reaches the kernel

The dominant case on a healthy system is that the SQE completes between
"caller decides to abort" and "kernel sees the cancel". The completion
arrives as a normal CQE with the original `user_data`; the cancel
arrives shortly after with `-ENOENT`.

Two checks are required for correctness:

1. **Cancel-completion tag check.** When the cancel CQE arrives, decode
   its `user_data` to confirm it is an `OpTag::Cancel`. A negative
   `result` of `-ENOENT` is *not* an error - it is the expected race
   outcome. Surface it as a log entry at most, never as `io::Error`.
2. **Cancelled-op completion check.** When a cancelled op's CQE arrives
   with `-ECANCELED`, the buffer can be released and the inflight
   entry removed. If the kernel raced and the op completed normally,
   the CQE arrives with the original `result` (bytes written) and
   the buffer release path is identical - the only consequence is that
   some bytes hit the destination after the user aborted.

No code path should distinguish "cancel hit" from "I/O won the race"
for correctness; both are valid terminal states.

## 5. Verification

`strace` proves the cancel SQE was submitted but is operator-only and
cannot run in CI without root. A useful test asserts the *effect* of
cancellation rather than the syscall.

### 5.1 Effect-based test sketch

```text
1. Create a destination file on a slow device, or use `posix_fallocate`
   to a very large size so the write is observably slow.
2. Start an oc-rsync transfer with a known-large source.
3. After ~1 s, send SIGINT.
4. After the process exits, assert:
   - destination is shorter than source, AND
   - destination is shorter than `bytes-the-test-saw-go-by`
     measured by progress output.
```

The test is a *correctness assertion* (we did not write all the bytes
the kernel was going to write) rather than a syscall trace. It only
runs on Linux with io_uring available, gated on `cfg(target_os = "linux")`
and a runtime probe.

### 5.2 Unit-level coverage

The cancel CQE handling can be unit tested without a real abort: build
a ring, submit a long-running op (e.g. `Read` on a freshly opened pipe
with no writer), submit a cancel for its tag, drive the ring forward,
assert the cancel CQE arrives with `0` or `-ENOENT` and the read CQE
arrives with `-ECANCELED`. This validates the bookkeeping in isolation.

### 5.3 What is hard to verify

Bulk-cancel-by-fd against many in-flight SQEs requires kernel >= 5.19;
older kernels will silently downgrade to per-tag cancel or fail with
`-EINVAL`. The runtime detection has to live in the probe helper next
to `probe_poll_add` at
`crates/fast_io/src/io_uring/shared_ring.rs:355-364`.

## 6. Recommendation: defer

The cost-benefit ranking is:

1. **No present in-flight SQEs to cancel.** Every io_uring helper in
   the tree today is `submit + wait + drain`. There is no window
   during which an SQE is in-flight without the caller already
   blocking on its CQE. The "writes to an abandoned destination"
   problem described in #2205 cannot occur on the current path; the
   *only* way bytes hit the destination after a Ctrl-C is the bytes
   that were already in the kernel's submission queue when the signal
   arrived, and `submit_and_wait` was about to return anyway.
2. **No production signal-check inside io_uring code.** Even if
   cancellation were wired, nothing inside `fast_io` consults
   `core::signal::is_abort_requested`. The plumbing to *invoke*
   cancellation would require touching every helper above, plus
   threading the signal flag (or a cancellation token) through every
   call site. That is a bigger refactor than the cancellation itself.
3. **Cancellation interacts with buffer ownership.** Until #2208
   resolves whether consumers borrow registered-buffer slices,
   cancellation cannot offer "free the buffer immediately" semantics
   that would justify the complexity. The buffer must still wait for
   `-ECANCELED` before reclaim, so cancellation today buys nothing
   beyond "stop further bytes from being written" - and the current
   synchronous wait already prevents that scenario.
4. **The architectural direction matters.** #2243 (per-thread rings)
   and #4217 (async io_uring) are the prerequisites for any code path
   that keeps SQEs in flight across function returns. Either of those
   will need a cancellation story baked in from the start; designing it
   speculatively now means re-doing it once those land.
5. **No bug report.** No issue, audit, or interop run has surfaced a
   "transfer aborted but destination kept growing" symptom. The
   current synchronous helpers prevent the failure mode by
   construction.

The defer condition is concrete: re-open this work when *either*
(a) #4217 / #2243 lands a code path that keeps SQEs in-flight across
the abort decision point, *or* (b) an abort-leak bug report appears
with a reproducer that shows the destination growing past the
abort signal. Until then the synchronous wait pattern is the
cancellation strategy.

## 7. Cross-references

- **#2243** - per-thread io_uring rings. Prerequisite for any code path
  that holds in-flight SQEs across function boundaries; cancellation
  becomes meaningful only when there is something to cancel that
  outlives the caller's stack frame. See
  `docs/design/io-uring-ring-pool.md`.
- **#1937** - session ring pool. Centralises ring ownership and is the
  natural place to put an `Inflight` registry if one is ever added. See
  `docs/design/iouring-session-ring-pool-impl.md` and
  `docs/design/iouring-session-ring-pool.md`.
- **#4217** - async io_uring. The first code path that would actually
  benefit from cancellation, because async submission inherently keeps
  SQEs in-flight across `.await` points.
- **#4220** - io_uring submission from rayon. Same rationale as #4217:
  worker threads submitting in parallel and not blocking on CQE drain
  are the workloads that need cancellation.
- **#2208** - borrowed-slice consumer for `READ_FIXED`. Cancellation
  interacts with buffer ownership: a cancelled SQE still owns its
  buffer until the `-ECANCELED` CQE is reaped, so any borrowed-slice
  contract has to reserve the slice across the cancel race window.
  See `docs/design/iouring-borrowed-slice-consumer.md`.

## Appendix: relevant call sites

For future re-evaluation, the submission sites that would each need a
unique `user_data` tag and an inflight-registry insert/remove are:

- `crates/fast_io/src/io_uring/file_writer.rs:214` (single `Write`),
  `crates/fast_io/src/io_uring/file_writer.rs:404` (`Fsync`).
- `crates/fast_io/src/io_uring/file_reader.rs:128` (single `Read`),
  `crates/fast_io/src/io_uring/file_reader.rs:244` (batched `Read`).
- `crates/fast_io/src/io_uring/batching.rs:90` (`submit_write_batch`),
  `crates/fast_io/src/io_uring/batching.rs:314` (`submit_send_batch`),
  `crates/fast_io/src/io_uring/batching.rs:186` (`PollAdd POLLOUT`),
  `crates/fast_io/src/io_uring/batching.rs:191` (`LinkTimeout`).
- `crates/fast_io/src/io_uring/disk_batch.rs:238` (`Fsync`).
- `crates/fast_io/src/io_uring/registered_buffers.rs:550` (`WRITE_FIXED`
  batch), `crates/fast_io/src/io_uring/registered_buffers.rs:670`
  (`READ_FIXED` batch).
- `crates/fast_io/src/io_uring/linkat.rs:180` (`Linkat`),
  `crates/fast_io/src/io_uring/renameat2.rs:164` (`Renameat2`).
- `crates/fast_io/src/io_uring/shared_ring.rs:238` (Read on shared
  ring), `crates/fast_io/src/io_uring/shared_ring.rs:264` (PollWrite),
  `crates/fast_io/src/io_uring/shared_ring.rs:291` (Send).
