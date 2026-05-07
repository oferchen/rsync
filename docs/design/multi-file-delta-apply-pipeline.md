# Multi-file delta-apply pipeline with preserved wire ordering

Tracking: oc-rsync task #1079. This note specifies a receiver-side
multi-file delta-apply pipeline that overlaps per-file delta application
across in-flight files while preserving strict wire-NDX ordering for
acknowledgements and disk commits. No wire-protocol changes, no new
flags advertised on the wire, no on-disk artefacts. Tcpdump-replay
against an upstream peer must remain byte-identical with the feature on
or off.

## 1. Current sequential delta-apply flow

The receiver today applies one file at a time. The relevant call graph
(repository-relative paths):

- `crates/transfer/src/receiver/transfer/pipeline.rs` -
  `run_pipeline_loop_decoupled` is the outer reception loop. The window
  fill, request emission, and signature precomputation are pipelined,
  but per-response delta-apply runs synchronously inside
  `process_file_response_streaming`.
- `crates/transfer/src/delta_apply/mod.rs` - public surface of the
  applicator module: `DeltaApplicator`, `DeltaApplyConfig`,
  `DeltaApplyResult`, `apply_delta_stream`, `BasisWriterKind`.
- `crates/transfer/src/delta_apply/applicator.rs` -
  `apply_delta_stream(reader, applicator)` runs the per-file inner loop
  `while applicator.apply_token(reader)? {}`. It blocks on the wire
  reader, copies basis blocks, writes literal bytes, updates the strong
  checksum, and only returns after the file's delta stream is fully
  consumed. `BasisWriterKind` selects mmap vs `BufferedMap` for the
  basis-file mapping; the choice is per-applicator and does not leak
  cross-file state.
- `crates/transfer/src/delta_apply/checksum.rs` - `ChecksumVerifier`
  finalises the file digest before the temp file is renamed.
- `crates/transfer/src/delta_apply/sparse.rs` - sparse-write bookkeeping
  collapsed per file.

The dispatch layer is already pluggable via
`crates/transfer/src/delta_pipeline.rs` (the `ReceiverDeltaPipeline`
trait, with `SequentialDeltaPipeline`, `ParallelDeltaPipeline`, and the
`ThresholdDeltaPipeline` auto-selector at `DEFAULT_PARALLEL_THRESHOLD =
64`). What that trait already covers is *dispatch* (which file goes to
which worker) and *result reordering* (results from workers are
re-serialised before the next stage). What it does not cover is
overlapping the actual byte-level delta-apply for files N and N+1: a
single-threaded applicator still drains each file end-to-end before the
next file starts.

The engine-side complement
`crates/engine/src/concurrent_delta/` provides a bounded
`work_queue` (`work_queue/bounded.rs`, capacity multiplier in
`work_queue/capacity.rs:8` = 2 x rayon thread count), the engine-side
`ReorderBuffer` (`concurrent_delta/reorder.rs`), and the ordered
`DeltaConsumer` (`concurrent_delta/consumer.rs`) bridging parallel
dispatch back into in-order delivery. Section 3 reuses every piece.

## 2. ReorderBuffer integration

The bounded sliding-window reorder buffer at
`crates/transfer/src/reorder_buffer.rs` is the in-order-delivery
primitive. Key surface:

- `BoundedReorderBuffer<T>::new(window_size)` (`reorder_buffer.rs:106`)
  with the invariant that all buffered keys `k` satisfy `next_expected
  <= k < next_expected + window_size`.
- `insert(seq, item)` (`reorder_buffer.rs:129`) admits an item if `seq`
  falls inside the current window, returns `BackpressureError`
  otherwise (`reorder_buffer.rs:79`), and on admission drains the
  longest contiguous prefix starting at `next_expected`
  (`drain_consecutive`, `reorder_buffer.rs:149`).
- `next_expected`, `buffered_count`, `window_remaining`, `window_size`
  accessors at `reorder_buffer.rs:160-189` expose the metrics needed
  for stall observability and the bypass condition.

The reorder buffer guarantees three properties used in this design:

1. **Monotonic delivery.** Drained items are always emitted with seq
   numbers in strictly increasing order starting from
   `next_expected`. Once seq N has been delivered, no later
   `insert(N, _)` can deliver out of order; the duplicate path returns
   `Ok(Vec::new())` without disturbing state.
2. **Bounded memory.** The pending `BTreeMap` size never exceeds
   `window_size`, regardless of total transfer size. This is the
   memory-cost ceiling for slow-successor scenarios (section 5).
