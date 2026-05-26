# PIP-10.e - Error-path validation for parallel receive-delta mid-transfer failures

Tracking: PIP-10.e (#3026). Parent: PIP-10 (end-to-end parallel
receive-delta validation). Series siblings: PIP-10.a (happy-path byte
identity), PIP-10.b (phase-2 redo parity), PIP-10.c (metadata commit
ordering), PIP-10.d (stats accumulation).

Predecessors: FFB-1 (barrier API design), FFB-2 (barrier
implementation), PIP-7 (receiver corruption teardown), PIP-9 (production
wire-up), PIP-9.b (feed loop), PIP-9.f.1 (bake criterion).

## 1. Scope

PIP-10.e validates that the parallel receive-delta path handles
mid-transfer failures identically to the sequential path: same exit
codes, same error messages, same temp-file cleanup, same partial-transfer
semantics. The parallel path introduces concurrency hazards that do not
exist in upstream rsync's sequential `recv_files()` loop. Every error
scenario below must demonstrate that the parallel path does not leak
resources, orphan temp files, corrupt completed files, or report wrong
exit codes when a failure occurs while workers are in flight.

The validation targets `ParallelDeltaApplier` at
`crates/engine/src/concurrent_delta/parallel_apply/`, the
`ParallelDeltaPipeline` at `crates/transfer/src/delta_pipeline/parallel.rs`,
and the `DeltaConsumer` at
`crates/engine/src/concurrent_delta/consumer/`. All tests run under
`--features parallel-receive-delta`.

## 2. Invariants

Every error scenario must satisfy all six invariants:

**INV-1: Exit code parity.** The parallel path must produce the same
`ExitCode` variant (from `crates/core/src/exit_code/codes.rs`) as the
sequential path for an identical failure. The mapping from `io::Error`
to `ExitCode` goes through `ExitCode::from_io_error()`. Key mappings
relevant to these scenarios:

| Failure class | `io::ErrorKind` | `ExitCode` | Code |
|---|---|---|---|
| Broken pipe / sender disconnect | `BrokenPipe`, `ConnectionReset` | `SocketIo` | 10 |
| Disk full (ENOSPC) | `Other` (OS errno 28) | `FileIo` | 11 |
| File vanished | `NotFound` | `FileSelect` | 3 |
| Network timeout | `TimedOut` | `Timeout` | 30 |
| Corrupted data / checksum mismatch | `Other` (via `ParallelApplyError`) | `FileIo` | 11 |
| Protocol stream corruption | `UnexpectedEof`, `InvalidData` | `StreamIo` | 12 |
| Mutex poisoned (worker panic) | `Other` (via `ParallelApplyError::SlotPoisoned`) | `FileIo` | 11 |

Note: ENOSPC maps through the `_` arm of `from_io_error()` to `FileIo`
(code 11) because `std::io::ErrorKind::StorageFull` is unstable. This
matches upstream rsync which returns `RERR_FILEIO` for disk-full errors.

**INV-2: Temp-file cleanup.** No `.XXXXXX` temp files may remain in the
destination tree after the transfer completes (successfully or with
error). The receiver writes through temp files and renames on commit.
When an error aborts the transfer, all temp files from files that were
in-flight at the time of the error must be removed. Files that completed
their commit (rename) before the error are left in place.

**INV-3: Partial-transfer integrity.** Files whose `finish_file()` +
temp-file rename completed before the error must have correct content.
The parallel path must not corrupt already-committed files when
unwinding from a mid-transfer failure.

**INV-4: Worker drain.** All in-flight rayon workers must complete or be
cancelled before the transfer function returns. The
`drain_inflight()` method at
`crates/engine/src/concurrent_delta/parallel_apply/drain.rs` blocks until
every registered slot's in-flight counter reaches zero. No worker thread
may outlive the applier. No worker may hold a `SlotHandle` after
`drain_inflight()` returns. This is the FFB-1/FFB-2 guarantee.

**INV-5: Error message format.** Error messages emitted by the parallel
path must include the role trailer matching upstream rsync's format:
`... (code N) at <repo-rel-path>:<line> [<role>=<version>]`. The
`<role>` is one of `receiver`, `sender`, `generator`, `server`,
`client`, `daemon`. The parallel path runs on the receiver side, so all
its error messages must carry `[receiver=<version>]`.

