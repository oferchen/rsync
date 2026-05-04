# ReorderBuffer head-of-line blocking semantics

Tracking issue: oc-rsync task #1883. Branch: `docs/reorder-buffer-hol-1883`.

## Scope

Document the head-of-line (HoL) blocking behaviour of the concurrent delta
pipeline's reorder buffer, contrast it against upstream rsync 3.4.1's
strictly-sequential per-file pipeline, and capture the workloads where the
behaviour is observable. The audit covers the two reorder-buffer
implementations that exist today, the consumer thread that owns the
`engine`-side buffer, the bounded work queue that supplies it, and the relevant
upstream code paths.

Source files inspected (all paths repository-relative):

- `crates/engine/src/concurrent_delta/reorder.rs` (ring-buffer `ReorderBuffer`,
  capacity bound, `force_insert` deadlock break, `finish` gap detection).
- `crates/engine/src/concurrent_delta/consumer.rs` (`DeltaConsumer`, the two
  background threads `delta-drain` and `delta-reorder` that drive the buffer).
- `crates/engine/src/concurrent_delta/work_queue/bounded.rs` and
  `crates/engine/src/concurrent_delta/work_queue/capacity.rs` (bounded work
  queue, default capacity multiplier, adaptive depth heuristic).
- `crates/engine/src/concurrent_delta/mod.rs` (pipeline overview and rayon
  ordering audit).
- `crates/engine/src/concurrent_delta/adaptive.rs`
  (`AdaptiveCapacityPolicy`, grow/shrink counters).
- `crates/transfer/src/reorder_buffer.rs` (`BoundedReorderBuffer`, the
  alternative `BTreeMap`-backed sliding-window variant exposed at the
  `transfer` crate boundary).
- `crates/transfer/src/delta_pipeline.rs` (`SequentialDeltaPipeline`,
  `ParallelDeltaPipeline`, `ThresholdDeltaPipeline`).
- Upstream rsync 3.4.1 source under `target/interop/upstream-src/rsync-3.4.1/`
  (`receiver.c:recv_files`, `receiver.c:handle_delayed_updates`,
  `generator.c:generate_files`, `match.c:match_sums`).

## TL;DR

The concurrent delta pipeline computes per-file deltas on a rayon worker pool
and then re-serialises results into wire order before they reach
post-processing (checksum verification, temp-file commit, metadata
application). Re-serialisation is performed by `ReorderBuffer`
(`crates/engine/src/concurrent_delta/reorder.rs:65`), a fixed-capacity ring
buffer that yields items strictly in `next_expected` order. When a slow file
holds the head slot, every completed successor that has already arrived stays
buffered until the head is filled. Once the buffer reaches its capacity bound,
the upstream `delta-reorder` thread either drains whatever has accumulated and
keeps inserting, or - when it cannot drain because `next_expected` itself is
the missing item - calls `ReorderBuffer::force_insert`
(`crates/engine/src/concurrent_delta/reorder.rs:334`) to grow the ring and
break the deadlock. There is no spill-to-disk and no bypass for workloads that
do not need ordered delivery.

This is by design: oc-rsync's wire protocol must emit results in monotonic NDX
order to stay compatible with upstream's sequential `recv_files` loop
(`target/interop/upstream-src/rsync-3.4.1/receiver.c:522`). The trade-off is
that one straggler can stall up to `capacity - 1` already-completed successors
in memory, blocking their disk commit until the straggler finishes. Upstream
rsync has no such failure mode because it never starts file `N+1` before file
`N` is fully committed.

The HoL window is bounded - peak memory is `O(capacity * average_result_size)`,
and `capacity` defaults to `2 * rayon::current_num_threads()` for the parallel
pipeline (`crates/transfer/src/delta_pipeline.rs:210`). The behaviour is
visible in two regimes: long-tailed file size distributions (a few large files
mixed with many small ones) and `--delay-updates` runs where every committed
file is rebound at phase boundary so any HoL stall is observed by all files in
the batch.