3. **Backpressure surface.** `BackpressureError` is the explicit
   signal back to producers that the window is saturated. Producers
   block (parking, no spinning) until the commit head advances.

The receiver-side `ParallelDeltaPipeline` already consumes
`BoundedReorderBuffer` for dispatch results. The multi-file apply
pipeline reuses the *same* primitive at a different granularity: rather
than buffering `DeltaResult` (dispatch-stage outcomes), it buffers
`AppliedFileResult` (post-apply outcomes carrying temp-file path,
`DeltaApplyResult` stats, and verifier outcome) keyed by the wire NDX
that the producer assigned. The window size is sized off
`work_queue::capacity` so the apply pipeline never holds more in-flight
state than the dispatch budget already permits.

## 3. Proposed pipeline

### 3.1 Stage shape

```
Wire reader (1 thread)             Apply pool (W workers)             Commit (1 thread)
+--------------------+              +-------------------+              +------------------+
| read NDX, file meta|              | pull (seq, meta)  |              | drain reorder    |
| stage delta blob   | --bounded--> | open basis        | --reorder--> | rename temp file |
| assign monotonic   |  work queue  | apply_delta_stream|  buffer (W)  | emit NDX ack     |
| seq = wire-arrival |  (W slots)   | verify checksum   |              | aggregate stats  |
| order              |              | push (seq, res)   |              | run delete fence |
+--------------------+              +-------------------+              +------------------+
```

W = effective in-flight window. Default 16. Hard upper bound is `2 *
rayon::current_num_threads()` for symmetry with the existing work-queue
capacity policy in `work_queue/capacity.rs:8`.

### 3.2 What is reused unchanged

- `apply_delta_stream` and `DeltaApplicator` are called per file
  exactly as today; no per-file state crosses workers.
- `BoundedReorderBuffer` provides the reorder + backpressure surface.
- `crates/engine/src/concurrent_delta/work_queue/bounded.rs` provides
  the producer-side bounded channel.
- The disk-commit thread spawned by `disk_commit::spawn_disk_thread`
  (referenced from `pipeline/receiver.rs:23`) is the natural drain
  target: it already serialises commits through a single thread.
- The `temp_guard.rs` RAII handle continues to own each in-flight temp
  file; cleanup on panic / drop is unchanged.

### 3.3 What changes

- A new `MultiFileApplyPool` type owns W workers and the reorder
  buffer. Proposed module placement:
  `crates/transfer/src/delta_pipeline/apply_pool.rs`. The pool is
  constructed and destructed at receiver-context scope; lifetime is
  one transfer.
- `process_file_response_streaming` splits into a producer half (read
  delta bytes into a per-slot blob, stamp wire NDX as the seq) and a
  consumer half (apply onto basis, verify, hand to commit) so workers
  can run the consumer half off the producer thread.
- The commit thread becomes a reorder-buffer drain consumer rather
  than a direct downstream of apply. It still issues exactly one
  `rename(2)` and one NDX ack at a time.

### 3.4 Wire-NDX-ordered commit

Every file in the transfer carries a unique NDX assigned in file-list
order on the sender. The producer reads file responses in wire-arrival
order, which equals NDX order for a single sender (the wire is a single
ordered byte stream, framed responses arrive in the order they were
sent). The producer assigns `seq = NDX` to each work item before
pushing to the work queue.

Workers may run in any order. Each worker computes its result, then
calls `reorder_buffer.insert(seq, result)`. The reorder buffer drains
results in `seq` order. The commit thread pulls drained items
sequentially. `rename(2)` and NDX ack are emitted per drained item, in
drain order, by a single thread. This composition restores wire-NDX
order at the commit boundary.

### 3.5 Backpressure flow

1. Producer stages file N+W's delta blob. If the blob exceeds the
   per-slot budget (proposed `MAX_STAGED_DELTA_BYTES = 4 MiB`), it
   spills to a pre-opened scratch temp file. This bounds heap growth
   on adversarial transfers (one tiny file followed by a multi-GB
   delta).
2. Producer tries to push `(seq, meta, staged_handle)` into the
   bounded work queue. Full queue blocks the producer (parking, no
   spinning).
3. Worker pops, applies, calls `reorder_buffer.insert(seq, result)`.
   On `BackpressureError`, the worker parks on a condvar tied to
   `next_expected` advancement and retries when the head moves.
4. Commit thread drains as soon as `next_expected` is satisfied,
   releasing one reorder slot, one work-queue slot, and one wire
   producer push per emitted commit. The cycle is closed-loop and
   self-pacing.

### 3.6 Handover to disk-commit

