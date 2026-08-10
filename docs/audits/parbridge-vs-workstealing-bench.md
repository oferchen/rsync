# par_bridge vs crossbeam work-stealing deque benchmark plan

Task: #1403. Branch: `docs/parbridge-workstealing-bench-1403`.

## Summary

This audit defines a benchmark plan that compares two parallel-iteration
strategies for feeding rayon worker threads from a single producer in
oc-rsync's concurrent delta pipeline:

- `rayon::iter::ParallelBridge::par_bridge()` over a `crossbeam_channel`
  receiver, the pattern previously used by `WorkQueueReceiver` before
  PR #1538.
- `crossbeam::deque::Worker` plus per-consumer `Stealer` handles wired
  through a hand-rolled scheduler, the canonical work-stealing primitive.

The benchmark is descriptive only - no execution is performed in this
task. The goal is to capture methodology, expected outcomes, and decision
criteria so a follow-up PR can produce reproducible numbers and either
reintroduce `par_bridge` (if it wins on small-file workloads) or commit
to the current `rayon::scope` + bounded-channel design (if not).

## Current state

The single-producer, multiple-consumer drain that feeds delta workers
lives at `crates/engine/src/concurrent_delta/work_queue/mod.rs`.
`WorkQueueReceiver::drain_parallel` (declared in
`crates/engine/src/concurrent_delta/work_queue/drain.rs`) uses
`rayon::scope` with per-task spawns and per-thread result buffers. The
queue itself is a bounded `crossbeam_channel` with capacity
`2 * rayon::current_num_threads()` (`work_queue/capacity.rs`); the sender
side is `Send` but not `Clone` to enforce SPMC at compile time
(`work_queue/mod.rs:11-21`).

`ParallelBridge::par_bridge()` was used in earlier revisions of
`work_queue.rs` to spawn one rayon task per channel item from a `for`
loop, but was removed in PR #1538 along with the previous file at
`crates/engine/src/pipeline/work_queue.rs`. The removal reasoning recorded
in the commit was lifetime friction: `par_bridge` borrows the iterator for
the duration of the parallel walk, which forced the queue's internal
state into `'static` storage and complicated the scoped-thread teardown
contract that the bounded queue uses today.

A complementary parallel iteration ladder exists in `crates/checksums/src/parallel/files.rs`
(rolling-checksum scans) and `crates/engine/src/walk/`
(directory traversal), both of which use `par_iter_mut` over owned
`Vec<_>` rather than bridging from a channel. Neither of those sites
shows the SPMC streaming pattern that the delta pipeline needs, which is
why `par_bridge` is the relevant comparison target.

A grep across the workspace confirms zero remaining `par_bridge` or
`ParallelBridge` call sites today; reintroducing it for benchmark code
must be feature-gated under `cfg(feature = "bench")` so production
builds do not regain the import.

## crossbeam work-stealing deque alternative

`crossbeam::deque::Worker<T>` is a Chase-Lev-style FIFO/LIFO deque with
`Stealer<T>` handles that allow other threads to steal from the tail.
The intended pattern is:

1. Allocate one `Worker<DeltaWork>` per rayon worker.
2. Distribute `Stealer` clones to every worker so any thread can steal
   from any other thread's queue when its own deque is empty.
3. The producer pushes new items round-robin onto the workers' deques.
4. Workers pop from their own deque first, fall back to stealing,
   then park on a `Condvar` or `crossbeam::sync::WaitGroup` when both
   their deque and all stealers report empty.

The deque uses lock-free atomics: `push`/`pop` are wait-free on the
owning thread, and `steal` returns `Steal::Empty`, `Steal::Success(T)`,
or `Steal::Retry` (the last one indicating ABA contention with the
owner). This is the same primitive rayon's internal scheduler is built
on, so a bespoke implementation effectively duplicates rayon's plumbing
rather than replacing it - the comparison measures whether bypassing
the rayon-task abstraction is worth the additional code surface for our
specific workload shape.

Cost dimensions:

- Per-item enqueue: rayon's `scope().spawn(..)` allocates a heap-resident
  task closure. `Worker::push` writes into a pre-allocated ring with no
  allocation on the steady-state path.
- Per-item dequeue: rayon's deque-of-tasks walks task pointers; the raw
  deque deals in `T` directly with no closure indirection.
- Steal latency: both are O(1) atomic loads, but rayon's path goes
  through job execution and may incur an extra `unpark` on the steal
  victim. The raw deque steal is one CAS plus a memcpy.
- Memory footprint: rayon allocates per-task. The deque has a fixed
  capacity per worker (default 256) plus `Stealer` overhead.

## Proposed benchmark methodology

Add a criterion benchmark at `crates/engine/benches/work_queue_strategies.rs`
that exercises three workloads under both strategies. Bench code is
gated behind a `bench` feature so the `par_bridge` import does not leak
into release builds.

### Workload 1: 100K small files (1 KiB each)

Pre-populate a `tempfile::TempDir` with 100,000 files of exactly 1024
bytes, contents `b"x" * 1024`. Wrap each as a `DeltaWork::SignatureOnly`
item (no basis match path, the cheapest possible work). Producer pushes
all 100K items into the queue and closes. Consumers compute a no-op
delta and return. Measures pure scheduling overhead - the work per item
is dominated by enqueue/dequeue/steal.

Expected throughput target: > 1M items/sec on an 8-core box. Anything
below that means scheduling is the bottleneck, not the algorithm under
test.

### Workload 2: 100K medium files (1 MiB each)