**INV-6: Sequential-parallel behavioral identity.** For every error
scenario, running the same transfer with the sequential
`SequentialDeltaPipeline` must produce the same observable outcome (exit
code, error message content minus timing details, temp-file state,
committed-file state). The parallel path is a performance optimization;
it must not change failure semantics.

## 3. Error scenarios

### 3.1 Sender disconnects mid-file (broken pipe)

**Trigger:** The sender closes its socket (or pipe) while the receiver
is mid-way through reading a file's delta token stream. Chunks
already submitted to `apply_batch_parallel()` may be in flight on
rayon workers; the token reader encounters `BrokenPipe` or
`ConnectionReset` on the next `read_token()` call.

**Injection mechanism:** A test-only `TokenReader` wrapper that returns
`io::Error::new(ErrorKind::BrokenPipe, "simulated sender disconnect")`
after delivering N tokens for a file. N is chosen to be past the first
`apply_batch_parallel()` dispatch but before the file's `End` token, so
at least one batch is in flight.

**Expected behavior:**

1. The token loop propagates the `BrokenPipe` error upward.
2. The receiver calls `drain_inflight()` on the applier. All in-flight
   workers for all registered files complete their verify+write cycle or
   observe a poisoned slot and return.
3. The applier's `DashMap` is fully drained; no `SlotHandle` clones
   remain.
4. Temp files for in-flight files are removed by the receiver's cleanup
   path.
5. Files that completed `finish_file()` + rename before the pipe break
   remain intact with correct content.
6. Exit code: `SocketIo` (10).

**Concurrency hazard:** Workers holding `SlotHandle` clones may attempt
`lock_slot()` concurrently with the error-path `drain_inflight()`.
The barrier in `flush_workers()` must wait for these workers to release
their handles via `DecrementGuard::drop()`. The `Condvar`-based wait
in `BarrierState::wait_until_idle()` handles this correctly - but the
test must verify no deadlock occurs when the error path and worker
completion overlap.

### 3.2 Disk full on receiver (ENOSPC)

**Trigger:** The destination filesystem runs out of space while a
worker's `FileSlot::write_chunk()` calls `writer.write_all()`. The
`write_all` returns `io::Error` with OS errno 28 (ENOSPC).

**Injection mechanism:** A test-only `Write` implementation that returns
`io::Error::new(ErrorKind::Other, "No space left on device")` after
accepting B bytes of writes. B is chosen so at least one chunk writes
successfully before the error, and other files' chunks are queued in
the reorder buffer or in flight on rayon workers.

**Expected behavior:**

1. The `FileSlot::ingest()` call that surfaces the ENOSPC propagates
   the error through `apply_one_chunk()` or `apply_batch_parallel()`
   back to the caller.
2. The receiver marks the failed file for redo or records the failure.
3. `drain_inflight()` retires all other in-flight workers.
4. Workers for other files that have not yet hit the full disk may
   succeed or fail independently - the receiver collects per-file
   results.
5. All temp files (for both the failed file and any in-flight files
   that did not commit) are cleaned up.
6. Exit code: `FileIo` (11) for the transfer, or `PartialTransfer` (23)
   if some files succeeded and only some failed - matching upstream
   rsync's behavior where partial success yields code 23.

**Concurrency hazard:** Multiple workers writing to different files on
the same filesystem may each independently encounter ENOSPC. The first
error must not prevent the cleanup of other files' temp resources. The
per-file `Mutex<FileSlot>` isolates per-file write failures, but the
caller's error-collection loop must handle multiple failures.

### 3.3 File vanishes on sender mid-transfer

**Trigger:** The sender's generator discovers a file has disappeared
between file-list building and the transfer phase. Upstream rsync sends
a `MSG_ERROR_XFER` message and moves to the next file. The receiver
sees a truncated or absent token stream for that file's NDX.

**Injection mechanism:** For interop testing, create a large source
tree, start the transfer, and `unlink` a file on the sender side after
the file list is sent but before the file's data is read. For unit
testing, the mock token reader delivers an `End` token for the file
after zero data tokens, simulating the sender skipping the vanished
file.

**Expected behavior:**

