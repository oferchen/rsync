# Per-Worker Drain Channels for `concurrent_delta::work_queue` (DPC-3, #2848)

Status: design only. Implementation lands in DPC-5 behind a Cargo feature
flag; bench rerun lands in DPC-6.

Companion notes:
[[project_drain_parallel_mutex_vec_contention]],
[[project_apply_batch_write_serial]].

Cross-references prior art rather than rederiving it:
`docs/design/lockfree-mpsc-drain-design.md` covers an MPSC-only sketch for
issue #1681. This doc extends that work along the "per-worker private
lane" axis the audit in DPC-1 (#2846) called out, and commits to a single
primitive choice instead of leaving the matrix open.

## 1. Problem Statement

Current production drain
(`crates/engine/src/concurrent_delta/work_queue/drain.rs:57-90`):

- Builds `num_shards = rayon::current_num_threads()` mutex-guarded
  vectors at `drain.rs:62-65`:

  ```rust
  let num_shards = rayon::current_num_threads();
  let shards: Vec<std::sync::Mutex<Vec<R>>> = (0..num_shards)
      .map(|_| std::sync::Mutex::new(Vec::new()))
      .collect();
  ```

- Each `rayon::scope` task locks one shard at `drain.rs:81`:

  ```rust
  shards[idx % num_shards].lock().unwrap().push(result);
  ```

- Shard selection prefers `rayon::current_thread_index()`
  (`drain.rs:73-80`) with a hashed `ThreadId` fallback for threads that
  spawn outside the rayon pool. The fallback only avoids the degenerate
  "all threads collapse to shard 0" case; it does not pin a worker to a
  shard.

The contention shape DPC-1 audited and DPC-2 will bench:

- Per-item dispatch: one rayon task per `DeltaWork` item. At 100K items
  the lock acquire / release happens 100K times per drain, distributed
  across `num_shards` mutexes.
- Sharding is keyed on `rayon::current_thread_index()`, so the shards
  *coincide* with rayon workers in steady state. The mutex is therefore
  uncontested while a single worker is on the shard, but contested any
  time the rayon scheduler steals work across a shard boundary, which
  happens frequently at high worker counts.
- At T = 4 the steal rate is low and the mutex is rarely contended.
- At T >= 16 the steal rate, the wakeup storm, and the cache-line
  ping-pong on `Mutex<Vec<R>>` headers combine to push the mutex into
  the hot path. DPC-1 traced this to the `Mutex::lock` + `Vec::push`
  pair as the single hottest sequence in `drain_parallel`.

The replacement must remove the cross-worker lock acquire from the
per-item push path while preserving the wire-ordering contract the
downstream `ReorderBuffer` relies on.

## 2. Replacement Architecture

Per-worker drain lanes with merge-at-barrier:

1. **Private worker lane.** Each rayon worker (and each non-rayon
   producer thread, which is rare on this path) owns a private
   `SegQueue<DrainEntry<R>>`. Pushes do not lock and do not coordinate
   with any other worker.
2. **Stable lane registry.** The `WorkQueueReceiver` owns a
   `Vec<Arc<SegQueue<DrainEntry<R>>>>` of length `num_workers`, indexed
   by `rayon::current_thread_index()`. Non-rayon threads fall back to a
   hashed `ThreadId` -> lane index map, identical in shape to the
   current `drain.rs:73-80` fallback. The registry order is stable across
   the lifetime of the drain.
3. **Per-entry sequence tag.** Each `DrainEntry<R>` carries the
   `DeltaWork::ndx()` (`u32`) the entry was produced for. The tag is
   required at the merge step to reconstruct wire order; it is *not*
   used for ordering inside a lane.
4. **Merge-at-barrier.** When the `rayon::scope` returns (the only
   barrier in this path - section 3 walks through it), one thread (the
   thread that called `drain_parallel`) drains every lane in stable lane
   order, concatenating the per-lane vectors. The merged vector is
   handed to `ReorderBuffer` exactly as the current code does, so the
   downstream contract is unchanged.