Same shape, but each file is 1 MiB and the consumer runs the real
rolling-checksum + delta-match path against an empty basis. Work per
item dominates scheduling, so the benchmark measures whether the
strategy starves cores or saturates them. Expected variance between
strategies: < 5% on this workload.

### Workload 3: file-size mix

A realistic mix matching common rsync corpora: 50% files at 1 KiB,
30% at 64 KiB, 15% at 1 MiB, 5% at 16 MiB, total 100K files. Generated
with a deterministic PRNG seed (`StdRng::seed_from_u64(1403)`) so
runs are reproducible. Consumers run the full delta path. This is the
workload most representative of production traffic and the one whose
result drives the decision.

### Harness rules

- `criterion` 0.5 with `measurement_time = 30s`, `sample_size = 50`.
- Pin rayon thread count to `num_cpus::get_physical()` and disable
  hyperthreading siblings via taskset/affinity so steal latency is
  not skewed by SMT contention.
- Drop page cache between runs (`/proc/sys/vm/drop_caches` on Linux,
  `purge` on macOS) so warm-cache effects do not leak between
  strategies.
- Report wall time, items/sec, peak RSS (sampled by a sidecar reading
  `/proc/self/status`), and rayon steal-count from
  `rayon::current_thread_index()` instrumentation.
- Run on the `localhost/oc-rsync-bench:latest` Arch container for
  repeatability against the `BENCH_RUNS` env knob; cross-check on macOS
  hardware to surface scheduler divergence.
- Validate that both strategies complete with identical
  `ReorderBuffer` output by running each end-to-end on workload 3 and
  comparing the produced `Vec<DeltaResult>` byte-for-byte.

## Expected outcomes per workload

Workload 1 (100K x 1 KiB): the raw deque should win by 1.5-3x on
items/sec because enqueue/dequeue dominate. `par_bridge` allocates a
rayon task per item, which is 100K heap allocations versus 100K ring
writes. `rayon::scope` (current code) sits between the two: it
allocates per-spawn but reuses the channel for transport. If the deque
margin is below 2x, the implementation cost is not justified.

Workload 2 (100K x 1 MiB): all three strategies should converge to
within 5%. Per-item work is on the order of milliseconds, which dwarfs
sub-microsecond scheduling differences. A win larger than 5% for any
strategy is a measurement artifact (likely cache thrash or background
load) and the run should be repeated.

Workload 3 (size mix): the deque should retain a 10-20% advantage
because the small-file tail of the distribution still pays scheduling
overhead per item. `par_bridge` and `rayon::scope` should be within
~5% of each other on this workload, with `rayon::scope` slightly ahead
because it avoids the `par_bridge` iterator-borrow setup cost on every
batch.

Memory: the deque should peak at `capacity * num_workers * sizeof(DeltaWork)`,
roughly 256 * 8 * 64B = 128 KiB. `par_bridge` and `rayon::scope` peak
at the bounded-channel cap (2 * num_workers items) plus per-task
allocations, similar order of magnitude. No strategy should show
unbounded growth on the 100K runs.

## Decision criteria

Adopt the crossbeam work-stealing deque if and only if all of:

1. Workload 1 shows a >= 2x throughput improvement and
2. Workload 3 shows a >= 15% throughput improvement and
3. Workload 2 regressions stay within 5% (no large-file penalty) and
4. Peak RSS stays within 10% of the current implementation and
5. The end-to-end output check on workload 3 passes byte-for-byte.

Keep the current `rayon::scope` design if any of (1)-(4) fail.
Reintroduce `par_bridge` only if it wins workload 1 by >= 1.5x without
losing workloads 2 or 3 - given the lifetime friction documented in
PR #1538, the bar for re-adopting `par_bridge` is intentionally
higher than the bar for adopting the deque.

If the benchmark shows the deque wins workload 1 but loses workload 3,
investigate whether a hybrid (rayon for large items, deque for small)
is justified before committing to a rewrite. The audit recommends
starting with a single-strategy decision and revisiting hybrids only
if the simple form leaves measurable throughput on the table.

## References

- `crates/engine/src/concurrent_delta/work_queue/mod.rs:1-50` - current
  SPMC contract and bounded-channel rationale.
- `crates/engine/src/concurrent_delta/work_queue/drain.rs` -
  `drain_parallel` implementation using `rayon::scope`.
- `crates/engine/src/concurrent_delta/work_queue/capacity.rs` -
  `2 * num_threads` capacity heuristic.
- `crates/engine/src/concurrent_delta/work_queue/multi_producer.rs` -
  multi-producer audit landing site (issues #1382, #1569).
- PR #1538 - removal of `par_bridge` from
  `crates/engine/src/pipeline/work_queue.rs`; the previous file path
  is the historical reference for the old SPMC bridging pattern.
- `crates/checksums/src/parallel/files.rs:43,238,341` - existing rayon
  `par_iter_mut` consumers, included as the contrasting non-streaming
  pattern.
- crossbeam-deque crate documentation:
  `https://docs.rs/crossbeam-deque/latest/crossbeam_deque/` - `Worker`,
  `Stealer`, and `Steal::Retry` semantics.
- rayon `ParallelBridge` documentation:
  `https://docs.rs/rayon/latest/rayon/iter/trait.ParallelBridge.html` -
  iterator-borrow contract and per-item task spawn cost.
- `tools/ci/run_interop.sh` and `scripts/benchmark_hyperfine.sh` -
  reference for reproducible bench harness wiring; the new criterion
  bench plugs into the same container image.
