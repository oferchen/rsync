# Lock-Free MPSC `drain_parallel` Alternative (#1681)

## Summary

`WorkQueueReceiver::drain_parallel` today fans N rayon workers into a
sharded `Vec<Mutex<Vec<R>>>` keyed by `rayon::current_thread_index()`,
then flattens the shards once the rayon scope returns. Issue #1681 asks
whether a lock-free MPSC variant - `crossbeam_channel::unbounded()` is
the obvious candidate - is a net win at production worker counts
(T = 8 and T = 16) or whether the sharded mutex stays.

This note is a design sketch, not an implementation. The actionable
decision waits on the bench data from the `drain_parallel_alternatives`
group added in PR #4214; section 4 names the threshold the data must
cross before we commit to the migration. Recommendation: defer the
implementation until that data is in hand.

## 1. Current Mutex-Based Drain

The production drain lives in one method:

- `crates/engine/src/concurrent_delta/work_queue/drain.rs:57-90`
  - `pub fn drain_parallel<F, R>(self, f: F) -> Vec<R>`
  - Builds `num_shards = rayon::current_num_threads()` mutex-guarded
    vectors at `drain.rs:62-65`.
  - Spawns one `rayon::scope` task per `DeltaWork` item at
    `drain.rs:67-83`. Each task hashes its thread id when outside the
    rayon pool to avoid collapsing on shard 0
    (`drain.rs:73-80`), then locks the shard at
    `drain.rs:81` to append the result.
  - Flattens shards into a single `Vec<R>` at
    `drain.rs:86-89`.
- `crates/engine/src/concurrent_delta/work_queue/drain.rs:136-155`
  - `pub fn drain_parallel_into<F, R>(self, f: F, tx: Sender<R>)`
  - Streaming sibling. Already uses a `crossbeam_channel::Sender<R>`
    (`drain.rs:9, 144-149`) so the MPSC shape is partially
    present on the streaming side; the question is whether the batch
    variant should adopt the same shape.
- `crates/engine/src/concurrent_delta/work_queue/bounded.rs:48-60`
  - `WorkQueueSender` (no `Clone`, single-producer compile-time
    invariant) and `WorkQueueReceiver`, the types the drain consumes.
- `crates/engine/src/concurrent_delta/work_queue/mod.rs:23-29`
  - Ordering contract: items arrive in wire order from the single
    producer; consumers may complete out of order; sequential output is
    reconstructed downstream by `ReorderBuffer` (cited below).