Wire-ordering contract this preserves:

- The drain itself does not promise insertion order today
  (`drain.rs:22-24`). Neither does the per-worker variant.
- Sequential output is reconstructed by `ReorderBuffer` via the
  per-entry `ndx` tag. The merged vector from per-worker lanes is a
  permutation of the same `DrainEntry<R>` set the mutex variant
  produces; the `ndx` tag is identical between shapes, so
  `ReorderBuffer`'s output is byte-identical.

A note on the streaming sibling `drain_parallel_into`
(`drain.rs:136-155`): it already uses `crossbeam_channel::Sender<R>`
and has no `Mutex<Vec<R>>` to remove. This design does not touch it.
DPC-5 only changes the batch variant body.

## 3. Concurrency Model Decision

The three candidates the brief lists, scored against the
`drain_parallel` hot path:

| Primitive | Per-push cost | Drain cost (n_workers) | Notes |
|---|---|---|---|
| Thread-local `Vec<DrainEntry<R>>` + drain-at-barrier | 1 cache line touch (TLS slot + Vec header) | n_workers cache misses + n_workers reads of TLS slot from non-owner thread (requires `std::sync::Mutex` or a once-init handoff to publish per-thread Vec to the barrier owner) | Lowest steady-state cost; non-trivial publish step at barrier because TLS data is not visible to other threads without a synchronisation primitive |
| Per-worker `crossbeam_channel::unbounded` MPSC | 1 node alloc + 1 atomic CAS on tail per push | n_workers `recv` loops; small constant per drain | Cleaner ownership, but per-push allocation under load matches the cost lockfree-mpsc-drain-design.md flagged as the open risk |
| Per-worker `crossbeam_queue::SegQueue` | 1 atomic CAS on tail per push, segment-amortised allocation (segments hold 32 entries) | n_workers `pop` loops; bounded latency | Allocation is amortised across 32 entries per segment, no per-push node alloc, no Mutex on the publish step (the queue is intrinsically `Sync`) |

**Choice: per-worker `crossbeam_queue::SegQueue<DrainEntry<R>>`.**

Justification (cost model is in section 7):

- The thread-local + Mutex variant trades the per-item Mutex for a
  per-worker Mutex at the barrier. The barrier owner takes
  `n_workers` mutexes serially, which is small in absolute terms but
  reintroduces the lock the design is trying to remove. The publish
  step is also fragile across non-rayon threads (the hashed fallback
  workers do not have stable TLS slots).
- The per-worker MPSC variant has the cleanest ownership but pays one
  node allocation per push. DPC-1's audit estimates 100K-1M pushes per
  drain on long file lists. At 1M pushes the allocator becomes the
  bottleneck, which is the exact failure mode the existing
  `lockfree-mpsc-drain-design.md:120-128` warned about.
- `SegQueue` is intrinsically `Sync` (no Mutex on the publish step),
  amortises allocation across 32 entries per segment, and uses a single
  atomic CAS per push. It composes cleanly with the stable lane
  registry from section 2 and is already used elsewhere in the codebase
  (`crates/transfer/benches/parallel_stat_collector_contention.rs:177-191`),
  so DPC-5 inherits a vetted dependency rather than introducing one.

## 4. API Shape

Public surface is unchanged. Callers in
`crates/transfer/src/delta_pipeline.rs:150` and tests at
`crates/engine/tests/multi_producer_work_queue.rs:71, 142, 210, 270, 327, 435`
keep their call sites verbatim. Only the body of `drain_parallel`
changes.

Internal types added under
`crates/engine/src/concurrent_delta/work_queue/`:

```rust
// crates/engine/src/concurrent_delta/work_queue/per_worker_drain.rs
use crossbeam_queue::SegQueue;
use std::sync::Arc;

pub(super) struct DrainEntry<R> {
    pub ndx: u32,
    pub value: R,
}

pub(super) struct PerWorkerDrain<R: Send + 'static> {
    lanes: Vec<Arc<SegQueue<DrainEntry<R>>>>,
}

impl<R: Send + 'static> PerWorkerDrain<R> {
    pub fn new(num_workers: usize) -> Self {
        let lanes = (0..num_workers)
            .map(|_| Arc::new(SegQueue::new()))
            .collect();
        Self { lanes }
    }

    pub fn handle(&self) -> WorkerHandle<R> {
        let idx = rayon::current_thread_index().unwrap_or_else(|| {
            // Mirrors the existing fallback at drain.rs:73-80.
            let id = std::thread::current().id();
            let mut hasher = std::hash::DefaultHasher::new();
            std::hash::Hash::hash(&id, &mut hasher);
            std::hash::Hasher::finish(&hasher) as usize
        }) % self.lanes.len();
        WorkerHandle { lane: Arc::clone(&self.lanes[idx]) }
    }

    pub fn drain_into_vec(self) -> Vec<R> {
        let mut out = Vec::new();
        for lane in self.lanes {
            // Arc is the only outstanding reference at barrier; the
            // SegQueue is moved out via try_unwrap or drained in place
            // through pop().
            while let Some(entry) = lane.pop() {
                out.push(entry.value);
            }
        }
        out
    }
}

pub(super) struct WorkerHandle<R: Send + 'static> {
    lane: Arc<SegQueue<DrainEntry<R>>>,
}

impl<R: Send + 'static> WorkerHandle<R> {
    pub fn push(&self, ndx: u32, value: R) {
        self.lane.push(DrainEntry { ndx, value });
    }
}
```

The `drain_parallel` body becomes:

```rust
#[cfg(feature = "per-worker-drain-channels")]
pub fn drain_parallel<F, R>(self, f: F) -> Vec<R>
where
    F: Fn(DeltaWork) -> R + Send + Sync,
    R: Send + 'static,
{
    let drain = PerWorkerDrain::new(rayon::current_num_threads());
    rayon::scope(|s| {
        for work in self.into_iter() {
            let f = &f;
            let drain = &drain;
            s.spawn(move |_| {
                let ndx = work.ndx().get();
                let value = f(work);
                drain.handle().push(ndx, value);
            });
        }
    });
    drain.drain_into_vec()
}
```

Worker registration is *implicit*: `drain.handle()` is called per task
and indexes the stable lane registry. There is no explicit
`register_worker` step. This matches the current code's policy of
treating rayon worker indices as the registration key
(`drain.rs:73`).

The `DrainEntry::ndx` field is internal: the public return type
remains `Vec<R>`. The `ndx` tag is required for the wire-ordering
parity proof (section 5) and for any future variant that wants to sort
inside the drain instead of deferring to `ReorderBuffer`. DPC-5 ships
the tag even though the current callers do not consume it directly,
because removing it later forces a second design pass.

## 5. Ordering Invariant Proof Obligation

The drain does not produce wire-ordered output; `ReorderBuffer` does.
The proof obligation is therefore:

**Claim**: for every input sequence of `DeltaWork` items, the
`Vec<R>` produced by the per-worker drain, when fed through
`ReorderBuffer`, yields a byte-identical wire stream to the `Vec<R>`
produced by the Mutex baseline.

Existing test that demonstrates the contract for the Mutex baseline:
`crates/engine/tests/pipeline_reorder_integration.rs:233-247` -
this is the call site that wires `drain_parallel` to `ReorderBuffer`
and asserts wire-ordered output.

**DPC-5 must add one new test, gated by the cargo feature flag**, that
runs the same fixture through both implementations and asserts the
post-`ReorderBuffer` byte streams are identical:

- Location: `crates/engine/tests/per_worker_drain_parity.rs` (new file).
- Shape: take the same fixtures used by
  `pipeline_reorder_integration.rs:233-247`. Run them through (a) the
  Mutex baseline, (b) the per-worker drain. Compare `Vec<R>` after the
  `ReorderBuffer` sort step, not before.
- Worker counts to cover: `[1, 4, 8, 16, 64]`. T = 64 exercises the
  fallback hash path (rayon pool typically caps below this on the
  bench host).
- Item counts: `[100, 10_000, 100_000]`. The smallest is for
  determinism debugging; the largest reflects the production hot path.

The pre-sort `Vec<R>` is allowed to differ between implementations.
The post-sort wire stream is not.

The existing fixtures already cover the streaming
`drain_parallel_into` path; this design does not modify that path so
no new tests are required for it.

## 6. Backwards-Compatibility

DPC-5 lands behind a Cargo feature flag:

- Name: `per-worker-drain-channels`.
- Default: **OFF**.
- Scope: gates the body of `drain_parallel`. The public signature is
  identical with and without the flag, so downstream crates compile
  unchanged.
- Test parity: CI matrix gains one entry that builds the `engine`
  crate with `--features per-worker-drain-channels` and runs the
  parity test from section 5 plus the existing
  `multi_producer_work_queue.rs` and `pipeline_reorder_integration.rs`
  suites. No new test files for either of those existing suites.
- Soak period: at least one release with the flag off-by-default in
  CI but on in the dedicated matrix entry.

DPC-6 re-runs the bench (see DPC-2's plan for the bench shape and the
reference host bracket). DPC-7 owns the flip-vs-hold decision and is
bound by section 8's rollback criteria.

The flip criterion this design commits to (cited explicitly so DPC-7
cannot drift):

> **Default-on requires >= 5% throughput improvement at T = 16 with no
> regression worse than 5% at T in {1, 4}**, measured on the reference
> Mac Studio M2 Ultra host that DPC-2's bench plan names.

The 5% margin is tighter than the 20% the MPSC sketch
(`lockfree-mpsc-drain-design.md:185-196`) demanded because (a) the
SegQueue choice avoids the per-push allocation the MPSC sketch worried
about, and (b) the per-worker design is closer in structure to the
existing sharded mutex than MPSC is, so the migration cost is lower.

## 7. Cost Model

Per-push (steady state, T workers, each worker pushing to its own lane):

| Cost | Per-push value | Notes |
|---|---|---|
| Cache line touches | 1 (SegQueue tail) | Tail is owned by the pushing worker until the segment fills; cross-worker invalidation is amortised across 32 entries per segment |
| Atomic ops | 1 CAS on segment tail | `SegQueue::push` is a single CAS on the tail pointer; no Mutex acquire |
| Branch predictions | 2 mispredict-safe branches (segment full / not full) | The full path takes the allocator; the not-full path is the steady state |
| Allocator hits | 1 / 32 pushes (segment alloc) | Segments hold 32 entries; allocator is hit on segment rollover, not per push |

Compare to the Mutex baseline (`drain.rs:81`):

| Cost | Per-push value | Notes |
|---|---|---|
| Cache line touches | 2-3 (Mutex word + Vec header + Vec data tail) | All three are cross-worker contended when the rayon scheduler steals across shard boundaries |
| Atomic ops | 1 CAS on Mutex acquire + 1 store on release | Plus the futex syscall on contention |
| Branch predictions | 4+ (lock fast path, Vec capacity check, etc.) | Vec capacity check forks into the realloc path on growth |
| Allocator hits | Vec realloc on growth (1 / `capacity_doubling` pushes) | Comparable to SegQueue's segment alloc; not a differentiator |

Per-drain (T workers, N items total):

| Cost | Per-drain value (per-worker design) | Per-drain value (Mutex baseline) |
|---|---|---|
| Cache misses on barrier owner | T (one per lane head pointer) | T (one per shard Mutex header) |
| Allocator hits | `T + N / 32` | `T + log2(N)` per shard (realloc growth) |
| Lock acquires | 0 | N (one per `Vec::push`) |

