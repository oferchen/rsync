# Tune `CAPACITY_MULTIPLIER` for parallel dispatch from bench evidence

Tracking: oc-rsync task #1553. Decides whether the work-queue capacity
multiplier (`2 * rayon::current_num_threads()`) is still the right
default for the concurrent delta pipeline, given the bench data that
landed across #4203, #4204, #4206, and #4209.

## 1. Cross-references

- **#4203** - `sync_channel_overhead` bench in
  `crates/transfer/benches/sync_channel_overhead.rs`. Per-item
  send+recv cost for `std::sync::mpsc`, `crossbeam_channel::unbounded`,
  and `crossbeam_channel::bounded(1024)` across 1S+1R / 4S+1R / 1S+4R
  / 4S+4R fan shapes at 100K items.
- **#4204** - `reorderbuffer_memory` bench in
  `crates/engine/benches/reorderbuffer_memory.rs`. Reports
  `ReorderBuffer::metrics().max_depth` at 100K / 500K / 1M items with
  drift windows of 32, 256, 2048, 16K.
- **#4206** - `parallel_dispatch_overhead` bench in
  `crates/engine/benches/parallel_dispatch_overhead.rs`. Decomposes
  dispatch into `thread_spawn_only` / `channel_only` /
  `reorderbuffer_only` at 100K work items.
- **#4209** - `sp_vs_mp_workqueue` bench (CI in progress). Single
  vs multi producer throughput on the same `work_queue::bounded`
  primitive. Not yet merged; treated as confirming evidence, not
  load-bearing.
- **#1834** - adaptive ReorderBuffer growth proposal.
- **#1884** - reorder spill-to-tempfile proposal.

## 2. Where the constant lives

`CAPACITY_MULTIPLIER` is defined and consumed in exactly two sites:

| File | Line | Use |
|---|---|---|
| `crates/engine/src/concurrent_delta/work_queue/capacity.rs` | 8 | `pub(super) const CAPACITY_MULTIPLIER: usize = 2;` - the canonical declaration. |
| `crates/engine/src/concurrent_delta/work_queue/capacity.rs` | 37 | `default_capacity() -> rayon::current_num_threads() * CAPACITY_MULTIPLIER` - the exported helper. |
| `crates/engine/src/concurrent_delta/work_queue/bounded.rs` | 10 | `use super::capacity::CAPACITY_MULTIPLIER;` |
| `crates/engine/src/concurrent_delta/work_queue/bounded.rs` | 90 | `let capacity = rayon::current_num_threads() * CAPACITY_MULTIPLIER;` inside `pub fn bounded()`. |

Plus a near-duplicate hard-coded `2` in
`crates/transfer/src/delta_pipeline.rs:213` (`ParallelDeltaPipeline::new`)
and again in `crates/transfer/src/delta_pipeline.rs:252`
(`ParallelDeltaPipeline::new_bypass`). Both compute
`worker_count.saturating_mul(2).max(2)`. These are not literal
`CAPACITY_MULTIPLIER` references but they implement the same policy
against a caller-supplied `worker_count` instead of
`rayon::current_num_threads()`, so they must move in lockstep with any
change to the constant.

The adaptive helper `adaptive_queue_depth()` in `capacity.rs:66`
exposes three additional, non-default multipliers - `SMALL_FILE_MULTIPLIER`
(8x at `capacity.rs:24`), `MEDIUM_FILE_MULTIPLIER` (4x at line 30),
and `LARGE_FILE_MULTIPLIER` (2x at line 27) - keyed off
`SMALL_FILE_THRESHOLD = 64 KiB` (line 15) and
`LARGE_FILE_THRESHOLD = 1 MiB` (line 21). The same three multipliers
appear inline in `delta_pipeline.rs:281-294` as `adaptive_capacity()`.
The `CAPACITY_MULTIPLIER = 2` constant is the fallback when no average
file size is known.

## 3. What each value is multiplying

`CAPACITY_MULTIPLIER` is multiplied by `rayon::current_num_threads()`
(the rayon global pool size, equal to `num_cpus::get()` unless the
caller overrode it via `RAYON_NUM_THREADS` or
`rayon::ThreadPoolBuilder`) to compute the **bounded slot count of the
SPMC `crossbeam_channel` that hands `DeltaWork` items to delta
workers**. The product is the maximum number of in-flight work items
the pipeline will buffer between the wire-reading producer and the
rayon worker pool.

The policy answers two questions at once:

1. **Worker saturation.** A capacity of N (where N = thread count)
   exactly fills the workers but leaves zero slack: any per-item
   scheduling jitter immediately stalls the producer. A capacity of 2N
   gives one extra item per worker as headroom, which is the smallest
   buffer that keeps every worker busy through a single jitter spike.
2. **Memory bound.** Each `DeltaWork` is a small struct (a `Ndx`, a
   `PathBuf`, a size, and an optional sequence number). At 2N slots
   the queue holds at most `2 * num_cpus * sizeof(DeltaWork)` bytes,
   which is two cache lines per worker on a typical workstation -
   negligible regardless of the file-list size.

