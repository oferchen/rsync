# DPC-5: Per-Worker Drain Channels Implementation Spec

Status: implementation spec. Implements the DPC-3 design
(`docs/design/per-worker-drain-channels.md`) behind a Cargo feature flag.
Rollback criteria are documented in DPC-4
(`docs/operations/drain-restructure-rollback.md`).

## 1. Summary of DPC-3 Design

The current `drain_parallel` body at
`crates/engine/src/concurrent_delta/work_queue/drain.rs:57-90` collects
results through `num_shards` mutex-guarded vectors keyed by
`rayon::current_thread_index()`. At T >= 16 workers, mutex contention on
cross-shard work-steal boundaries becomes the hottest sequence in the
drain path (DPC-1 audit).

DPC-3 replaces this with per-worker `crossbeam_queue::SegQueue` lanes -
one lane per rayon worker. Each worker pushes to its own lane with a
single atomic CAS (no mutex). After the `rayon::scope` barrier, the
calling thread drains all lanes sequentially into a flat `Vec<R>`. The
downstream `ReorderBuffer` then sorts results into wire order, identical
to the current flow.

Key design choices from DPC-3:

- **Primitive**: `crossbeam_queue::SegQueue<DrainEntry<R>>` - amortises
  allocation across 32-entry segments, no per-push node allocation, no
  mutex on the publish step. Already used in the codebase at
  `crates/transfer/benches/parallel_stat_collector_contention.rs:177-191`.
- **Lane registry**: `Vec<Arc<SegQueue<DrainEntry<R>>>>` of length
  `num_workers`, indexed by `rayon::current_thread_index()`. Non-rayon
  threads fall back to a hashed `ThreadId` modulo.
- **Sequence tag**: Each `DrainEntry<R>` carries the `DeltaWork::ndx()`
  value (`u32`). The tag enables future in-drain sorting but is not
  consumed by current callers - `ReorderBuffer` handles ordering.
- **Wire-ordering contract**: The per-worker drain produces a permutation
  of the same result set the mutex baseline produces. `ReorderBuffer`
  output is byte-identical.

## 2. Feature Flag

- **Name**: `per-worker-drain-channels`
- **Crate**: `engine`
- **Default**: OFF
- **Scope**: Gates the body of `drain_parallel` only. The public
  signature is identical with and without the flag. `drain_parallel_into`
  (the streaming variant) is untouched - it already uses
  `crossbeam_channel::Sender<R>` and has no `Mutex<Vec<R>>`.

Add to `crates/engine/Cargo.toml` under the Performance Optimization
Features section:

```toml
# DPC-5: Per-worker drain channels (#2850) - replaces the sharded
# Mutex<Vec<R>> collector in drain_parallel with per-worker SegQueue lanes.
# Removes cross-worker lock acquire from the per-item push path.
# Default off; DPC-6 re-benches, DPC-7 owns the flip-vs-hold decision.
per-worker-drain-channels = []
```

No dependency additions required - `crossbeam-queue` is already in
`engine`'s dependency list.

## 3. Files to Create

### 3.1 `crates/engine/src/concurrent_delta/work_queue/per_worker_drain.rs`

New module containing the per-worker drain types and logic. Only compiled
when the feature flag is active:

```rust
//! Per-worker drain lanes for lock-free result collection.
//!
//! Replaces the sharded `Mutex<Vec<R>>` collector in `drain_parallel`
//! with per-worker `SegQueue` lanes. Each rayon worker pushes to its
//! own lane (single atomic CAS, no mutex). After the `rayon::scope`
//! barrier, the calling thread drains all lanes into a flat `Vec<R>`.
//!
//! Gated behind the `per-worker-drain-channels` Cargo feature flag.
//! See `docs/design/per-worker-drain-channels.md` (DPC-3) for the
//! design rationale and cost model.

use crossbeam_queue::SegQueue;
use std::sync::Arc;

/// A result entry tagged with the originating file index.
///
/// The `ndx` tag enables the wire-ordering parity proof (DPC-3 section 5)
/// and any future in-drain sort variant. Current callers do not consume
/// the tag directly - `ReorderBuffer` handles ordering via the
/// `DeltaResult::sequence` field.
pub(super) struct DrainEntry<R> {
    pub ndx: u32,
    pub value: R,
}

/// Registry of per-worker drain lanes.
///
/// Each lane is a `SegQueue` shared via `Arc` so the `rayon::scope`
/// tasks can hold references without lifetime issues. Lane assignment
/// is implicit: `handle()` indexes by `rayon::current_thread_index()`
/// with a hashed `ThreadId` fallback for non-rayon threads.
pub(super) struct PerWorkerDrain<R: Send + 'static> {
    lanes: Vec<Arc<SegQueue<DrainEntry<R>>>>,
}

impl<R: Send + 'static> PerWorkerDrain<R> {
    /// Creates a drain with `num_workers` lanes.
    pub fn new(num_workers: usize) -> Self {
        let lanes = (0..num_workers)
            .map(|_| Arc::new(SegQueue::new()))
            .collect();
        Self { lanes }
    }

    /// Returns a handle pinned to the current thread's lane.
    ///
    /// Uses `rayon::current_thread_index()` when available, falling
    /// back to a hashed `ThreadId` modulo for threads outside the
    /// rayon pool. Mirrors the fallback at `drain.rs:73-80`.
    pub fn handle(&self) -> WorkerHandle<R> {
        let idx = rayon::current_thread_index().unwrap_or_else(|| {
            let id = std::thread::current().id();
            let mut hasher = std::hash::DefaultHasher::new();
            std::hash::Hash::hash(&id, &mut hasher);
            std::hash::Hasher::finish(&hasher) as usize
        }) % self.lanes.len();
        WorkerHandle {
            lane: Arc::clone(&self.lanes[idx]),
        }
    }

    /// Drains all lanes into a single `Vec<R>`, discarding the `ndx` tags.
    ///
    /// Called after the `rayon::scope` barrier when all workers have
    /// finished pushing. Drains lanes in stable index order
    /// (0..num_workers).
    pub fn drain_into_vec(self) -> Vec<R> {
        let mut out = Vec::new();
        for lane in self.lanes {
            while let Some(entry) = lane.pop() {
                out.push(entry.value);
            }
        }
        out
    }
}

/// Handle to a single worker's drain lane.
///
/// Cheap to create (one `Arc::clone`). Each `push` is a single atomic
/// CAS on the `SegQueue` tail - no mutex, no cross-worker coordination.
pub(super) struct WorkerHandle<R: Send + 'static> {
    lane: Arc<SegQueue<DrainEntry<R>>>,
}

impl<R: Send + 'static> WorkerHandle<R> {
    /// Pushes a result into the worker's lane.
    pub fn push(&self, ndx: u32, value: R) {
        self.lane.push(DrainEntry { ndx, value });
    }
}
```

### 3.2 `crates/engine/tests/per_worker_drain_parity.rs`

New integration test that runs the same fixtures through both the mutex
baseline and the per-worker drain, asserting post-`ReorderBuffer` byte
parity. Only compiled with `--features per-worker-drain-channels`.

See section 8 for the full test strategy.

## 4. Files to Modify

### 4.1 `crates/engine/Cargo.toml`

- Add `per-worker-drain-channels = []` to `[features]`.

### 4.2 `crates/engine/src/concurrent_delta/work_queue/mod.rs`

- Add conditional module declaration:

  ```rust
  #[cfg(feature = "per-worker-drain-channels")]
  mod per_worker_drain;
  ```

### 4.3 `crates/engine/src/concurrent_delta/work_queue/drain.rs`

- Add `cfg`-gated import of `per_worker_drain::PerWorkerDrain`.
- Add second `drain_parallel` body gated behind the feature flag.
- Retain the existing body under `#[cfg(not(feature = "per-worker-drain-channels"))]`.

The dispatch pattern:

```rust
#[cfg(feature = "per-worker-drain-channels")]
use super::per_worker_drain::PerWorkerDrain;

impl WorkQueueReceiver {
    #[cfg(not(feature = "per-worker-drain-channels"))]
    pub fn drain_parallel<F, R>(self, f: F) -> Vec<R>
    where
        F: Fn(DeltaWork) -> R + Send + Sync,
        R: Send,
    {
        // Existing mutex-sharded body (unchanged).
        let num_shards = rayon::current_num_threads();
        let shards: Vec<std::sync::Mutex<Vec<R>>> = (0..num_shards)
            .map(|_| std::sync::Mutex::new(Vec::new()))
            .collect();

        rayon::scope(|s| {
            for work in self.into_iter() {
                let f = &f;
                let shards = &shards;
                s.spawn(move |_| {
                    let result = f(work);
                    let idx = rayon::current_thread_index().unwrap_or_else(|| {
                        let id = std::thread::current().id();
                        let mut hasher = std::hash::DefaultHasher::new();
                        std::hash::Hash::hash(&id, &mut hasher);
                        std::hash::Hasher::finish(&hasher) as usize
                    });
                    shards[idx % num_shards].lock().unwrap().push(result);
                });
            }
        });

        shards
            .into_iter()
            .flat_map(|shard| shard.into_inner().unwrap())
            .collect()
    }

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

    // drain_parallel_into is unchanged - no cfg gate needed.
}
```