1. The receiver handles the vanished-file notification per upstream
   protocol: the file is counted as a transfer error but does not abort
   the entire transfer.
2. If chunks were already registered via `register_file()` for the
   vanished NDX, `finish_file()` sees an empty or partial reorder
   buffer. If `drained()` returns `false`, the
   `UndrainedChunks` error surfaces.
3. The temp file for the vanished NDX is removed.
4. Other files' parallel workers continue unaffected.
5. Exit code: `Vanished` (24) if only vanished-file errors occurred;
   `PartialTransfer` (23) if mixed with other errors. Matches upstream.

**Concurrency hazard:** The vanished-file notification may arrive on the
wire while chunks for the vanished file are still queued in the reorder
buffer or in flight on a rayon worker. The receiver must not call
`finish_file()` until `flush_workers()` drains the in-flight workers
for that NDX, even though the file is being abandoned. Calling
`finish_file()` with workers still in flight would race against the
`Arc::try_unwrap` and hit `ApplierStillReferenced`.

### 3.4 Network timeout during chunk reception

**Trigger:** The network connection stalls. The receiver's read
operation times out after the configured `--timeout` duration. Chunks
may be in flight on rayon workers when the timeout fires.

**Injection mechanism:** A test-only `Read` wrapper that blocks for
longer than the timeout threshold on a specific read call, then returns
`io::Error::new(ErrorKind::TimedOut, "read timeout")`. The timeout
fires after several chunks have been dispatched to the applier.

**Expected behavior:**

1. The token reader's `read_token()` returns the timeout error.
2. The receiver propagates the error and initiates shutdown.
3. `drain_inflight()` waits for all in-flight workers. Workers that
   are mid-verify or mid-write complete normally (they operate on
   already-received data, not on the timed-out connection).
4. All temp files for uncommitted files are removed.
5. Exit code: `Timeout` (30).

**Concurrency hazard:** The timeout fires on the I/O thread (the
receiver's main loop), not on a rayon worker. Workers do not interact
with the network connection directly - they read from `DeltaChunk.data`
which was already materialized. The drain is therefore bounded by the
verify+write time of the in-flight chunks, not by any network
operation. However, if the `DeltaConsumer`'s background thread is
blocked on `WorkQueueReceiver::drain_parallel()` waiting for more work,
the `flush` must close the `WorkQueueSender` to unblock it. The
`ParallelDeltaPipeline::flush()` does this by taking `self.work_tx`.

### 3.5 Corrupted chunk data (checksum mismatch)

**Trigger:** A chunk's `expected_strong` digest does not match the
digest computed by `verify_chunk()`. This occurs when the wire data is
corrupted in transit or a bug in the delta reconstruction produces
wrong bytes.

**Injection mechanism:** Submit a `DeltaChunk` with a deliberately
incorrect `expected_strong` value via `with_expected_strong()`. The
existing test `verify_chunk_rejects_mismatched_digest_and_does_not_write`
covers the single-chunk case; PIP-10.e extends coverage to the
mid-batch scenario where some chunks in a batch verify successfully
before the mismatch is detected.

**Expected behavior:**

1. `verify_chunk()` returns `ParallelApplyError::ChecksumMismatch`
   with the typed fields: `ndx`, `chunk_sequence`, `algorithm`,
   `expected` (hex), `actual` (hex).
2. Under `apply_batch_parallel()`, rayon's parallel `collect` short-
   circuits on the first error. Chunks that verified successfully
   before the error was observed are not written (the verify step
   runs before the serial write loop).
3. The receiver maps the error to a phase-2 redo for the affected file
   (matching upstream rsync's `MSG_REDO` behavior on checksum
   mismatch at the per-file level).
4. Other files in the same batch or in other batches are unaffected.
5. The corrupt bytes never reach the destination writer. The
   `bytes_written` counter for the affected NDX stays at its
   pre-error value.
6. Exit code: `FileIo` (11) if the redo also fails; `Ok` (0) if the
   phase-2 redo succeeds.

**Concurrency hazard:** The rayon `collect` short-circuit is
unordered - when multiple chunks in a batch have mismatched digests,
only one error is surfaced (the first one rayon finds). The other
mismatched chunks may have their verify run but the result is
discarded. This is acceptable because the receiver retries the entire
file in phase 2. However, the test must verify that the serial write
loop after `collect` never runs when any error is returned.

### 3.6 Worker panic during verify

**Trigger:** A rayon worker panics inside `verify_chunk()`. This
should not happen under normal operation, but a bug in the checksum
strategy or a corrupted `ChecksumStrategy` vtable could cause it.

**Injection mechanism:** A test-only `ChecksumStrategy` implementation
that panics when `compute()` is called for a specific chunk
(identified by data content or sequence number).

**Expected behavior:**

1. Under `apply_one_chunk()`: `rayon::join` catches the panic and
   propagates it. The calling thread observes the panic as a resume
   from `rayon::join`.
2. Under `apply_batch_parallel()`: `into_par_iter().collect()` catches
   the first panic and propagates it after all workers have completed
   or panicked. The calling thread receives the panic.
3. The panicking worker's `DecrementGuard` fires on unwind (it
   implements `Drop`), decrementing the in-flight counter. This is
   the FFB-W.c invariant: the barrier must not deadlock when a worker
   panics.