The disk-commit thread is the single sink for renames and the single
emitter of NDX acks. It receives `AppliedFileResult` items via an mpsc
channel from the reorder-buffer drainer. Per item it:

- Performs the `rename(2)` from temp to final path (or skips, under
  `--inplace` / failure / `--partial`).
- Emits the NDX ack frame (success or `IT_BASIS_TYPE_FOLLOWS` for
  redo) onto the writer.
- Aggregates stats into the per-transfer counters (literal bytes,
  matched bytes, file-size; `--stats` and the delete-stats wire frame
  read from these counters).
- Advances the `--delete-during` directory fence: deletion for
  directory D is gated on `commit_head >= max_seq(file in D)`. The
  fence implementation is unchanged from the current sequential
  receiver; it simply runs from the commit thread instead of inline
  with apply.

## 4. Wire ordering invariant proof

Claim: the externally observable sequence of NDX acks and disk
renames is identical to the sequential receiver's, for any worker
schedule.

Setup. Let `A = (a_0, a_1, ...)` be the wire-arrival sequence of file
responses at the producer. The sender emits NDX in file-list order
(upstream `receiver.c:recv_files()` invariant); the wire is a single
ordered byte stream, so `NDX(a_i) = i` for all i. The producer assigns
`seq(a_i) = i` before pushing into the work queue.

Lemma 1 (admission monotonicity). For any pair `i < j` admitted by the
reorder buffer, `i` is admitted before `j`. Proof: the producer is
single-threaded and pushes in arrival order; the work queue is FIFO
per producer; workers admit results in the order they finish, but
admission order does not affect *delivery* order, only *buffering*.

Lemma 2 (drain monotonicity). The drain sequence emitted by
`drain_consecutive` is strictly increasing and starts at the
post-state's `next_expected - drained.len()`. Proof: by inspection of
`reorder_buffer.rs:149` - the loop advances `next_expected` by 1 per
removed item starting at the entry-state value, and removes items
whose key equals the current `next_expected`. Output is therefore
`(next_expected_0, next_expected_0 + 1, ..., next_expected_0 + k - 1)`
for some k >= 0, strictly increasing.

Lemma 3 (drain progress). For every `i` admitted, there exists a
drain in which `i` appears. Proof: insert never deletes state; once
`i` is in `pending`, it remains until either delivery or buffer
destruction. The window invariant guarantees `i < next_expected +
window_size`. Suppose for contradiction that `i` is never drained:
then `next_expected` stalls below `i + 1` forever. But each
`insert(j, _)` with `j < i` either delivers `j` (advancing
`next_expected`) or is rejected with `BackpressureError`. The producer
pushes seq numbers in strict increasing order starting from 0, so all
seq values in `[0, i)` are eventually inserted and admitted (the
window advances enough to admit them, by induction). Once the last gap
in `[0, i)` is filled, the contiguous-drain loop advances
`next_expected` past `i`, contradicting the assumption.

Theorem (wire-NDX commit order). The commit thread emits items in seq
order, equivalently NDX order, equivalently file-list order. Proof:
the commit thread receives drained items via an mpsc channel from a
single drainer. The drainer drains in seq order (Lemma 2) and every
admitted item is eventually drained (Lemma 3). The mpsc channel
preserves send order, and the commit thread pops in receive order.
Therefore commits and acks are emitted in seq = NDX order.

Corollary (stats accounting). Per-file stats are aggregated on the
commit thread in commit order, which equals NDX order. The aggregate
counters reported via `--stats` and the delete-stats wire frame are
therefore identical to the sequential receiver's, regardless of
worker schedule.

## 5. Risks

### 5.1 Head-of-line blocking on slow file N

Worker for file N runs slow (large file, slow disk, expensive
checksum verification). Workers N+1..N+W finish quickly but cannot
commit. Reorder buffer fills. Producer blocks. Throughput drops to
N's apply rate.

This is a real and unavoidable cost of preserving NDX order. The
mitigations are bounding, not elimination:

- The window size W bounds buffered memory during stall. The bound is
  W slots times per-slot footprint (section 3.5). Nothing unbounded
  grows.
- The adaptive capacity policy in
  `crates/engine/src/concurrent_delta/adaptive.rs` already grows the
  reorder buffer under sustained pressure and shrinks it back; we
  reuse the same policy here.
- The `--delay-updates`-off bypass (#1886, sibling design at
  `docs/design/reorderbuffer-metrics-and-bypass.md`) lets the
  reorder buffer become a pass-through when wire-order
  re-serialisation is not load-bearing for atomicity, eliminating the
  HoL stall entirely in that mode.