## Upstream evidence

Upstream rsync 3.4.1 has no reorder buffer. `receiver.c:recv_files` is a tight
single-threaded loop that reads `(ndx, iflags, ...)` tuples from the wire,
applies the delta against the basis, commits the temp file, and writes the
ack. Files are processed strictly in NDX order:

- `receiver.c:522` - `int recv_files(int f_in, int f_out, char *local_name)`.
- `receiver.c:554-560` - `while (1) { ... read_ndx_and_attrs(...) ... }`. Each
  iteration reads exactly one NDX from the wire and processes it to
  completion before reading the next.
- `match.c:362` - `match_sums(int f, struct sum_struct *s, struct map_struct
  *buf, OFF_T len)` is invoked synchronously from the recv loop; no work
  queue, no concurrency.
- `generator.c:2226` - `generate_files(int f_out, const char *local_name)` on
  the sender side is a matching single-threaded producer.

Two-phase delivery for `--delay-updates` is the only place upstream
intentionally stalls a completed file: when the option is set, the receiver
writes finished files into the partial dir and bitbags their NDX
(`receiver.c:546-547` and `bitbag_set_bit` calls in `recv_files`). Phase
transition (`receiver.c:584-585`) calls
`handle_delayed_updates` (`receiver.c:422`), which iterates the bitbag and
performs the deferred renames. The delay is intentional and observable in wire
output, not a side-effect of pipelining.

Because upstream has no parallel dispatch, it has no reorder buffer, no
sequence numbering, and no HoL blocking pathology. Any oc-rsync extension must
preserve the externally-observable property that the receiver writes file `N`
before it writes file `N+1` (as seen by post-processing, the wire ack stream,
and the on-disk state at any point during the transfer).

## 1. Where ReorderBuffer fits in the pipeline

oc-rsync ships two reorder-buffer implementations. They are not redundant:
they live at different layers of the pipeline and have different ownership.

### 1.1 `engine::concurrent_delta::ReorderBuffer` (ring buffer, primary path)

This is the production implementation used by the parallel delta pipeline.

- Storage: `Box<[Option<T>]>`, indexed by `(sequence - next_expected) + head`
  modulo capacity (`crates/engine/src/concurrent_delta/reorder.rs:138-151`).
  O(1) insert and O(1) drain.
- Owner: `DeltaConsumer` background thread named `delta-reorder`
  (`crates/engine/src/concurrent_delta/consumer.rs:147-188`).
- Producer: rayon workers spawned by `WorkQueueReceiver::drain_parallel_into`
  inside the `delta-drain` thread (`consumer.rs:138-143`). Workers complete in
  arbitrary order and forward `DeltaResult` items through a bounded
  `crossbeam_channel` to the reorder thread.
- Consumer: `DeltaConsumer::iter()` and `try_recv` deliver in-order results to
  `ParallelDeltaPipeline::poll_result`
  (`crates/transfer/src/delta_pipeline.rs:236-242`), which feeds the
  receiver's per-file post-processing (checksum verify, temp commit, metadata
  apply).

The ring is sized via `DeltaConsumer::spawn(rx, reorder_capacity)`
(`consumer.rs:129`). `ParallelDeltaPipeline::new(worker_count)` passes
`worker_count.saturating_mul(2).max(2)` as both the work-queue capacity and
the reorder capacity (`crates/transfer/src/delta_pipeline.rs:209-212`), so the
default is `2 * rayon::current_num_threads()` slots. The same multiplier is
defined as `CAPACITY_MULTIPLIER = 2` in
`crates/engine/src/concurrent_delta/work_queue/capacity.rs:8`.

A `force_insert` path
(`crates/engine/src/concurrent_delta/reorder.rs:334-360`) grows the ring when
a sequence beyond the current capacity arrives and the buffer would otherwise
deadlock - see Section 2.3.