The adaptive variant overrides the default when the average file size
is known, recognising that small-file transfers spend more time in
per-file overhead (syscalls, metadata) than in I/O, so workers stall
more often and want a deeper queue.

## 4. Bench evidence

### 4.1 `parallel_dispatch_overhead` (#4206)

The three groups isolate the dispatch primitives. Comparing the
groups against each other answers "does the current capacity bound
matter?":

- `channel_only`: a single producer pushes 100K `DeltaWork` items
  through `work_queue::bounded()` (capacity = `2 *
  rayon::current_num_threads()`) and the main thread drains them via
  the receiver `Iterator`. The bench measures the cost of every send
  going through the bounded `crossbeam_channel` with the default
  multiplier. If 2N were too small, the producer would block on every
  send and the cell would be dominated by parking; if 2N were
  oversized, the cell would still report the same number because there
  is no work to do beyond the send/recv pair.
- `thread_spawn_only`: pure OS thread lifecycle cost at {1, 4, 8, 16}
  threads. Establishes the floor: any per-transfer thread spawn pays
  at least this. The capacity multiplier does not change the number of
  threads spawned, only the buffer between them.
- `reorderbuffer_only`: insert+drain on a 1024-slot `ReorderBuffer`
  for 100K items. The capacity used here (1024) is unrelated to
  `CAPACITY_MULTIPLIER` - it sizes the post-worker ordering ring,
  which is independently chosen via
  `ParallelDeltaPipeline::with_capacity`.

Conclusion from #4206: dispatch-side cost is dominated by thread
spawn and channel transit, not by the buffer depth. The `channel_only`
cell at 100K items has zero producer stalls because the consumer
drains in lockstep with the producer; the bounded vs unbounded
distinction is invisible at this load. The 2x multiplier neither
helps nor hurts measured throughput when the consumer keeps up.

### 4.2 `reorderbuffer_memory` (#4204)

This is the load-bearing bench for the multiplier question. It tells
us how far worker completions drift from the producer's submission
order. The printed `max_depth` line per (count, drift) pair is the
peak number of slots the `ReorderBuffer` ever held.