4. `drain_inflight()` completes because the panic decremented the
   counter.
5. If the panic poisoned the per-file `Mutex<FileSlot>` (because the
   worker held the lock during the panic), subsequent `lock_slot()`
   calls return `ParallelApplyError::SlotPoisoned`. The receiver maps
   this to `FileIo` (11).
6. Temp files are cleaned up via the receiver's normal shutdown path.
7. Exit code: `FileIo` (11) or `Crashed` (15) depending on whether
   the panic is caught at the receiver level.

**Concurrency hazard:** The `DecrementGuard` must decrement even on
panic unwind. Rust's drop-on-unwind guarantees this as long as the
guard type is not `#[may_dangle]` and the drop body does not itself
panic. The `BarrierState::decrement_inflight()` implementation at
`crates/engine/src/concurrent_delta/parallel_apply/slot_barrier.rs:202`
uses `lock().expect()` inside drop - if the inflight mutex is itself
poisoned, this panics during unwind, which aborts the process. The
test must verify that a single worker panic does not cascade into a
process abort.

### 3.7 Reorder buffer overflow (stalled file blocks pipeline)

**Trigger:** A file's chunks arrive heavily out of order, filling the
per-file `ReorderBuffer` to its capacity
(`DEFAULT_PER_FILE_REORDER_CAPACITY = 64`). The next chunk for that
file cannot be inserted. Meanwhile, other files' chunks are flowing
normally.

**Injection mechanism:** Submit chunks for a single file with sequence
numbers `[63, 62, ..., 1, 0]` (reverse order) into an applier with
`per_file_reorder_capacity = 32`. The reorder buffer fills at capacity
before sequence 0 arrives.

**Expected behavior:**

1. `FileSlot::ingest()` calls `reorder.insert()` which returns an
   error when the buffer is full.
2. The error propagates as
   `"parallel apply reorder full: ..."` through `apply_one_chunk()`
   or `apply_batch_parallel()`.
3. The receiver treats this as a per-file failure. The file may be
   retried in phase 2 or reported as failed.
4. Other files are not affected.
5. Exit code: `FileIo` (11).

**Concurrency hazard:** Under `apply_batch_parallel()`, the serial
write loop processes verified chunks sequentially. If chunk N for file
F fails on reorder insert, chunks N+1, N+2, ... for file F that are
already verified will also attempt insert and fail. The error from
chunk N should cause the receiver to stop submitting further chunks
for file F, but the batch may contain pre-verified chunks for F that
were submitted before the error was known. The write loop must handle
repeated failures gracefully without corrupting other files.

### 3.8 DeltaConsumer background thread crash

**Trigger:** The `DeltaConsumer`'s delta-drain or delta-reorder
background thread panics or encounters a fatal error. The
`ParallelDeltaPipeline`'s `submit_work()` or `poll_result()` then
operates on a dead pipeline.

**Injection mechanism:** A test-only `DeltaWork` processor that panics
after processing N items inside the rayon `scope`. The delta-drain
thread catches the panic from `rayon::scope`; the stream channel
closes; the delta-reorder thread exits.

**Expected behavior:**

1. `submit_work()` returns an error when the `WorkQueueSender::send()`
   fails because the consumer thread has terminated and the channel is
   closed: `"parallel pipeline consumer thread has shut down"`.
