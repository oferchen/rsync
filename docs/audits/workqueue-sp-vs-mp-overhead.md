# `WorkQueue` SP vs MP overhead benchmark plan (#1572)

Tracking task: oc-rsync task #1572 (single-producer vs multi-producer
overhead for the concurrent delta `WorkQueue`). Companion sources:
`crates/engine/src/concurrent_delta/work_queue/{mod,bounded,capacity,drain,multi_producer}.rs`,
`crates/engine/src/concurrent_delta/multi_producer_audit.rs`, and
`crates/engine/benches/drain_parallel_benchmark.rs`. Adjacent design:
`docs/audits/drain-parallel-contention-static-analysis.md` (#1679,
#1681, #1682) and the Arc-wrapped sender note at
`docs/design/arc-wrapped-work-queue-sender.md` (#3812).

Last verified: 2026-05-07 against
`crates/engine/src/concurrent_delta/work_queue/` and the engine
`Cargo.toml` feature table.

This is a read-only plan. No source files are modified. It defines the
criterion harness, axes, and decision criteria that gate whether the
`multi-producer` cargo feature stays gated forward-looking
infrastructure or graduates into a default-on capability.

## Summary

The `WorkQueueSender` is `Send + !Clone` by default. The
`multi-producer` cargo feature flips on a `Clone` impl that exposes the
underlying `crossbeam_channel::Sender` clone, turning the queue from
SPMC into MPMC. The audit at
`crates/engine/src/concurrent_delta/multi_producer_audit.rs` already
concluded there is no production call site that needs MP today (the
wire protocol is single-stream on receive, and local copy bypasses the
queue). This benchmark plan answers the harder question: if a future
caller wanted MP, what does it cost and at what fan-in does it actually
win?

The output is a decision table indexed by `(producers, consumers,
items)` that says either:
- "MP overhead is in the noise" -> keep the feature, document zero-cost
  graduation.
- "MP slower at all measured points" -> close the feature as
  not-justified, drop the gated `Clone` impl.
- "MP wins above N producers" -> document the crossover and require
  callers to opt in by feature plus a runtime threshold.

## Current `WorkQueueSender` shape

Source citations against `crates/engine/src/concurrent_delta/work_queue/`:

- **Backend.** `bounded.rs:8` imports
  `crossbeam_channel::{Receiver, Sender}`. `bounded.rs:102` calls
  `crossbeam_channel::bounded(capacity)` to construct the queue.
- **Default capacity.** `bounded.rs:90` computes
  `rayon::current_num_threads() * CAPACITY_MULTIPLIER` with
  `CAPACITY_MULTIPLIER = 2` (`capacity.rs:8`). Adaptive sizing
  (`capacity.rs:66`) bumps small-file workloads to 8x and clamps
  large-file to 2x.
- **Send path.** `bounded.rs:78` is a thin wrapper:
  `self.tx.send(work).map_err(|e| SendError(e.0))`. No retry, no
  ordering glue; the bounded `Sender` blocks when full and that is the
  backpressure.
- **Clone gating.** `bounded.rs:48` declares `WorkQueueSender { tx:
  Sender<DeltaWork> }` without deriving `Clone`. The `Clone` impl is
  isolated in `multi_producer.rs:17` behind
  `#[cfg(feature = "multi-producer")]` (`mod.rs:102`). Engine
  `Cargo.toml:90` declares the feature with empty deps, so flipping it
  on adds zero compile units.
- **Static enforcement.**
  `multi_producer_audit.rs:251` asserts `WorkQueueSender: Send` and,
  under `#[cfg(not(feature = "multi-producer"))]`, documents that
  `Clone` is intentionally absent. That is the SP invariant the
  benchmark must compare against.
- **Drain side.** `drain.rs:57` (`drain_parallel`) and `drain.rs:136`
  (`drain_parallel_into`) both pull from a single `Receiver` via
  `into_iter()`; consumer fan-out is handled by `rayon::scope`. The
  drain side is unchanged between SP and MP, so the benchmark
  isolates the *send-side* delta and not the consumer side.

## `crossbeam_channel::bounded` MP semantics

Relevant when the SUT goes from one sender to N:

- **Single MPSC backing.**
  `crossbeam_channel::bounded(cap)` returns one channel; calling
  `Sender::clone` increments an internal Arc and shares the same
  ring. `WorkQueueSender::clone` (`multi_producer.rs:18-22`) just
  forwards that.
- **Slot-locked ring.** Crossbeam's bounded channel uses a locked
  array-backed ring (`Mutex` per slot stamp + park notifications),
  not the lock-free MPMC `crossbeam_queue::ArrayQueue`. Under SP fan-in
  the lock is uncontended; under MP fan-in N senders contend on the
  tail slot's stamp + the parking primitive whenever the queue is
  near-full or near-empty.
- **Wake amplification.** Every `send()` may wake a parked receiver,
  and every `recv()` may wake a parked sender. Adding senders does not
  add wakeups per item but increases the probability that the next
  `send()` finds the slot stamp owned by another producer, forcing a
  spin or park.
- **No FIFO across producers.** Items are FIFO per-producer but
  interleaved across producers. That is exactly why the audit at
  `multi_producer_audit.rs:38-66` insists ordering must be reasserted
  by an external `AtomicU64` sequence counter, demonstrated at
  `multi_producer_audit.rs:172-214`.
- **Backpressure unchanged.** Capacity is shared. With N producers
  each pushing at rate R, aggregate offered load is N*R; the bounded
  ring still caps in-flight at `capacity`, so total memory remains
  `capacity * sizeof(DeltaWork)`.

The SP path therefore has zero contention on the send slot; the MP
path adds contention proportional to the number of senders that race
on the same tail. That is the per-message overhead the benchmark must
quantify.

## Proposed bench

A new `crates/engine/benches/work_queue_sp_vs_mp.rs` modelled on
`drain_parallel_benchmark.rs:41-85`. The existing harness already
fixes consumer count via `rayon::ThreadPoolBuilder` and uses
`bounded_with_capacity(threads * 4)`; this plan keeps that shape and
adds a producer axis.

### Axes

- **Producers `P`**: `[1, 2, 4, 8]`. `P=1` is the SP baseline (the
  default-feature build). `P>=2` requires the `multi-producer`
  feature.
- **Consumers `C`**: `[1, 4, 8, 16]`, mapped to
  `rayon::ThreadPoolBuilder::num_threads(C)`. Matches
  `drain_parallel_benchmark.rs:23`.
- **Items `N`**: `[10_000, 100_000, 1_000_000]`. The drain benchmark
  stops at 100K; this plan extends to 1M to surface amortized
  send-side overhead. Each producer sends `N / P` items so total
  offered work is constant across `P`.
- **Capacity**: fixed at `C * 4` (matches existing harness) so the
  capacity-multiplier axis does not confound the producer-count axis.
  A second sweep at `C * 8` confirms small-file regime.
- **Per-item cost**: reuse `simulate_work` from
  `drain_parallel_benchmark.rs:31-39` so the consumer side is
  identical to the existing benchmark and the only delta is on the
  producer.

### Configurations

Two harnesses share the same consumer scope:

- `bench_sp_send` (default features). One `std::thread::spawn`
  producer feeds `N` items. Mirrors
  `drain_parallel_benchmark.rs:61-67`. This is the control.
- `bench_mp_send` (`--features multi-producer`). Spawn `P` producers,
  each cloning the sender via the gated `Clone` impl, each pushing
  `N / P` items with sequence numbers claimed via
  `AtomicU64::fetch_add(1, Ordering::SeqCst)`. The atomic-coordination
  pattern is the one already validated at
  `multi_producer_audit.rs:174-214`.

Both harnesses call `rx.drain_parallel(|w| simulate_work(...))`
identically and `black_box` the output, so any throughput delta is
attributable to the send side.

### Metrics

- **Throughput** (items/sec) via `Throughput::Elements(N as u64)`.
  Already wired into the existing harness at
  `drain_parallel_benchmark.rs:46`.
- **Per-message overhead** (ns/item) derived from criterion's
  per-iter timing divided by `N`. Report SP and MP curves on the same
  axis.
- **Send-side wall time**: instrument the producer thread(s) with
  `Instant::now()` deltas and report `(producer_total_ns) / N` so that
  send-side cost is separated from drain cost. The existing harness
  does not split these; this plan adds the split because the
  hypothesis is purely about send-side overhead.
- **Aggregate vs per-producer**: for `P>1` capture both
  `max(producer_ns)` (longest producer) and `sum(producer_ns) / P`
  (mean). Divergence between max and mean indicates contention rather
  than work imbalance.

### Reporting

Criterion group `work_queue_send` with one bench per
`(mode, P, C, N)` tuple, named e.g. `mp/4p/8c/100k`. Output the
default criterion HTML plus a CSV-ready table that the decision
criteria below consume. Track regressions via the existing
`scripts/benchmark.sh` harness; this benchmark joins the engine
benches list in `crates/engine/Cargo.toml` once it lands.

### Sanity checks before publishing

- Each MP run asserts that `results.len() == N` and that the sequence
  numbers cover `0..N` exactly once after a stable sort, matching
  `multi_producer_audit.rs:206-213`.
- SP and MP runs must produce identical output multisets when sorted
  by `(ndx, sequence)`. If they disagree the benchmark is invalid and
  must not be reported.
- `bounded_with_capacity` is called fresh per criterion iteration to
  avoid amortizing channel allocation across samples.

## Expected outcomes

These predictions are derived from the channel semantics above and
from the existing single-producer-overhead measurements implicit in
`drain_parallel_benchmark.rs`. They are hypotheses the benchmark
falsifies or confirms.

### Where MP is expected to lose

- **`P=1` MP vs `P=1` SP.** The MP build adds one extra Arc-clone per
  channel construction (the `Clone` impl in
  `multi_producer.rs:18-22`), nothing else. Expected delta: <1%, in
  the noise of criterion's confidence interval. If the delta exceeds
  3% the cause is almost certainly inlining differences between the
  two cargo feature builds and must be investigated before any other
  conclusion is drawn.
- **`P=2..4`, `N=10_000`.** With small `N` the channel is rarely
  full, so contention windows are short. Per-item overhead is
  dominated by the atomic sequence counter
  (`fetch_add(1, SeqCst)` in `multi_producer_audit.rs:184`) plus
  cache-line bouncing on the channel tail. Expected: MP is 5-15%
  *slower* per item than SP at this point because the work per item
  is too small to amortize the coordination cost.
- **`C=1`.** A single consumer drains slower than `P` producers can
  fill, so the queue spends most of its time full and senders park.
  Adding producers cannot help because the consumer is the
  bottleneck; MP just adds park/unpark traffic. Expected: MP equal
  to or slightly worse than SP for any `P>1` when `C=1`.

### Where MP is expected to win

- **`P=N=large`, `C>=4`.** When per-item send cost (DeltaWork
  construction, sequence stamping, channel push) starts to outrun
  one CPU core's throughput, splitting the producer across cores
  recovers headroom. The `simulate_work` cost is constant on the
  consumer side; if the test fixture wraps a heavier producer-side
  cost (e.g., file-list parsing) MP wins earlier. With the
  `simulate_work` shape proposed here, the crossover is expected
  near `N=1_000_000, P>=4, C>=8`.
- **`P=2, C=16, N=1_000_000`.** Two producers feeding 16 consumers
  at 1M items is the regime where consumers can drain fast enough to
  keep the queue near-empty (eliminating tail contention) while
  splitting send-side work across two cores. Expected: 30-50%
  throughput improvement vs SP.

### Per-message overhead targets

- **SP send.** Single uncontended `crossbeam_channel::Sender::send`
  on a bounded ring is on the order of 30-80 ns/item on modern x86
  / Apple Silicon when the queue is below capacity. This is the
  reference number `bench_sp_send` measures.
- **MP send, no contention (`P=1` with feature on).** Within 2 ns of
  SP. Anything larger means the `Clone` impl introduced an
  unexpected indirection.
- **MP send, light contention (`P=2`, queue rarely full).** Add
  `fetch_add(1, SeqCst)` on the sequence counter (~5-10 ns) plus one
  Arc refcount round-trip on the cloned `Sender`'s send path
  (~2-5 ns). Expected total: 40-100 ns/item.
- **MP send, heavy contention (`P>=4`, queue near-full).** Park /
  unpark traffic dominates. Expected: 200-500 ns/item, i.e. 3-6x SP.
  This is the regime where MP provably loses on per-item cost and
  can only win by spreading send work across cores faster than the
  contention tax.

### Decision criteria

After running the matrix:

- If `mp/1p/*` is within 2% of `sp/1p/*` *and* there exists any
  `(P, C, N)` where MP throughput exceeds SP by >=15%, keep the
  feature. Document the crossover in `mod.rs:31-35` and reference
  this audit. Add a runtime guard in any future caller so SP is the
  default and MP is opt-in only when the workload matches the
  validated regime.
- If `mp/1p/*` regresses SP by >2% even with no contention, that is
  a bug in the gating, not a property of MP. Investigate before
  drawing conclusions.
- If MP never wins by >=15% across the whole matrix, close the
  feature as not-justified. Drop `multi_producer.rs` and the gated
  block in `mod.rs:102`. Update
  `multi_producer_audit.rs:83-95` to record the falsification and
  cite this audit's commit SHA.

The 15% threshold matches the buffer-pool sharded-benchmark gate
(`docs/audits/bufferpool-sharded-benchmark-plan.md`), keeping the
project's "speedup must clear noise plus measurement variance"
convention consistent across optimization decisions.

## Out of scope

- Lock-free MPSC alternatives (e.g.,
  `crossbeam_queue::ArrayQueue`-backed sender). Considered in
  `docs/design/arc-wrapped-work-queue-sender.md` (#3812); not part of
  this overhead study.
- Async send paths. The work queue is sync today; an async sender is
  a different design question.
- Cross-architecture parity sweeps. The harness runs on the standard
  CI matrix (Linux x86_64 stable, macOS aarch64 stable, Windows
  stable); aarch64 NEON parity is already covered by the SIMD parity
  tests and is not what this benchmark measures.