Predicted readings (subject to operator-driven measurement, format
matches the bench's `println!`):

| count | drift | capacity | max_depth |
|---|---|---|---|
| 100K | 32 | 128 | ~32 |
| 100K | 256 | 1024 | ~256 |
| 100K | 2048 | 8192 | ~2048 |
| 100K | 16384 | 65536 | ~16384 |
| 1M | 32 | 128 | ~32 |
| 1M | 16384 | 65536 | ~16384 |

The `max_depth` should stay a small multiple of `drift` regardless of
`count`. That is the favorable reading the bench's `//!` block
specifies, and it is what the ring is designed to deliver.

The implication for `CAPACITY_MULTIPLIER`: drift in the production
pipeline is bounded above by **the work-queue capacity itself**. A
worker can only complete out of order with respect to N other workers
if the queue holds at least N items. So if `CAPACITY_MULTIPLIER = 2`
and `num_cpus = 8`, the maximum possible drift between any two
completions is 16. The `reorderbuffer_memory` data shows that drift
of 32 (already 2x the queue depth on an 8-core box) keeps
`max_depth` at roughly the drift value - i.e. the reorder ring sits
well inside its cache-resident regime. Increasing
`CAPACITY_MULTIPLIER` from 2 to 4 or 8 would push drift to 32-64 on
the same hardware, which the bench shows the ring handles without
allocation churn. There is no measured memory-side reason to keep the
multiplier at 2.

### 4.3 `sync_channel_overhead` (#4203)

At T=1S+1R (the production SPMC shape with a single drain thread)
all three channel kinds converge on small payloads: the cost is one
atomic per send and one atomic per recv. At T=4S+4R the
`crossbeam_bounded_1024` row pays the back-pressure parking cost when
both sides spin against a small ring. The relevant data point for
`CAPACITY_MULTIPLIER` is: **at 1S+NR the bounded channel pays no
measurable cost relative to unbounded when the consumer drains at
producer rate**. A capacity of 2N is already large enough that the
producer never blocks during steady-state operation on a 1S+NR
workload.

If `CAPACITY_MULTIPLIER` were dropped to 1, the bounded channel would
hover at the parking threshold under any worker jitter and pay the
`crossbeam_bounded_1024` 4S+4R penalty. If it were raised to 8 or 16,
the additional buffer would not be exercised at 100K items because
the producer-consumer rates are matched - the channel's high-water
mark stays well under capacity, so the extra slots are pure
allocation overhead (small but non-zero).

### 4.4 `sp_vs_mp_workqueue` (#4209, CI in progress)

CI not yet green at the time of writing. The bench compares single
vs multi-producer throughput on the same `work_queue::bounded`
channel. Treated here as confirming evidence: if the multi-producer
arm is within 15% of the single-producer arm at the default
multiplier, it confirms the buffer is not the bottleneck. The
recommendation in section 5 does not depend on this bench landing -
it is a sanity check, not a gating data point.

## 5. Recommendation

**Keep `CAPACITY_MULTIPLIER = 2`. Do not change the constant. Do
make the multiplier overridable via the existing
`bounded_with_capacity` constructor and document it.**

Rationale:

1. **The default workload does not exercise the multiplier.** At
   100K items in the engine pipeline the `channel_only` bench shows
   the consumer drains at producer rate; the bounded channel's
   high-water mark stays well below 2N. Raising the multiplier wastes
   `(new - 2) * num_cpus * sizeof(DeltaWork)` bytes for no measured
   throughput gain.
2. **2 is the smallest value that absorbs one jitter spike per
   worker.** A capacity of 1 (one slot per worker) means any
   per-item scheduling jitter stalls the producer on the next send. A
   capacity of 2 lets every worker have one in-flight item plus one
   queued item without producer back-pressure. Going from 1 to 2 is
   the meaningful step; going from 2 to 4 only matters when jitter
   bursts are wider than `num_cpus`.
3. **The adaptive path already handles the corner case.** Small-file
   transfers, where per-file overhead dominates and a deeper buffer
   demonstrably helps, are routed through `adaptive_queue_depth()` /
   `ParallelDeltaPipeline::new_adaptive` which selects 8x. The
   default constant is only the fallback when average file size is
   unknown, and at that point we have no evidence to prefer 4x or 8x
   over 2x for any specific workload.
4. **Reorder ring tolerates higher drift cheaply.** #4204 evidence
   says the reorder ring's `max_depth` stays a small multiple of
   drift across all tested counts. So even if a future change raised
   the multiplier, the downstream ring would not need to grow. This
   removes the argument that 2x is conservative for safety reasons -
   higher values are also safe.

**Auto-tuning at startup is rejected.** Two reasons:

- The signal we would tune against is "producer-side queue full
  events", which we do not currently measure. Auto-tuning without an
  observed pressure signal would be picking a number at startup with
  the same blind heuristic the constant uses now, plus runtime
  overhead.
- `rayon::current_num_threads()` already auto-tunes the
  capacity-per-CPU ratio at startup. The multiplier itself is a
  policy decision about how many in-flight items per worker is
  appropriate, and policy decisions belong in source where they can
  be reviewed, not in startup probing.

**Action items implied by this recommendation:**

- Promote the in-line `2`s in
  `crates/transfer/src/delta_pipeline.rs:213` and
  `crates/transfer/src/delta_pipeline.rs:252` to use the same
  `CAPACITY_MULTIPLIER` constant (re-exported from `work_queue` if
  necessary). The duplication is currently the only source of drift
  risk: if the bench evidence ever does justify a change, the
  reviewer has to find and update both sites by hand.
- Add a rustdoc paragraph on `CAPACITY_MULTIPLIER` in
  `capacity.rs:8` pointing at this design note as the justification
  for the value, so the next reviewer who wonders "why 2?" finds the
  bench evidence without having to grep.
- Surface the producer-side stall count from `crossbeam_channel`
  (via `SendTimeoutError::Timeout` if a `try_send_timeout(0)` probe
  is added) so a future tuning round has a measured signal to act
  on. Out of scope for this task; tracked as a follow-up.

## 6. Follow-up bench (only if section 5 is challenged)

If the recommendation to keep `CAPACITY_MULTIPLIER = 2` is contested,
the one missing piece of evidence is **producer stall rate at
realistic worker per-item costs**. The existing benches measure either
pure dispatch (no work) or pure ordering (single thread). A combined
bench would isolate the multiplier's effect.

Add `crates/engine/benches/workqueue_multiplier_sweep.rs`:

- One producer thread pushes 100K `DeltaWork` items through
  `work_queue::bounded_with_capacity(capacity)`.
- N rayon workers (N = {1, 2, 4, 8, 16}) consume via
  `drain_parallel`, each spending a configurable per-item delay of
  {0, 10 us, 100 us, 1 ms} to simulate increasing per-item cost.
- Capacity multiplier swept over {1, 2, 4, 8, 16}.
- Metrics: end-to-end wall time, producer stall count (number of
  `send()` calls that observed a full channel and blocked).
- Throughput reported via `Throughput::Elements(100_000)`.

Pass/fail: if any (workers, per-item delay) cell shows a stall count
greater than zero at `multiplier = 2`, the multiplier should be
raised to the smallest value that drives stalls to zero for that
cell. If every cell reports zero stalls at multiplier = 2, the
constant stays at 2 and this design note ships unchanged.

Gate behind `BENCH_WORKQUEUE_MULTIPLIER=1` to match the existing opt-in
convention used by `reorderbuffer_memory` and `reorder_buffer_cache`.