2. `poll_result()` returns `None` once the internal `mpsc` channel is
   drained and closed.
3. `flush()` collects whatever results were delivered before the crash
   and returns them.
4. The `DeltaConsumer::join()` call propagates the panic payload.
5. Exit code: depends on what the panic caused - typically `FileIo`
   (11) or `StreamIo` (12).

**Concurrency hazard:** The pipeline has two background threads
(delta-drain and delta-reorder). A crash in delta-drain closes the
stream channel, which causes delta-reorder to exit its loop and close
the result channel. This cascading shutdown must not deadlock. The
`crossbeam_channel` bounded channel between them provides the
backpressure boundary; when the sender drops, `recv()` returns `Err`.

### 3.9 Spill-to-disk failure in reorder buffer

**Trigger:** The `SpillableReorderBuffer` encounters ENOSPC or a
missing temp directory when spilling overflow items to disk. The
`run_spillable_loop` at
`crates/engine/src/concurrent_delta/consumer/loops.rs:82` handles this.

**Injection mechanism:** Configure the spillable reorder buffer with a
temp directory on a tiny tmpfs (or a test-only filesystem shim that
returns ENOSPC after N bytes). Fill the reorder buffer past its
in-memory threshold.

**Expected behavior:**

1. The spill write returns `SpillError::Io(...)`.
2. `run_spillable_loop` maps this to
   `DeltaResult::failed(ndx, "spill write failed: ...")` and sends it
   through the result channel.
3. The receiver sees the failed `DeltaResult` and maps it to
   `ExitCode::FileIo` (11).
4. The reorder thread exits after sending the failure, closing the
   result channel.
5. Remaining items in the pipeline are lost (acceptable - the transfer
   is aborting).
6. Exit code: `FileIo` (11).

**Concurrency hazard:** The spill failure occurs on the delta-reorder
thread. Workers on the delta-drain thread may still be computing
results and sending them into the stream channel. The stream channel
is bounded, so workers will block if the reorder thread has exited.
When the delta-drain thread's `rayon::scope` completes, the stream
sender drops, and workers unblock with their results lost. This is
acceptable for an aborting transfer.

### 3.10 Concurrent errors across multiple files

**Trigger:** Multiple files encounter different errors simultaneously:
file A gets a checksum mismatch, file B hits ENOSPC, file C's sender
data is truncated.

**Injection mechanism:** Register three files in `ParallelDeltaApplier`.
Inject per-file error conditions via test-only `Write` implementations
and deliberate digest mismatches. Submit interleaved chunks for all
three files in a single `apply_batch_parallel()` call.

**Expected behavior:**

1. Each file's error is independent. The checksum mismatch for file A
   does not prevent the ENOSPC detection for file B.
2. Under `apply_batch_parallel()`: the verify step catches file A's
   mismatch. If file A's bad chunk is the first error rayon finds, the
   ENOSPC for file B is not observed until the serial write loop (which
   does not run because verify failed). The receiver must re-attempt
   the remaining files or report them as failed.
3. If verify succeeds for all chunks but the write loop hits ENOSPC on
   file B, file A's already-written chunks and file C's already-written
   chunks remain intact in their respective `FileSlot` reorder buffers
   or destination writers.
4. The "worst exit code wins" rule applies: `FileIo` (11) takes
   precedence over `PartialTransfer` (23). Upstream rsync uses the
   highest-severity code.
5. All temp files for all three files are cleaned up.

**Concurrency hazard:** The per-file `Mutex<FileSlot>` ensures file A's
error does not corrupt file B's writer. The `DashMap` shard isolation
ensures looking up file B's slot is not blocked by file A's error
handling. The test must verify that `drain_inflight()` completes even
when multiple slots have errors or poisoned mutexes.

## 4. Test implementation strategy

### 4.1 Unit tests (per-scenario, in engine crate)

Location: `crates/engine/src/concurrent_delta/parallel_apply/` - new
test module `error_paths.rs` (or extend the existing `tests` block in
`mod.rs`).

Each scenario from Section 3 maps to one or more `#[test]` functions
that:

1. Construct a `ParallelDeltaApplier` with the appropriate concurrency
   and strategy configuration.