**Note on the `R: Send + 'static` bound**: The per-worker variant
requires `R: 'static` because `Arc<SegQueue<DrainEntry<R>>>` stores `R`
behind an `Arc`. The mutex baseline only requires `R: Send`. This is
a backwards-compatible relaxation for the feature-gated path only -
callers under the default (flag off) path retain the original bound.
All production callers already satisfy `'static` (`DeltaResult` is
`'static`), so no call-site changes are needed.

### 4.4 CI configuration (workflow YAML)

Add a matrix entry to the existing nextest workflow that builds and tests
the `engine` crate with `--features per-worker-drain-channels`. This
entry runs:

- The new `per_worker_drain_parity` integration test.
- The existing `multi_producer_work_queue` and
  `pipeline_reorder_integration` test suites.
- The existing `work_queue::tests` unit tests.

The entry is non-required (advisory) during the soak period. It becomes
required when DPC-7 flips the default.

## 5. Per-Worker Channel Type

**Selected**: `crossbeam_queue::SegQueue<DrainEntry<R>>` (lock-free,
segment-amortised).

Rationale (from DPC-3 section 3):

| Property | SegQueue | crossbeam unbounded MPSC | Thread-local Vec |
|---|---|---|---|
| Per-push alloc | 1/32 (segment) | 1 (node) | 0 |
| Per-push sync | 1 atomic CAS | 1 atomic CAS | 0 (TLS) |
| Publish step | None (intrinsically Sync) | Channel already Sync | Requires Mutex |
| Existing crate dep | Yes | Yes | No new dep, but fragile TLS |

SegQueue wins on the per-push allocation axis (32x fewer allocator
hits than MPSC) while avoiding the publish-step Mutex that the
thread-local Vec approach requires. The segment size (32 entries) is
a crossbeam implementation constant - not configurable, which is
acceptable since 32 entries per allocation is well within the memory
budget DPC-3 section 7 validates.

**Not using bounded channels**: The drain operates within a
`rayon::scope` that provides a natural barrier. Backpressure comes from
the upstream bounded `WorkQueue`, not from the drain's collector. An
unbounded lane per worker is correct here - the total item count is
bounded by the `WorkQueue` capacity times the number of scope
iterations.

## 6. Merge-at-Barrier Implementation

The merge step runs on the thread that called `drain_parallel` -
the same thread that owns the `rayon::scope` call. After the scope
returns (the barrier), all workers have completed and no further pushes
can occur.

Implementation of `PerWorkerDrain::drain_into_vec`:

1. Iterate over `self.lanes` in stable index order (0 through
   `num_workers - 1`).
2. For each lane, call `lane.pop()` in a loop until `None`, pushing
   each `entry.value` into the output `Vec<R>`.
3. The `DrainEntry::ndx` tag is discarded at this step. It exists for
   the parity proof and future in-drain sorting - current callers rely
   on `ReorderBuffer` for ordering.

The output `Vec<R>` is a permutation of the result set. Its internal
order is deterministic for a fixed worker-to-lane assignment and fixed
task completion order, but not guaranteed across runs. This matches the
current mutex baseline's semantics exactly.

**Complexity**: O(N) where N is total items, plus O(T) for the lane
iteration where T is worker count. No sorting, no merge-sort across
lanes. The downstream `ReorderBuffer` handles ordering in O(N) via
ring-buffer insertion.

## 7. cfg-Gated Dispatch

The dispatch uses mutually exclusive `#[cfg]` attributes on two
`drain_parallel` method definitions within the same `impl WorkQueueReceiver`
block. This avoids runtime branching and ensures the dead path is not
compiled.