Memory overhead per worker:

- `Arc<SegQueue<DrainEntry<R>>>` header: ~64 B (Arc count + pointer +
  SegQueue inner pointers).
- One pre-allocated empty segment: 32 * (8 B ndx + sizeof(R)) +
  segment header. For typical `R = DeltaWork`-derived types this is
  ~512-1024 B.
- Worst case per-worker overhead: **~1 KiB**, well below the
  "64-256 B" budget the brief allows (the brief's budget was for the
  Vec/handle alone; with the SegQueue's pre-allocated segment the
  upper bound shifts).

Total memory overhead at T = 64 workers, before any payload: ~64 KiB.
This is comparable to the Mutex baseline's `Vec<Mutex<Vec<R>>>` and is
not a budget concern.

## 8. Rollback Criteria

DPC-6 holds the flag OFF if any of the following triggers:

1. **Wire-byte parity test fails.** The parity test from section 5
   must pass on every CI run with `--features
   per-worker-drain-channels`. A single failure blocks the flip.
2. **Bench shows < 5% improvement at T = 16.** Below this margin the
   migration cost is not justified; the flag stays off and the
   Mutex baseline remains the default.
3. **Bench shows regression > 5% at T in {1, 4}.** The small-fanout
   case is the binding constraint; a workstation or single-user host
   spending more drain time on the new path is the worst-case
   outcome DPC-7 must avoid.
4. **Stress test reveals livelock or starvation.** The new
   `crates/engine/tests/per_worker_drain_parity.rs` shall include at
   least one stress shape (T = 64, N = 100_000, random `f` latency)
   that asserts every worker drains at least one entry and the total
   wall-clock stays within a multiplier (proposed 3x) of the median
   of the existing T = 4 / N = 10_000 fixture. Livelock surfaces as a
   wall-clock blowup.

If any of (1)-(4) trips, DPC-7 reports the trigger and the flag stays
off. The losing branch lives behind the flag for one more release
cycle and is removed only after the next release ships without
regression reports.

## 9. Cross-References

- [[project_drain_parallel_mutex_vec_contention]] - the memory note
  that motivates this work. Lists the contention shape as acknowledged
  but not yet shipped; DPC-3 through DPC-6 are the path to shipping it.
- [[project_apply_batch_write_serial]] - the related design point that
  the parallel verify / serial write split keeps the per-file Mutex
  in the apply pipeline. The drain Mutex this doc replaces is separate
  from that one; both are on the same critical path so DPC-6's bench
  numbers should be read together with the apply-batch numbers.
- `docs/design/lockfree-mpsc-drain-design.md` - prior art for the MPSC
  sketch. This design extends it along the per-worker-lane axis and
  picks SegQueue over MPSC for the per-push allocation reason in
  section 3.
- `crates/engine/src/concurrent_delta/work_queue/drain.rs:57-90` - the
  current production code.
- `crates/engine/src/concurrent_delta/work_queue/mod.rs:25-29` - the
  ordering contract this design preserves.
- `crates/engine/src/concurrent_delta/reorder/mod.rs:1-10` - the
  downstream sort step the drain feeds.
- `crates/transfer/benches/parallel_stat_collector_contention.rs:177-191`
  - the `SegQueue` arm of the existing collector bench; vetted prior
  use of the dependency this design selects.
- DPC-1 (#2846) - audit of the contention shape.
- DPC-2 (#2847) - bench plan that produces the baseline numbers DPC-6
  re-runs against.
- DPC-4 - rollback documentation, owns the runbook for flipping the
  flag back off after a release.
- DPC-5 (#2850) - implementation behind the flag.
- DPC-6 (#2851) - re-bench under the new path.
- DPC-7 - flip vs hold decision, bound by section 6's flip criterion
  and section 8's rollback criteria.