`ReorderBuffer::finish` (`reorder.rs:408-425`) panics if any items remain
buffered when the producer side has closed, which detects upstream sequence
gaps (a worker dropping a `DeltaWork` item without producing a `DeltaResult`).

### 1.2 `transfer::reorder_buffer::BoundedReorderBuffer` (sliding-window, alt)

A second implementation exists at the `transfer` crate boundary:
`BoundedReorderBuffer<T>` in `crates/transfer/src/reorder_buffer.rs:57`. It is
backed by a `BTreeMap<u64, T>` (O(log n) insert) with an explicit acceptance
window `[next_expected, next_expected + window_size)`. Insertions outside the
window return `BackpressureError`
(`crates/transfer/src/reorder_buffer.rs:79-86`) so the producer can throttle
before it overruns the buffer.

`DEFAULT_WINDOW_SIZE` is `64`
(`crates/transfer/src/reorder_buffer.rs:26`).

The two implementations exist for separate use cases: the engine ring buffer
sits inside the consumer thread that owns it (single producer per consumer
for the reorder step, even though the upstream queue is SPMC), while
`BoundedReorderBuffer` exposes a backpressure-style API that pushes the
"slow down" decision to a caller that holds the producer side. Both have the
same HoL semantics because both deliver strictly in `next_expected` order.

### 1.3 The bounded work queue feeding the reorder buffer

Upstream of the reorder buffer is a bounded `crossbeam_channel`
(`crates/engine/src/concurrent_delta/work_queue/bounded.rs:48-60`) that the
receiver / generator thread fills with `DeltaWork` items and that rayon
workers drain. Capacity policy lives in
`crates/engine/src/concurrent_delta/work_queue/capacity.rs`:

- `default_capacity()` - `2 * rayon::current_num_threads()`
  (`capacity.rs:36-38`).
- `adaptive_queue_depth(avg_file_size)` - `8x` for files under 64 KiB, `2x`
  for files over 1 MiB, `4x` otherwise (`capacity.rs:66-76`).

The work queue is SPMC by design: the rsync wire protocol delivers file
entries on a single multiplexed stream, so there is exactly one thread reading
from the wire and producing `DeltaWork` items
(`crates/engine/src/concurrent_delta/work_queue/mod.rs:14-22`).
`WorkQueueSender` is `Send` but not `Clone`, enforcing this at compile time
(`bounded.rs:48`).

## 2. Current head-of-line blocking behaviour

The reorder buffer is the only point in the pipeline that can stall completed
work behind incomplete work. Three behaviours combine to produce the observed
semantics.

### 2.1 Strict in-order delivery

`ReorderBuffer::next_in_order` returns `Some(T)` only when the slot at `head`
is occupied (`reorder.rs:262-274`). If the head is empty - i.e. the worker
processing sequence `next_expected` has not yet finished - the call returns
`None` regardless of how many later sequences have already arrived. The
consumer thread therefore cannot forward result `N+1` until result `N` lands,
even if `N+1` finished computation seconds earlier.

`drain_ready` (`reorder.rs:281-283`) is an iterator that calls
`next_in_order` repeatedly. It yields whatever contiguous run starts at the
current head, then stops as soon as the next slot is empty. The consumer
thread invokes it after every successful insert
(`consumer.rs:171-175`) and once more after the input stream closes
(`consumer.rs:179-183`).

### 2.2 Bounded window means peak memory is `O(capacity)`

Because the ring is fixed-size, HoL stalls cannot grow unboundedly. With
`capacity = 2 * num_threads` and (say) 16 threads, at most 32 completed
results can be queued behind a stalled head. Memory cost is bounded but so
is throughput recovery: once the window fills, no more workers can hand off
their results to the reorder thread.

### 2.3 What happens when the window fills

The `delta-reorder` loop (`consumer.rs:151-176`) handles the full-buffer
case explicitly:

```rust
while reorder.insert(result.sequence(), result.clone()).is_err() {
    let mut drained_any = false;
    for ready in reorder.drain_ready() {
        drained_any = true;
        if result_tx.send(ready).is_err() {
            return;
        }
    }
    if !drained_any {
        // Buffer full but next_expected is not buffered.
        // Force insert to break the deadlock.
        reorder.force_insert(result.sequence(), result.clone());
        break;
    }
}
```

There are two cases:

1. The buffer is full but the head slot is occupied. `drain_ready` yields the
   current contiguous run, freeing slots; the next `insert` succeeds.
2. The buffer is full and the head slot is *not* occupied (the slow file is
   still in flight, and `capacity - 1` successors have piled up).
   `drain_ready` yields nothing, so `force_insert`
   (`reorder.rs:334-360`) grows the ring to fit the new item. This is a
   correctness escape hatch, not a performance feature: it converts a
   capacity-bound buffer into an unbounded one for the duration of the stall.

The growth path matters for the HoL discussion because it is the only thing
that prevents a hard deadlock between the rayon workers (which block on the
bounded `crossbeam_channel` between drain and reorder) and the reorder thread
(which would otherwise be unable to accept the head item if it arrived after
the window filled). The cost is that the buffer's memory bound becomes
soft - a sufficiently slow head file can grow the ring to hold every
successor that has already completed.

The producer-side `BoundedReorderBuffer` returns `BackpressureError`
(`crates/transfer/src/reorder_buffer.rs:129-142`) instead of growing. A
caller that uses that variant must drain or wait before re-submitting; there
is no force-insert escape hatch.

### 2.4 Optional adaptive capacity scaling

`AdaptiveCapacityPolicy`
(`crates/engine/src/concurrent_delta/adaptive.rs:22`) is an opt-in policy
attached via `ReorderBuffer::with_adaptive_policy`
(`crates/engine/src/concurrent_delta/reorder.rs:131-136`). It grows the ring
under sustained pressure and shrinks back toward `policy.min` once the gap
closes. Grow/shrink event counts are exposed through `ReorderBuffer::stats`
(`reorder.rs:312-325`). The default `DeltaConsumer::spawn` path does *not*
attach an adaptive policy - it constructs a fixed-capacity ring with
`ReorderBuffer::new(reorder_capacity)` (`consumer.rs:149`).

Adaptive scaling reduces the probability of force-insert growth by anticipating
pressure, but it does not eliminate HoL blocking - successors are still
held until the head completes. It only changes how much memory is committed
in the window during the stall.

## 3. Trade-offs vs upstream's strictly-sequential pipeline

The two designs differ on what they optimise for and where they pay the cost.

### 3.1 Throughput

- **Upstream**: one file at a time. Throughput equals
  `1 / sum(per_file_time)`. CPU-bound delta computation cannot overlap with
  disk-bound commit; both serialise on the receiver thread.
- **oc-rsync parallel pipeline**: up to `num_threads` files in flight
  simultaneously, with delta computation overlapped with commit through the
  reorder buffer. Throughput is bounded by
  `max(network_input_rate, reorder_window / slowest_in_flight_file)`.

Under uniform file-size distributions oc-rsync wins decisively: parallel
delta computation amortises across the rayon pool. Under skewed
distributions the gap narrows because the reorder window can stall on the
tail.

### 3.2 Latency to first commit

- **Upstream**: file `0` commits as soon as it finishes - no buffering.
- **oc-rsync**: file `0` is also released as soon as it lands in the head
  slot, but only after `delta-reorder` services it. There is one extra hop
  through the bounded `crossbeam_channel` between drain and reorder, plus
  the mpsc channel to `poll_result`. In practice this is sub-millisecond, but
  it is non-zero and must be accounted for in tail-latency benchmarks.

### 3.3 Memory

- **Upstream**: one in-flight delta result. Constant memory in file count.
- **oc-rsync**: up to `capacity` in-flight results, each holding the
  `DeltaResult` (NDX, byte counts, redo status) plus any borrowed data the
  result holds. With the default capacity of `2 * num_threads`, peak memory
  is bounded but proportional to the worker pool size.