```rust
#[cfg(not(feature = "per-worker-drain-channels"))]
pub fn drain_parallel<F, R>(self, f: F) -> Vec<R> { /* mutex baseline */ }

#[cfg(feature = "per-worker-drain-channels")]
pub fn drain_parallel<F, R>(self, f: F) -> Vec<R> { /* per-worker lanes */ }
```

Both bodies share the same public signature (modulo the `R: 'static`
bound addition in the per-worker variant). The `drain_parallel_into`
method is not gated - it is unchanged in both configurations.

## 8. Migration Path for Existing Callers

### 8.1 Production caller

`crates/transfer/src/delta_pipeline/parallel.rs` - uses
`drain_parallel_into` (the streaming variant), not `drain_parallel`.
No changes required.

### 8.2 Test callers

The following test files call `drain_parallel` directly:

- `crates/engine/tests/multi_producer_work_queue.rs` - lines 71, 142,
  210, 270, 327, 435.
- `crates/engine/tests/pipeline_reorder_integration.rs` - line 247.

All these callers return types that already satisfy `R: Send + 'static`
(`u32`, `(u32, u64)`, `(u32, bool, u64)`, `DeltaResult`). No call-site
changes are needed. These tests compile and run identically under both
flag states.

### 8.3 Bench callers

- `crates/engine/benches/drain_parallel_benchmark.rs` - returns `u64`.
  Satisfies `'static`. No changes needed.
- `crates/engine/benches/drain_parallel_alternatives.rs` - uses the
  sharded mutex directly (not through `drain_parallel`). Unaffected.

### 8.4 Migration summary

No existing call sites require modification. The feature flag is purely
an internal implementation swap with identical public API.

## 9. Rollback Procedure (per DPC-4)

DPC-4 (`docs/operations/drain-restructure-rollback.md`) defines four
rollback triggers and a five-step procedure. The implementation must
support rollback at every stage:

### 9.1 Before default flip (flag OFF)

Rollback is trivial: the feature is opt-in. Users who enabled it
explicitly disable it by removing `per-worker-drain-channels` from their
`--features` list. No code revert needed.

### 9.2 After default flip (DPC-7)

If any DPC-4 trigger fires after DPC-7 adds the flag to `default = [...]`:

1. Revert the DPC-7 commit that added the flag to the default list.
   This is a one-line change in `crates/engine/Cargo.toml`.
2. The `per_worker_drain.rs` module and the `per_worker_drain_parity.rs`
   test remain in-tree, compilable under the explicit flag. Opt-in users
   with measured local wins keep access.
3. Follow DPC-4 sections 5-7 for the permanent revert PR, communication
   template, and post-rollback investigation.

### 9.3 Code-level rollback isolation

The `#[cfg]`-gated dispatch ensures the mutex baseline is always
compilable and tested. The CI matrix entry for the flag-off build
validates the baseline on every PR. Removing the flag from defaults
restores the baseline with zero code changes beyond the `Cargo.toml`
edit.

## 10. Test Strategy

### 10.1 Parity test: `per_worker_drain_parity.rs`

New integration test, gated by `required-features = ["per-worker-drain-channels"]`.

**Purpose**: Prove that the per-worker drain and the mutex baseline
produce identical post-`ReorderBuffer` output for the same input
sequence.

**Shape**:

- Worker counts: `[1, 4, 8, 16, 64]`. T = 64 exercises the hashed
  `ThreadId` fallback (rayon pools typically cap below this on CI hosts).
- Item counts: `[100, 10_000, 100_000]`. The smallest is for
  determinism debugging; the largest reflects the production hot path.
- For each (T, N) pair:
  1. Construct N `DeltaWork` items with sequential NDX and sequence
     values.
  2. Run through `drain_parallel` on a private rayon pool of size T.
     Each closure applies a variable-spin delay keyed on NDX to induce
     reordering.
  3. Feed the resulting `Vec<DeltaResult>` through a `ReorderBuffer`.
  4. Assert the post-reorder sequence is `[0, 1, 2, ..., N-1]`.

Since the parity test runs under the `per-worker-drain-channels` feature
flag, it exercises the new per-worker path. The existing
`pipeline_reorder_integration::batch_drain_parallel_with_reorder_buffer`
test covers the mutex baseline under the default build. Together they
prove both paths produce identical ordered output.

### 10.2 Stress test: livelock/starvation detection

Included in `per_worker_drain_parity.rs` as a separate `#[test]`.