- Worst-case stall wall-clock is `W * max(per-file apply time)`,
  which is the same upper bound a sequential receiver pays on every
  file. Pipelining never regresses throughput below the sequential
  baseline.

### 5.2 Memory growth on slow successors

If file N completes long before file N-1, file N's
`AppliedFileResult` holds its temp-file FD, basis mapping (released
at apply end), and stats while waiting in the reorder buffer. With W
slow successors, peak retained state is O(W) results.

Mitigations:

- The reorder buffer's window invariant caps pending entries at W;
  the BTreeMap cannot exceed that.
- Per-slot staging buffer is capped by `MAX_STAGED_DELTA_BYTES`;
  overflow spills to a scratch temp file (one FD, no heap).
- Basis-file mappings are released at apply completion (the
  `MapFile` drops at the end of `apply_delta_stream`); the result
  payload buffered in the reorder slot does not retain mmap pages.
- The aggregate at default W=16 is O(20 MiB) of resident state, which
  is acceptable on every supported target; a configurable budget knob
  shrinks W on memory-constrained systems.

### 5.3 Error propagation

A worker for file N may fail in three ways: a recoverable apply
error (checksum mismatch, basis-read error, malformed delta), an
unrecoverable error (panic, OOM, I/O error not representable as
`io::Result`), or a wire-side failure that drops the producer.

Recoverable apply failure for file N. The worker reports
`AppliedFileResult::Failed(error)` with `seq = N` to the reorder
buffer. The buffer admits it normally. The commit thread drains seq
N, sees the failure, leaves the temp file in place under `--partial`
or removes it under default policy, emits the failure / redo ack,
and continues with seq N+1. Files N+1..N+W in flight are NOT
cancelled - they each succeed or fail independently. This matches
upstream's behaviour that a single file's failure does not abort the
transfer. The delete-during fence still holds: deletion for a
directory is gated on commit-head, regardless of per-file
success/failure.

Unrecoverable worker failure. Panics propagate through the worker
thread's join handle to the receiver context, which unwinds the
transfer. `TempGuard` cleans the in-flight temp files on drop. The
work-queue close semantics ensure no orphaned worker survives the
unwind. Workers must drop their `TempGuard` on the unwind boundary;
this is achieved by RAII alone, no special handling. The abort path
is exercised by the property tests in #2049 (reorder buffer
drop-on-error).

Wire-side failure during pipelining. The producer's `read(2)`
returns `Err`. The producer drops the work-queue sender, which
closes the channel. Workers see the close, finish their current
file, and exit. The reorder-buffer drainer drains whatever is still
admissible; the commit thread emits acks for committed files only.
For in-flight files that have not yet been committed, the commit
thread emits no ack (the wire is dead) and removes the temp file via
`TempGuard`. The error propagates up to the receiver context. This
is functionally identical to today's serial path: the wire fails, no
further commits happen, partial state is cleaned.

The single new failure-cascade case introduced by pipelining is the
HoL-during-error scenario: file N fails but its result is still
behind a slow predecessor in the reorder buffer. The buffer drains
in seq order regardless of result kind, so the failure is reported
at the correct wire position; downstream successors are not
prematurely committed. This is covered by an additional property
test class (workers panic / return error in random orderings; the
reorder buffer must still emit the correct acks in seq order, and
the commit thread must not deadlock waiting on a never-arriving
result) tracked under #1079 follow-up.

## Cross-references

- `crates/transfer/src/delta_apply/mod.rs`,
  `crates/transfer/src/delta_apply/applicator.rs` - the unchanged
  applicator entry points workers call.
- `crates/transfer/src/delta_pipeline.rs` - the existing
  `ReceiverDeltaPipeline` trait the new pool plugs into.
- `crates/transfer/src/reorder_buffer.rs` - `BoundedReorderBuffer`,
  the in-order delivery primitive.
- `crates/engine/src/concurrent_delta/work_queue/bounded.rs`,
  `crates/engine/src/concurrent_delta/work_queue/capacity.rs` - the
  bounded work queue and capacity policy reused for backpressure.
- `crates/engine/src/concurrent_delta/reorder.rs`,
  `crates/engine/src/concurrent_delta/consumer.rs` - the engine-side
  ordered consumer pattern.
- `docs/design/reorderbuffer-metrics-and-bypass.md` - sibling
  observability and `--delay-updates`-off bypass design.
- `docs/architecture/reorder-buffer.md` - HoL semantics
  formalisation.
- Task #1079 - this design.