The static contention question - "what does the sharded mutex cost at
T = 8 vs T = 16 with 100K items" - is answered by the existing
collector contention bench in
`crates/transfer/benches/parallel_stat_collector_contention.rs` (#4170):

- `parallel_stat_collector_contention.rs:75` (`const ITEMS: usize =
  100_000;`)
- `parallel_stat_collector_contention.rs:80` (`WORKER_COUNTS: &[usize]
  = &[1, 4, 8, 16];`)
- `parallel_stat_collector_contention.rs:136-150` - `shared_mutex` arm
  (single `Arc<Mutex<Vec<R>>>`).
- `parallel_stat_collector_contention.rs:152-175` - `sharded_mutex`
  arm; this is the shape `drain_parallel` uses today.
- `parallel_stat_collector_contention.rs:177-191` -
  `crossbeam_queue::SegQueue` lock-free baseline.
- `parallel_stat_collector_contention.rs:193-211` -
  `crossbeam_channel::unbounded` MPSC arm.

That bench targets the receiver's parallel-stat dispatcher
(`crates/transfer/src/parallel_io.rs::map_blocking`), not the
`drain_parallel` site, but the four arms share the producer/consumer
shape closely enough that its T = 8/16 ratios are a directional prior
on the `drain_parallel` numbers PR #4214 will publish.

## 2. Lock-Free MPSC Sketch

The smallest credible replacement keeps everything outside `drain.rs`
unchanged and swaps the per-shard `Mutex<Vec<R>>` for an unbounded MPSC:

```text
// crates/engine/src/concurrent_delta/work_queue/drain.rs
pub fn drain_parallel<F, R>(self, f: F) -> Vec<R>
where
    F: Fn(DeltaWork) -> R + Send + Sync,
    R: Send,
{
    let (tx, rx) = crossbeam_channel::unbounded::<R>();
    rayon::scope(|s| {
        for work in self.into_iter() {
            let f = &f;
            let tx = tx.clone();
            s.spawn(move |_| {
                let _ = tx.send(f(work));
            });
        }
    });
    drop(tx);
    rx.into_iter().collect()
}
```

Properties this shape preserves vs the current code:

- **API**: `pub fn drain_parallel<F, R>(self, f: F) -> Vec<R>` is
  unchanged. Callers in `crates/transfer/src/delta_pipeline.rs:150`
  and the integration tests in
  `crates/engine/tests/multi_producer_work_queue.rs:71, 142, 210,
  270, 327, 435` keep their call sites verbatim.
- **Concurrency**: `rayon::scope` with one task per item is preserved
  (matches `drain.rs:67-83`). Work-stealing across the rayon pool is
  unchanged.
- **Backpressure**: the bounded work queue producer side
  (`crates/engine/src/concurrent_delta/work_queue/bounded.rs:78-80`)
  still bounds in-flight items; the `crossbeam_channel::unbounded`
  collector is sized only by completed-but-not-yet-drained results,
  which is bounded by the rayon pool's in-flight task count.
- **Single-producer invariant for `WorkQueueSender`** stays compile-time
  enforced via the non-`Clone` `WorkQueueSender` at
  `crates/engine/src/concurrent_delta/work_queue/bounded.rs:48-50`.
  The MPSC collector is on the consumer side; it does not affect the
  producer-side contract documented in
  `crates/engine/src/concurrent_delta/work_queue/mod.rs:11-21`.

Property this shape gives up:

- **Shard ownership of allocations**: the current sharded `Vec<R>`
  amortises allocation across N pre-grown vectors (`drain.rs:63-65`).
  The MPSC variant trades that for `crossbeam_channel` node allocation
  on every `send`. PR #4214's per-item cost numbers at T = 8 and T = 16
  are the only way to know if the trade is favourable. Section 4 names
  the margin.

`drain_parallel_into` (the streaming variant at
`crates/engine/src/concurrent_delta/work_queue/drain.rs:136-155`)
already uses `crossbeam_channel::Sender<R>`. It does not need to
change. If PR #4214 picks MPSC, the streaming variant becomes a thin
wrapper that hands callers the receiver instead of draining it
internally; the batch variant is the only one whose body changes.

## 3. Ordering

The drain itself does not promise insertion order today, and the MPSC
variant does not change that. Three points cover the contract:

- `crates/engine/src/concurrent_delta/work_queue/drain.rs:22-24`
  documents "Results are collected in arbitrary order (determined by
  worker completion timing)". The sharded mutex sees the same
  arbitrary order; MPSC sees the same arbitrary order; neither shape
  is sorted.
- `crates/engine/src/concurrent_delta/work_queue/mod.rs:25-29`
  documents that sequential output, when required, is restored
  downstream by `ReorderBuffer`, not by the drain.
- `crates/engine/src/concurrent_delta/reorder/mod.rs:1-10` is the
  reorder buffer that absorbs the out-of-orderness (O(1) insertion and
  O(1) pop in sequence order). The drain feeds it and the buffer does
  the sort.

The integration tests confirm the call sites already wire
`drain_parallel` to `ReorderBuffer` for ordered consumption
(`crates/engine/tests/pipeline_reorder_integration.rs:233-247`).

The MPSC variant therefore does not need an extra sort phase. The one
case where MPSC has a subtly different shape than the sharded mutex is
**within a single rayon worker**: the sharded mutex preserves the
local push order inside each shard, MPSC preserves the local send
order on each `tx` clone. Both are "arbitrary across workers", so
`ReorderBuffer` is needed in both shapes for any consumer that cares.

## 4. Bench-Driven Decision

The bench that drives the decision is
`crates/engine/benches/drain_parallel_alternatives.rs`, added in
PR #4214. Its three arms are:

1. `sharded_mutex_vec` - the current production shape (mirror of
   `drain.rs:62-89`).