When `force_insert` triggers (Section 2.3), the bound is lost for the
duration of the stall. The upper bound becomes "every completed successor
since the head was last drained", which is `total_files - 1` in the
worst case (one slow head file at sequence 0, every other file completes
first). This is the spill-to-tempfile scenario tracked in #1884.

### 3.4 Failure mode visibility

- **Upstream**: a single slow file slows the whole transfer linearly. There
  is no surprise; the user sees a steady-but-slow progress rate.
- **oc-rsync**: while the head file is in flight, `num_threads - 1` workers
  may complete and idle (they have nothing to push because the bounded
  channel back-pressures them). The progress meter on the receiver side
  pauses, then jumps when the head finally lands and the entire window
  drains in one burst. The stall pattern is "stop, then catch up", not
  "smooth slowdown".

This is a UX difference, not a correctness difference. Stall-duration
metrics (#1885) would expose the pattern in observability data.

### 3.5 Correctness and protocol fidelity

Both pipelines deliver post-processing input in NDX order. The reorder buffer
is the gate that guarantees this for the parallel path
(`crates/engine/src/concurrent_delta/mod.rs:62-71`). The wider rayon
parallelism audit
(`crates/engine/src/concurrent_delta/mod.rs:53-166`) classifies every
`par_iter` site in the codebase as SAFE, GUARDED, or RISK; the
concurrent-delta path is GUARDED by the reorder buffer.

There is no externally-visible deviation from upstream: the receiver's
`poll_result` stream sees the same NDX sequence in the same order, the wire
ack stream is unchanged, and the on-disk state after each commit is the same
as upstream's after the same NDX. The only observable side effect is the
"stop-then-burst" UX pattern in Section 3.4.

## 4. When HoL blocking is observable in practice

The pathology requires (a) a parallel pipeline, (b) at least one file whose
delta-compute time is significantly larger than the rest, and (c) enough
follow-on files to fill the window. Common workloads:

### 4.1 Mixed large-and-small file transfers

A directory with one multi-gigabyte log file plus thousands of small config
files. The large file is dispatched first (NDX 0), takes seconds to checksum
and delta, and during that interval every small file completes near-instantly
on other workers. The window fills with tiny `DeltaResult` items. The small
files cannot commit until the large file releases the head.

The mitigation tracked in #1884 (spill-to-tempfile for stalled successors)
would recycle the buffered results to disk when memory pressure becomes
unacceptable, freeing the in-memory window for further inserts while still
preserving in-order delivery on read-back.

### 4.2 `--delay-updates`

`--delay-updates` is implemented end-to-end (see config validation in
`crates/transfer/src/config/builder.rs` and the option type in
`crates/transfer/src/setup/types.rs`). The flag stages every successful
file in the partial dir and renames them all at the end of phase 1, mirroring
upstream `receiver.c:handle_delayed_updates`
(`target/interop/upstream-src/rsync-3.4.1/receiver.c:422-450`).

Because the delayed renames execute in a single pass at phase end, any HoL
stall during delta dispatch becomes visible in two places: the per-file
commit latency (during the stall) and the phase-end batch latency (when the
delayed renames execute). The user-visible effect is a longer "all or nothing"
window during which no files appear at their final paths.

The bypass-when-`--delay-updates`-is-off mitigation tracked in #1886 is the
inverse observation: when the user has not opted into batched commits, the
sequential pipeline already provides upstream-equivalent semantics, and the
parallel pipeline's HoL stalls are pure overhead. A bypass would route those
runs through `SequentialDeltaPipeline`
(`crates/transfer/src/delta_pipeline.rs:101-144`) regardless of file count.

### 4.3 Phase 2 redo

When a file fails phase 1 checksum verification it is redone in phase 2 with
a longer strong checksum (see `SHORT_SUM_LENGTH` vs `MAX_SUM_LENGTH` in
`crates/signature/src/block_size.rs`). Phase 2 redo files are dispatched into
the same parallel pipeline as phase 1 files, so the same HoL behaviour
applies. Redo workloads tend to be small and skewed (one or two files needed
a redo), so the practical impact is negligible.

### 4.4 Single-large-file transfer

Not affected. There is exactly one in-flight file, no successors to stall.
The threshold pipeline (`ThresholdDeltaPipeline`,
`crates/transfer/src/delta_pipeline.rs:286-317`,
`DEFAULT_PARALLEL_THRESHOLD = 64`) routes such transfers to
`SequentialDeltaPipeline` automatically.

### 4.5 Many-small-files transfer

If every file completes within a few microseconds on a balanced rayon pool,
the head slot drains as fast as workers can fill it. Window utilisation
stays low and the HoL stall window is too short to observe. The adaptive
queue depth (`adaptive_queue_depth(avg_file_size) = 8 * num_threads` for
files under 64 KiB, `crates/engine/src/concurrent_delta/work_queue/capacity.rs:66-75`)
deliberately sizes the window deeper for this regime to keep workers
saturated despite per-file syscall overhead.

## 5. Related work in flight

The following tracker entries are *not* merged code; they are open design
items in the issue tracker. This document describes current behaviour so that
the trade-offs are explicit when those proposals are evaluated.

- **#1884 - Bounded-memory spill-to-tempfile for stalled successors.**
  Proposes that when the window fills and `force_insert` would otherwise
  grow the ring without bound, completed successors are streamed to a
  per-transfer tempfile and read back in NDX order when the head finally
  drains. Preserves in-order delivery while restoring the hard memory bound.
  Open question: where does the tempfile live (partial dir? `TMPDIR`?), and
  how does it interact with `--inplace`/`--partial-dir`?
- **#1885 - Stall-duration metrics.** Proposes counters on the
  `delta-reorder` thread that record (a) total time spent with the head slot
  empty, (b) peak window occupancy, and (c) number of `force_insert` events.
  Exposed via the existing `ReorderStats` struct
  (`crates/engine/src/concurrent_delta/adaptive.rs:88-95`) so they show up
  alongside grow/shrink counts in observability output.
- **#1886 - Bypass parallel pipeline when `--delay-updates` is off.**
  Proposes that runs without `--delay-updates` (and any other commit-batching
  option) skip the parallel pipeline entirely, falling back to
  `SequentialDeltaPipeline`. The motivation is that without batched commits
  the parallel pipeline's HoL stalls have no compensating benefit on the
  observable side: each file commits as soon as its delta is ready, exactly
  like upstream. The bypass would change the threshold semantics in
  `ThresholdDeltaPipeline` to consider option flags, not just file count.

Each of those mitigations addresses a different axis: #1884 caps the memory
cost of a stall, #1885 makes stalls observable, and #1886 avoids the cost
when there is no benefit. They are independent and can land in any order.

## 6. Summary

The reorder buffer is the contract that lets oc-rsync run a parallel delta
pipeline while preserving upstream's NDX-ordered delivery invariant. Its
correctness story is solid: ring-based O(1) insert/drain, an explicit
deadlock-break path via `force_insert`, and a `finish` panic that catches
upstream sequence gaps. Its performance story has one rough edge: a slow
head file stalls every completed successor in the bounded window until the
head lands, and on stall the `force_insert` escape hatch trades the memory
bound for a guaranteed forward progress path.

Upstream rsync sidesteps the entire problem by never running file `N+1`
before file `N` is committed. That is the right design for a single-threaded
receiver. oc-rsync's parallel pipeline is the right design for multi-core
receivers but inherits this HoL pathology from any reordering protocol.

The mitigations tracked in #1884, #1885, and #1886 each address a different
axis (memory bound, observability, and avoidance respectively). None of them
are implemented today; this document describes the current behaviour they are
intended to improve.
