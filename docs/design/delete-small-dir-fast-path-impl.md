# Small-directory fast path for --delete-before (DML-4 implementation)

Status: Implementation design (task DML-4, #3309)
Audience: engine, transfer maintainers
Scope: code-level plan for a fast path that bypasses the cohort/reorder
pipeline when the total extras count for a --delete-before invocation
falls below a threshold. Applies to all --delete-* timing modes but the
primary latency target is --delete-before where the full delete pass
blocks transfer start.

Depends on: DML-3 design analysis (threshold justification), DEL-4.c
bench data (section 3.2 threshold-gated design).

## 1. Problem recap

The parallel-deterministic-delete pipeline
(`crates/engine/src/delete/`) introduces constant-factor overhead per
directory: `CohortBatcher` allocation, `ReorderBuffer` slot management,
`DirTraversalCursor` frame stack, and the `Condvar`-driven wake-up loop.
For small transfers (< 64 extras across all directories) this overhead
dominates actual unlink time, making oc-rsync measurably slower than
upstream rsync's simple linear `delete_in_dir` loop.

Upstream rsync processes each directory inline: scan, subtract, unlink.
No buffering, no reordering, no parallelism. For small directory trees
this is optimal because the syscall cost of a few `unlink(2)` calls is
dwarfed by the pipeline bookkeeping.

## 2. Design decision: always-on (no feature flag)

The fast path is always-on, not gated behind a Cargo feature flag.
Rationale:

1. The bypass condition is a pure runtime count check - no new
   compilation branches, no conditional dependencies.
2. The sequential `DeleteEmitter` is already compiled in all builds
   (the `parallel-delete-consumer` feature only adds the parallel
   path, never removes the sequential one).
3. A feature flag would require maintaining two integration test
   configurations for what is a single `if` branch.
4. The fast path produces byte-identical output to the existing
   sequential emitter because it IS the sequential emitter - just
   invoked without the cohort/reorder wrapper.

## 3. Threshold constant

```rust
/// Maximum total extras count across all directories at which the
/// delete pipeline bypasses CohortBatcher/ReorderBuffer and dispatches
/// directly through the sequential DeleteEmitter.
///
/// Value rationale: matches `DEFAULT_DELETION_THRESHOLD` (64) used
/// elsewhere in the codebase for the sequential/parallel crossover.
/// At 64 files, unlink cost is ~192 us (64 * 3 us/unlink on ext4),
/// while the cohort pipeline setup cost is ~50 us for Condvar +
/// slot allocation. The crossover where pipeline overhead drops below
/// 10% of total work is approximately 500 entries; 64 gives a
/// conservative margin where the fast path is unambiguously cheaper.
pub const SMALL_DIR_FAST_PATH_THRESHOLD: usize = 64;
```

Location: `crates/engine/src/delete/context/core.rs` alongside the
existing `emit_one`/`emit_all` dispatch logic.

## 4. Detection point

The extras count is known after all `observe_segment_for_delete` calls
complete and before the drain begins. The natural detection point is
inside `DeleteContext::into_drain_parts`, which already extracts the
owned `DeletePlanMap`. The plan map exposes a total entry count via
iteration over its published plans.

However, `into_drain_parts` is shared infrastructure consumed by both
the sequential and parallel paths. The threshold check belongs one level
up, in the public `emit_one`/`emit_all` methods, which is where the
current `#[cfg(feature = "parallel-delete-consumer")]` dispatch already
lives.

### 4.1 Counting strategy

After `into_drain_parts` extracts the owned `DeletePlanMap`, sum the
`extras.len()` of every plan:

```rust
let total_extras: usize = plans.iter().map(|p| p.extras.len()).sum();
```

`DeletePlanMap` already supports iteration (it wraps a `DashMap` whose
`iter()` yields immutable references). The sum is O(D) where D is the
number of directories with plans - negligible overhead.

### 4.2 Alternative considered: pre-count during observation

An atomic counter incremented inside `observe_segment_for_delete` would
avoid the post-hoc iteration. Rejected because:

- Adds a `AtomicUsize` field to `DeleteContext` that is only read once.
- The iteration cost is bounded by the number of directories (typically
  < 100 for small transfers) and runs once per transfer.
- Keeping the count derivable from the plan map avoids a divergence bug
  where the counter and the map disagree.

## 5. Bypass implementation

### 5.1 Files to modify

| File | Change |
|------|--------|
| `crates/engine/src/delete/context/core.rs` | Add threshold constant; modify `emit_one`/`emit_all` to branch on total extras count |
| `crates/engine/src/delete/plan_map.rs` | Add `total_extras_count(&self) -> usize` method if not already derivable from existing API |

No other files need modification. The fast path reuses the existing
`DeleteEmitter` (sequential) and `DirTraversalCursor` unchanged.

### 5.2 Dispatch logic (parallel-delete-consumer enabled)

Current code in `core.rs` (simplified):

```rust
#[cfg(feature = "parallel-delete-consumer")]
pub fn emit_all<F: DeleteFs + Sync + Send + 'static>(self, fs: F) -> io::Result<DrainOutcome<F>> {
    self.emit_via_parallel_consumer(fs)
}
```

Modified:

```rust
#[cfg(feature = "parallel-delete-consumer")]
pub fn emit_all<F: DeleteFs + Sync + Send + 'static>(self, fs: F) -> io::Result<DrainOutcome<F>> {
    let (plans, cursor, policy) = self.into_drain_parts().map_err(io::Error::from)?;
    let total_extras: usize = plans.total_extras_count();

    if total_extras < SMALL_DIR_FAST_PATH_THRESHOLD
        || rayon::current_num_threads() < 2
    {
        // Fast path: bypass cohort/reorder, dispatch sequentially.
        let mut emitter = DeleteEmitter::with_policy(fs, plans, cursor, policy);
        emitter.emit_all()?;
        Ok(DrainOutcome::from_emitter(emitter))
    } else {
        // Standard parallel path via CohortBatcher + ReorderBuffer.
        Self::emit_parallel_from_parts(plans, cursor, policy, fs)
    }
}
```

The same pattern applies to `emit_one` (which delegates to `emit_all`
for the parallel feature).

### 5.3 Refactoring emit_via_parallel_consumer

The current `emit_via_parallel_consumer` calls `self.into_drain_parts()`
internally. With the fast path, `into_drain_parts` must be called before
the threshold check (to get the plan map for counting). The parallel
dispatch logic moves to a new associated function that takes owned parts:

```rust
fn emit_parallel_from_parts<F: DeleteFs + Sync + Send + 'static>(
    plans: DeletePlanMap,
    mut cursor: DirTraversalCursor,
    policy: EmitterErrorPolicy,
    fs: F,
) -> io::Result<DrainOutcome<F>> {
    // ... existing ParallelDeleteEmitter logic, unchanged ...
}
```

This avoids consuming `self` twice and keeps the parallel path isolated.

### 5.4 Non-parallel builds (feature disabled)

When `parallel-delete-consumer` is not enabled, the code already routes
through the sequential `DeleteEmitter`. No threshold check is needed -
there is no cohort/reorder overhead to bypass. The fast path is a no-op
in this configuration.

## 6. Wire-byte parity guarantee

The fast path produces identical wire output because:

1. **Same emitter.** The fast path uses `DeleteEmitter::emit_all` - the
   exact same code path as the non-parallel build.
2. **Same traversal order.** The `DirTraversalCursor` determines
   emission order. Both paths consume the same cursor built from the
   same observations.
3. **Same plan data.** `DeletePlan::sort_by_name` is called during
   `observe_segment_for_delete`, before the drain. The fast path
   receives identically-sorted plans.
4. **Same stats accumulation.** `DeleteEmitter` increments `DeleteStats`
   per entry, per kind. The NDX_DEL_STATS frame is generated from these
   counters downstream.
5. **Same itemize lines.** The `DeleteFs` trait methods are called with
   the same paths in the same order. MSG_DELETED notifications match
   byte-for-byte.

### 6.1 Verification strategy

The existing DEL-3.b wire-byte parity test suite covers this by
construction: when the test fixture has < 64 extras, the fast path
fires, and the golden capture must still match. No new parity tests
are needed - only a unit test confirming the threshold routing (fast
path taken vs not taken).

## 7. NDX_DEL_STATS correctness

The `NDX_DEL_STATS` goodbye-phase frame carries five varints
(files, dirs, symlinks, devices, specials) accumulated by the emitter.
The fast path uses `DeleteEmitter` which owns and populates
`DeleteStats` identically to the non-parallel path. The downstream
generator-side writer (`write_del_stats`) reads the same struct
regardless of which dispatch path produced it.

No change to `NDX_DEL_STATS` generation or wire encoding is needed.

## 8. Edge cases

### 8.1 Zero extras

When `total_extras == 0`, the fast path fires (0 < 64). The sequential
emitter walks the cursor, finds no plans with entries, and returns
immediately. This is a slight improvement over the parallel path which
would still allocate and tear down the `ParallelDeleteEmitter`
infrastructure.

### 8.2 Single directory with many extras

A single directory with 100 extras (total = 100 >= 64) routes through
the parallel path. Even though there is only one cohort, the parallel
consumer's intra-cohort `par_iter` still provides value by dispatching
100 unlinks across rayon workers.

### 8.3 Many directories each with few extras

50 directories each with 1 extra (total = 50 < 64) routes through the
fast path. The sequential emitter walks all 50 directories in cursor
order and issues 50 unlinks serially. At 50 unlinks (~150 us) the
parallel overhead (~50 us setup + ~10 us per cohort slot) would not
provide meaningful speedup.

### 8.4 Threshold boundary

At exactly 64 extras, the condition `total_extras < 64` is false, so the
parallel path runs. The threshold is strict-less-than to match the
`DEFAULT_DELETION_THRESHOLD` semantics used elsewhere in the codebase.

### 8.5 rayon thread count < 2

When `rayon::current_num_threads() < 2`, the fast path fires regardless
of extras count. This matches DEL-4.c section 3.3: on single-core
machines the parallel pipeline adds overhead with zero throughput
benefit.

## 9. Testing plan

### 9.1 Unit test: threshold routing

Add a test in `crates/engine/src/delete/context/tests.rs` that:

1. Constructs a `DeleteContext` with N plans totaling < 64 extras.
2. Calls `emit_all` with a `RecordingDeleteFs`.
3. Asserts events are emitted in cursor order (proving the sequential
   emitter ran, not the parallel consumer).
4. Repeats with N plans totaling >= 64 extras.
5. Asserts events are identical (same order, same content).

### 9.2 Integration test: wire parity at boundary

The existing DEL-3.b parity test should include at least one fixture
with exactly 63 extras (fast path) and one with exactly 65 extras
(parallel path) to confirm the boundary does not introduce divergence.

### 9.3 Benchmark: latency reduction

A targeted benchmark in `crates/engine/benches/` that measures
`--delete-before` latency for a 10-directory, 5-extras-per-directory
fixture (total = 50 < 64). Compare current (always-parallel) vs fast
path (sequential bypass). Expected improvement: 30-50% latency
reduction for small transfers.

## 10. Implementation steps

1. Add `total_extras_count()` to `DeletePlanMap` (trivial iterator sum).
2. Add `SMALL_DIR_FAST_PATH_THRESHOLD` constant to `context/core.rs`.
3. Refactor `emit_via_parallel_consumer` into `emit_parallel_from_parts`
   (takes owned parts, no `self`).
4. Modify `emit_one` and `emit_all` (parallel feature variant) to call
   `into_drain_parts` first, check total extras, and branch.
5. Add unit test for threshold routing.
6. Add boundary fixture to DEL-3.b parity test (63 vs 65 extras).
7. Add targeted latency benchmark.

Estimated diff: ~80 lines of production code, ~60 lines of tests.

## 11. Cross-references

- DML-3 design (threshold justification): this document
- DEL-4.c threshold-gated decision: `docs/design/del-4c-delete-threshold-decision.md`
- Delete module architecture: `crates/engine/src/delete/mod.rs`
- Sequential emitter: `crates/engine/src/delete/emitter/mod.rs`
- Parallel consumer: `crates/engine/src/delete/parallel_consumer.rs`
- Cohort batcher: `crates/engine/src/delete/cohort_batcher.rs`
- Reorder buffer: `crates/engine/src/delete/reorder_buffer.rs`
- Context dispatch: `crates/engine/src/delete/context/core.rs`
- Plan map: `crates/engine/src/delete/plan_map.rs`
- Traversal cursor: `crates/engine/src/delete/traversal.rs`
- DEFAULT_DELETION_THRESHOLD: `crates/transfer/src/parallel_io.rs:33`