2. Register files with test-only `Write` implementations (e.g.,
   `FailAfterNBytesWriter`, `PanicOnWriteWriter`).
3. Submit chunks that trigger the error condition.
4. Assert the error type and message content.
5. Call `drain_inflight()` and assert it returns without deadlock
   (bounded by a test timeout).
6. Inspect the `Write` sink to verify no corrupt bytes were written.
7. Assert `bytes_written()` reflects only successfully committed data.

### 4.2 Integration tests (sequential-parallel parity)

Location: `crates/transfer/tests/` or
`crates/engine/tests/parallel_error_parity.rs`.

For each error scenario, run the same workload through both
`SequentialDeltaPipeline` and `ParallelDeltaPipeline`. Collect:

- Exit code (or the error type that maps to it).
- Set of successfully committed files.
- Set of temp files remaining (must be empty for both).
- Error message content (modulo timing/thread-id differences).

Assert parity between the two paths.

### 4.3 Interop tests (against upstream rsync)

Location: extend `tools/ci/run_interop.sh` scenarios.

Two scenarios exercise error paths against upstream rsync:

- **vanished-file:** Start a transfer of a large tree. Remove a file
  on the sender side mid-transfer. Verify both oc-rsync and upstream
  rsync exit with code 24 (`Vanished`).
- **disk-full:** Mount a tiny tmpfs as the destination. Transfer a
  dataset larger than the tmpfs. Verify both exit with code 11
  (`FileIo`) or 23 (`PartialTransfer`).

These interop tests run under both the sequential default and the
`--features parallel-receive-delta` build.

## 5. Temp-file cleanup verification

The receiver's temp-file naming convention follows upstream rsync:
`.file.XXXXXX` (six random characters) in the destination directory or
the `--temp-dir` path.

Post-error verification:

```
find "$DEST" -name '.*.??????' -print
```

Must return zero results after every error scenario completes. The test
harness wraps this check in a helper that:

1. Records the destination directory at transfer start.
2. After the transfer returns (success or error), scans for orphaned
   temp files matching the pattern.
3. Asserts the count is zero.
4. If any are found, logs their names and sizes for diagnostic value.

For the parallel path specifically, temp files may be created by multiple
workers concurrently. The cleanup path must handle the case where a
worker created a temp file but the error occurred before `finish_file()`
ran, meaning the temp file was never renamed to its final name.

## 6. Worker drain timeout

All tests in Section 4 must enforce a drain timeout to detect deadlocks:

```rust
let deadline = Instant::now() + Duration::from_secs(10);
let drain_result = std::thread::spawn({
    let applier = Arc::clone(&applier);
    move || applier.drain_inflight()
});
// ... assert drain completes before deadline ...
```

A drain that exceeds 10 seconds indicates a deadlock in the barrier
mechanism. The test must fail with a diagnostic message identifying
which NDX has a non-zero in-flight count.

The 10-second bound is generous for CI; the actual drain time should be
sub-millisecond for the unit-test workload sizes. The bound exists
solely to prevent CI from hanging indefinitely on a regression.

## 7. Error message format verification

Each error scenario must verify the error message content against the
pattern:

```
<description> (code <N>) at <path>:<line> [receiver=<version>]
```

Test assertions check:

- The `(code N)` substring matches the expected `ExitCode` value.
- The `[receiver=<version>]` trailer is present.
- The `<path>` is a repository-relative path, not an absolute path.
- For `ParallelApplyError` variants: the typed fields (`ndx`,
  `strong_count`, `chunk_sequence`, `algorithm`) appear in the message.

## 8. Comparison with sequential path

The sequential path (`SequentialDeltaPipeline`) processes errors inline:
the `dispatch()` call in `submit_work()` runs synchronously, and any
error is immediately visible to the caller. There is no reorder buffer,
no background thread, and no worker pool.

The parallel path adds three layers where errors can occur:

| Layer | Component | Error surface |
|---|---|---|
| Verify | `verify_chunk()` on rayon worker | `ParallelApplyError::ChecksumMismatch` |
| Write | `FileSlot::ingest()` under per-file mutex | `io::Error` from writer |
| Reorder | `ReorderBuffer::insert()` | capacity overflow error |
| Pipeline | `DeltaConsumer` background threads | channel closure, panic propagation |