2. `per_thread_vec` - one task per worker (not per item), each owning
   its own `Vec<R>`, merged at the end. Lower contention than (1) but
   coarser scheduling.
3. `mpsc_unbounded_channel` - `crossbeam_channel::unbounded`, the
   shape sketched in section 2.

The sweep covers 10K and 100K items across 4, 8, 16 rayon workers.
Throughput is reported in elements/sec so per-iteration cost is
readable off the criterion summary.

Decision matrix this design commits to once PR #4214 data lands:

| Condition at 100K items | Action |
|---|---|
| `mpsc_unbounded_channel` >= 20% faster than `sharded_mutex_vec` at T = 8 AND T = 16, with no regression > 5% at T = 4 | Migrate `drain_parallel` to MPSC (gated, see section 5) |
| `per_thread_vec` >= 20% faster than `sharded_mutex_vec` and beats MPSC at T = 8/16 | Pick `per_thread_vec` instead; close #1681 as superseded |
| Both alternatives within +/- 10% of `sharded_mutex_vec` at every (T, items) cell | Keep the current sharded mutex; close #1681 as "no change warranted" |
| Any alternative regresses > 5% at T = 4 with no compensating win at T = 16 | Keep the current shape; the worst-case workstation is the binding case |

The 20% threshold is deliberately wider than the 15% gate used for the
multi-producer queue bench in
`docs/design/spsc-vs-mpsc-workqueue-bench.md:65-66`: switching a
collector shape that already works has higher churn cost than a flag
that already exists, so the win must be larger to clear the bar.

## 5. Migration Plan

If the matrix in section 4 selects MPSC (or `per_thread_vec`):

1. **Feature flag**, one release of soak time.
   Add `#[cfg(feature = "drain-mpsc")]` paired implementations in
   `crates/engine/src/concurrent_delta/work_queue/drain.rs`. Default
   off. The feature gates only the body of `drain_parallel`; the
   public signature stays identical.
2. **CI parity**. Add a `drain-mpsc` matrix entry to the existing
   nextest CI jobs so both shapes run the
   `multi_producer_work_queue.rs` and `pipeline_reorder_integration.rs`
   suites every PR. No new test files needed; the existing assertions
   on result count and post-reorder ordering cover both shapes.
3. **Bench gate**. Re-run
   `cargo bench -p engine --bench drain_parallel_alternatives`
   on the reference Mac Studio M2 Ultra host (see
   `MEMORY.md` and `crates/transfer/benches/parallel_stat_collector_contention.rs:78-80`
   for the reference host bracket) on the release branch before
   flipping the default.
4. **Default-on**. If the next release's benches stay within the
   section 4 thresholds, flip the cargo feature default. The losing
   shape lives behind the flag for one more release as a rollback
   path.
5. **Remove the flag**. Once a release ships with the new default and
   no regression report comes back from interop or benchmarks, delete
   the losing branch and the feature flag in a follow-up PR.

If the matrix selects "no change warranted" (the third row), the only
output is closing #1681 with a link to the PR #4214 numbers.

## 6. Cross-References

- #4170 - `crates/transfer/benches/parallel_stat_collector_contention.rs`
  - the established `Arc<Mutex<Vec>>` collector contention bench whose
    arm shapes this design mirrors. Provides the directional prior on
    T = 8/16 behaviour while PR #4214's drain-specific numbers are
    collected.
- #4173 - WorkQueueSender / `Mutex<Vec>` audit that named
  `drain_parallel` as one of the remaining production sites. The audit
  is the reason #1681 exists.
- #4203 - sync-channel overhead reference. PR #4214's MPSC arm uses
  `crossbeam_channel::unbounded` for parity with #4203 so the numbers
  compose without an apples-to-oranges adjustment.
- #4214 - the `drain_parallel_alternatives` bench at
  `crates/engine/benches/drain_parallel_alternatives.rs`. The data
  this design defers to.

## Recommendation

Defer the implementation until PR #4214's bench data is in on the
reference host. The MPSC sketch in section 2 is small enough that the
migration cost is dominated by the soak release in section 5, not the
code change. The 20% threshold in section 4 is the only thing that
should change that ordering.