**Shape**: T = 64, N = 100_000, each worker closure sleeps a random
duration (0-100 microseconds). Assert:

- All N items appear in the output (no lost items).
- Total wall-clock stays within 3x of a baseline run at T = 4, N = 10_000
  (adjusted for the 10x item count difference).
- At least `min(T, N)` distinct lanes received at least one push (no
  lane starvation). Verified by adding a debug counter per lane or
  checking that `SegQueue::is_empty()` returns false before drain for
  at least T distinct lanes (when N >= T).

### 10.3 Single-worker test

A dedicated `#[test]` in `per_worker_drain_parity.rs` with T = 1,
N = 100. Verifies correct behavior when there is only one lane and all
items flow through it. The output `Vec<R>` should contain all N items;
after `ReorderBuffer`, the sequence is `[0..N)`.

### 10.4 Existing test suites

The following suites must pass under both flag states:

- `crates/engine/tests/multi_producer_work_queue.rs` - exercises
  `drain_parallel` with multiple producers.
- `crates/engine/tests/pipeline_reorder_integration.rs` - end-to-end
  pipeline with `ReorderBuffer`.
- `crates/engine/src/concurrent_delta/work_queue/tests.rs` - unit tests
  for the work queue.

The CI matrix entry for `per-worker-drain-channels` runs all three.

### 10.5 Barrier merge ordering test

A `#[test]` verifying that `drain_into_vec` drains lanes in stable index
order (0..num_workers). Set up a `PerWorkerDrain` with 4 lanes, push
known tagged values into specific lanes, call `drain_into_vec`, and
assert the output order matches lane-0 items first, then lane-1, etc.
This test is internal to the `per_worker_drain` module (a `#[cfg(test)]`
submodule).

## 11. Bench Integration Points (DPC-6)

DPC-6 re-runs the contention benchmark under the per-worker drain path.
The following integration points enable this:

### 11.1 Existing benchmark

`crates/engine/benches/drain_parallel_benchmark.rs` already measures
`drain_parallel` throughput at T in {1, 4, 8, 16} and N in
{10K, 100K}. When built with `--features per-worker-drain-channels`, it
automatically exercises the new path (same public API, cfg-gated body).

DPC-6 runs this benchmark twice - once without the flag (baseline) and
once with - and compares the results. No changes to the benchmark file
are needed.

### 11.2 Alternatives benchmark

`crates/engine/benches/drain_parallel_alternatives.rs` benchmarks the
sharded-mutex, per-thread-vec, and MPSC strategies side by side. DPC-6
may add a fourth arm (`per_worker_segqueue`) that calls
`PerWorkerDrain` directly to get isolated numbers outside the
`drain_parallel` wrapper. This is optional - the primary signal comes
from the `drain_parallel_benchmark` comparison.

### 11.3 CI bench cell

The DPC-6 bench cell runs on the reference Mac Studio M2 Ultra host that
DPC-2 names. It captures:

- Throughput (items/sec) at T in {1, 4, 8, 16} and N in {10K, 100K}.
- Wall-clock per drain at T = 16, N = 100K (the target workload).
- Memory high-water (peak RSS delta) at N = 100K.

The flip criterion from DPC-3 section 6:

> Default-on requires >= 5% throughput improvement at T = 16 with no
> regression worse than 5% at T in {1, 4}.

## 12. Cross-References

- `docs/design/per-worker-drain-channels.md` - DPC-3 design (parent).
- `docs/operations/drain-restructure-rollback.md` - DPC-4 rollback
  runbook.
- `docs/design/lockfree-mpsc-drain-design.md` - prior-art MPSC sketch.
- `crates/engine/src/concurrent_delta/work_queue/drain.rs` - production
  code being modified.
- `crates/engine/src/concurrent_delta/work_queue/mod.rs` - module
  declaration site.
- `crates/engine/src/concurrent_delta/reorder/mod.rs` - downstream
  `ReorderBuffer` that restores wire order.
- `crates/engine/tests/pipeline_reorder_integration.rs` - existing
  parity reference.
- `crates/engine/benches/drain_parallel_benchmark.rs` - contention
  benchmark.
- `crates/transfer/benches/parallel_stat_collector_contention.rs` -
  existing `SegQueue` usage in the codebase.
- DPC-6 (#2851) - re-bench under the new path.
- DPC-7 - flip-vs-hold decision.