For each layer, the parallel-path test must show the same observable
outcome as the sequential path. Differences that are acceptable:

- Timing: the parallel path may detect errors in a different order than
  the sequential path. The test compares final state, not event order.
- Error messages: thread identifiers, timing info, and batch sizes may
  differ. The test strips these before comparison.
- Partial progress: the parallel path may have committed more files
  before detecting the error (because work was dispatched ahead). This
  is acceptable as long as committed files are correct (INV-3).

Differences that are NOT acceptable:

- Different exit code for the same failure.
- Orphaned temp files in one path but not the other.
- Corrupted committed files in one path but not the other.
- Deadlock (hang) in one path when the other returns promptly.

## 9. Implementation sequence

| Step | Deliverable | Depends on |
|---|---|---|
| PIP-10.e.1 | Test-only error-injection `Write` adapters (`FailAfterNBytesWriter`, `DisconnectingTokenReader`) | - |
| PIP-10.e.2 | Unit tests for scenarios 3.1 - 3.5 in `parallel_apply` | PIP-10.e.1 |
| PIP-10.e.3 | Unit tests for scenarios 3.6 - 3.7 (panic, reorder overflow) | PIP-10.e.1 |
| PIP-10.e.4 | Unit tests for scenarios 3.8 - 3.10 (consumer crash, spill, concurrent) | PIP-10.e.1 |
| PIP-10.e.5 | Sequential-parallel parity integration tests | PIP-10.e.2, PIP-10.e.3, PIP-10.e.4 |
| PIP-10.e.6 | Interop error-path scenarios (vanished-file, disk-full) | PIP-9.b complete |
| PIP-10.e.7 | Temp-file cleanup assertion harness | PIP-10.e.2 |
| PIP-10.e.8 | Error message format assertions | PIP-10.e.2 |

Steps PIP-10.e.1 through PIP-10.e.4 can run in parallel once the
adapters land.

## 10. Acceptance criteria

PIP-10.e is complete when:

1. All 10 error scenarios (Sections 3.1 - 3.10) have passing tests.
2. Every test verifies all six invariants (INV-1 through INV-6).
3. The sequential-parallel parity tests (PIP-10.e.5) demonstrate
   identical exit codes and temp-file states for every scenario.
4. The interop error-path tests (PIP-10.e.6) pass against upstream
   rsync 3.4.1 and 3.4.2.
5. No test exceeds the 10-second drain timeout.
6. CI green on all platforms (Linux, macOS, Windows).

## 11. Cross-references

- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` -
  `ParallelDeltaApplier`, `DeltaChunk`, `ParallelApplyError`.
- `crates/engine/src/concurrent_delta/parallel_apply/drain.rs` -
  `finish_file`, `flush_workers`.
- `crates/engine/src/concurrent_delta/parallel_apply/batch.rs` -
  `apply_batch_parallel`.
- `crates/engine/src/concurrent_delta/parallel_apply/slot_barrier.rs` -
  `SlotBarrier`, `BarrierState`, `SlotData`, `SlotEntry`.
- `crates/engine/src/concurrent_delta/parallel_apply/decrement_guard.rs` -
  `DecrementGuard` RAII drop semantics.
- `crates/engine/src/concurrent_delta/consumer/mod.rs` - `DeltaConsumer`.
- `crates/engine/src/concurrent_delta/consumer/loops.rs` -
  `run_bare_loop`, `run_spillable_loop`.
- `crates/transfer/src/delta_pipeline/parallel.rs` -
  `ParallelDeltaPipeline`.
- `crates/transfer/src/delta_pipeline/sequential.rs` -
  `SequentialDeltaPipeline`.
- `crates/core/src/exit_code/codes.rs` - `ExitCode` variants and
  `from_io_error()` mapping.
- `docs/design/ffb-1-applier-barrier-api.md` - barrier API design.
- `docs/design/pip-7-parallel-receive-delta-receiver-corruption-2026-05-22.md` -
  prior corruption analysis.
- `docs/design/pip-9-parallel-receive-wireup.md` - production wire-up
  plan.
- `docs/design/pip-9-f-1-bake-criterion.md` - default-on flip gate.
- Upstream: `receiver.c:recv_files()`, `errcode.h`.
