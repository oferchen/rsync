# Fast-path bypass for --delete-before on small directories (DML-3)

Status: Design (task DML-3; part of the DML series tracking delete module
latency on small transfers)
Audience: engine, transfer, and receiver maintainers
Scope: introduce a fast-path that bypasses the cohort/reorder buffer
pipeline for small directory deletes (< 100 files), eliminating the
latency overhead that the parallel-deterministic-delete infrastructure
imposes on workloads where parallelism has no payoff.

## 1. Problem statement

### 1.1 Current architecture overhead

The parallel-deterministic-delete pipeline (PDD) uses a multi-stage
model for every `--delete-*` operation regardless of directory size:

1. Phase 1: `compute_extras` publishes a `DeletePlan` per directory into
   the shared `DeletePlanMap`.
2. The `DirTraversalCursor` yields directories in upstream depth-first
   `f_name_cmp`-ascending order.
3. The `ReorderBuffer` (BTreeMap-backed, cap = 64 cohorts) holds
   per-cohort `DeleteOperation` entries until sealed, then surfaces
   complete cohorts to the single-threaded `DeleteEmitter` in strict
   rank order.
4. The `CohortBatcher` wraps the reorder buffer with atomic
   enqueue/seal semantics and drain-batch grouping.

For a directory with 5-50 extraneous files, this machinery adds:

- **BTreeMap insert/lookup** per operation (O(log N) with N capped at 64
  but still non-trivial for a handful of entries).
- **Cohort seal + drain cycle** with rank-monotonicity validation.
- **PathBuf cloning** for `DeleteCohortKey` construction.
- **Vec allocation** per cohort for the ops buffer.
- **Two-map bookkeeping** (`by_key` + `cohorts` in `ReorderBuffer`).

Upstream rsync has none of this overhead. For `--delete-before`,
upstream's `do_delete_pass` (`generator.c:351-387`) walks the flist
sequentially and calls `delete_in_dir` directly - no buffering, no
reordering, no per-operation allocation beyond the stack-local `delbuf`.

### 1.2 Measured impact

The DML series (tracked by DML-1 through DML-6) identifies a latency
gap for small-transfer `--delete-before` operations:

- At < 100 files, the PDD pipeline adds measurable per-directory setup
  cost (cohort registration, buffer slot allocation, seal/drain cycle)
  that upstream avoids entirely.
- The `ReorderBuffer` + `CohortBatcher` overhead dominates when the
  actual unlink work is sub-millisecond per directory.
- The benefit of parallelism is zero at this scale: there is nothing to
  overlap, and the single-threaded emitter is never a bottleneck.

### 1.3 Goal

For transfers where the total extraneous file count across all
directories is below a threshold, bypass the PDD pipeline entirely and
issue unlinks directly in upstream traversal order. The fast path must:

- Match upstream rsync's observable delete behavior byte-for-byte
  (itemize order, `MSG_DELETED` framing, `NDX_DEL_STATS` counters).
- Avoid breaking the wire-ordering invariant that DEL-3 parity tests
  validate.
- Incur zero allocation overhead beyond what upstream's sequential model
  requires.
- Degrade gracefully if the initial size estimate proves wrong.

## 2. Threshold criteria

### 2.1 Primary threshold: per-directory extras count

The fast-path decision is made per directory, not per transfer. Each
directory's extras set (the output of `compute_extras`) is evaluated
independently:

```
FAST_PATH_EXTRAS_THRESHOLD: usize = 64
```

A directory with `extras.len() <= FAST_PATH_EXTRAS_THRESHOLD` takes the
fast path. A directory exceeding the threshold routes through the full
PDD pipeline.

Rationale for 64:

- Matches `PARALLEL_STAT_THRESHOLD` used elsewhere in the receiver for
  the sequential/parallel decision boundary.
- At 64 entries, the sequential unlink loop completes in under 200
  microseconds on ext4 (each `unlink(2)` takes approximately 2-4
  microseconds). The PDD pipeline's constant-factor setup cost
  (cohort registration, seal, drain) exceeds this at low counts.
- Upstream rsync handles all directory sizes with its sequential
  `delete_in_dir` loop. Our fast path mirrors this exactly for small
  directories.
- 64 is a power-of-two that aligns with the `MAX_BUFFERED_COHORTS`
  constant in the reorder buffer, avoiding confusion about which
  threshold applies where.

### 2.2 Secondary threshold: tree depth (not applied)

Tree depth is deliberately excluded as a threshold criterion. The
per-directory decision is orthogonal to tree depth because:

- Deep trees with small leaf directories should still use the fast path
  for each leaf.
- Shallow trees with large directories should still use the full PDD
  pipeline.
- Mixing fast-path and full-pipeline directories in the same transfer is
  safe because the `DirTraversalCursor` drives emission order regardless
  of which path each directory takes.

### 2.3 Secondary threshold: total transfer size (not applied)

The total file count across all directories is deliberately excluded.
The overhead is per-directory, not per-transfer. A transfer touching
1000 directories each with 3 extras benefits from the fast path on every
single directory, even though the aggregate count is 3000.

### 2.4 Thread count gate

The fast path is unconditionally preferred when
`rayon::current_num_threads() < 2`. On single-threaded configurations,
the PDD pipeline offers zero parallelism benefit, so the fast path is
always optimal regardless of directory size.

## 3. Fast-path implementation

### 3.1 Direct sequential delete

The fast path replaces the `ReorderBuffer` + `CohortBatcher` +
`DeleteEmitter::emit_all` chain with a direct inline loop that mirrors
upstream's `delete_in_dir`:

```rust
fn fast_path_delete_dir(
    plan: &DeletePlan,
    fs: &mut impl DeleteFs,
    stats: &mut DeleteStats,
    policy: &EmitterErrorPolicy,
    #[cfg(unix)] sandbox: Option<&DirSandbox>,
) -> io::Result<i32> {
    let mut io_error = 0i32;
    #[cfg(unix)]
    let parent_fd = sandbox.and_then(|s| open_plan_dirfd(s, &plan.directory));

    for entry in &plan.extras {
        let full = plan.directory.join(&entry.name);
        let result = dispatch_entry(
            entry.kind,
            &full,
            &entry.name,
            #[cfg(unix)] parent_fd.as_ref(),
            fs,
        );
        match result {
            Ok(()) => increment_stat(stats, entry.kind),
            Err(err) if is_fatal_error(&err) => return Err(err),
            Err(err) => {
                record_nonfatal(&err, &mut io_error, policy);
                if !policy.continue_on_error {
                    return Err(err);
                }
            }
        }
    }
    Ok(io_error)
}
```

Key properties:

- **No allocation.** The `DeletePlan` is consumed in place. No
  `DeleteOperation` wrappers, no `Vec<Vec<DeleteOperation>>` batches,
  no `DeleteCohortKey` path cloning.
- **No reorder buffer.** The plan's entries are already in upstream
  order (reverse `f_name_cmp` sort applied by `compute_extras`).
- **No seal/drain cycle.** The loop issues unlinks directly.
- **Same error policy.** Non-fatal errors set `io_error` bits; fatal
  errors abort. Identical to the `DeleteEmitter::run_entry` logic.
- **Same SEC-1.q sandbox support.** The dirfd-anchored `*_at` trait
  methods are used when a sandbox is attached, matching the emitter's
  security posture.

### 3.2 Integration point

The fast-path decision lives in `DeleteContext::emit_one` (for
`--delete-during`) and `DeleteContext::emit_all` (for `--delete-before`,
`--delete-after`, `--delete-delay`). The check happens after
`compute_extras` produces the `DeletePlan` but before the plan is
published into `DeletePlanMap`:

```rust
// In DeleteContext, after compute_extras produces plan:
if plan.extras.len() <= FAST_PATH_EXTRAS_THRESHOLD {
    // Fast path: delete inline, skip PDD pipeline entirely.
    let io_err = fast_path_delete_dir(&plan, &mut self.fs, ...)?;
    self.io_error |= io_err;
    // Do NOT publish to DeletePlanMap or register with ReorderBuffer.
} else {
    // Full PDD path: publish plan, let emitter drain later.
    self.plans.publish(dir.clone(), plan);
    self.cursor.observe_children(...);
}
```

### 3.3 DirTraversalCursor interaction

Directories handled by the fast path are never registered with the
`DirTraversalCursor`. This is safe because:

- The cursor only needs to track directories that have plans published
  in the `DeletePlanMap`.
- Fast-path directories are fully deleted before the cursor would
  iterate to them.
- The emitter's `emit_all` loop calls `cursor.next_ready()` and looks
  up plans via `plans.take(&dir)`. A directory that was never published
  is never yielded by the cursor and never looked up.

The traversal order invariant is maintained because the fast path
executes inline at the point where upstream's `delete_in_dir` would run.
The emission order is:

```
For each directory in upstream order:
  if extras <= threshold:
    fast_path_delete_dir (immediate, inline)
  else:
    publish plan -> emitter drains later in same order
```

Both paths produce the same observable ordering because the iteration
over directories follows `DirTraversalCursor` semantics in both cases.

## 4. Wire-ordering invariant preservation

### 4.1 Why the invariant holds

The wire-ordering invariant (DEL-3 parity) requires that:

1. `MSG_DELETED` path notifications appear in the same order as upstream.
2. `NDX_DEL_STATS` counters are numerically identical.
3. Within a single directory, entries are processed in reverse
   `f_name_cmp` order.

The fast path preserves all three because:

- **Property 1:** The fast path processes directories in the same order
  as the full pipeline (upstream traversal order). Within each directory,
  entries are in the same order (the `DeletePlan` sort is applied
  identically regardless of which execution path consumes it).
- **Property 2:** `increment_stat` uses the same per-kind counter logic.
- **Property 3:** The `DeletePlan::extras` slice is sorted identically
  by `compute_extras` whether the plan is consumed by the fast path or
  the emitter.

### 4.2 Mixed-mode transfers

A single transfer may have some directories on the fast path and others
on the full PDD pipeline. The wire-ordering invariant still holds
because:

- Fast-path directories emit their `MSG_DELETED` frames immediately
  and in order.
- Full-pipeline directories emit theirs later, but still in the same
  upstream-determined order relative to each other.
- The overall directory emission order matches upstream because the
  controlling iteration (the for-loop over all directories in traversal
  order) is identical in both cases.

### 4.3 --delete-during interleave

For `--delete-during`, upstream interleaves deletion with transfer
within each directory: `Delete(dir_A) -> Transfer(dir_A) ->
Delete(dir_B) -> Transfer(dir_B)`. The fast path naturally achieves
this interleave because it executes inline at the per-directory
decision point, before the transfer phase for that directory begins.

## 5. Fallback trigger: mid-operation promotion

### 5.1 When promotion is needed

The fast-path threshold is evaluated once per directory when the plan is
produced. No mid-directory promotion is needed because:

- `compute_extras` produces the complete extras set for a directory in
  one pass.
- The extras count is known before any unlink begins.
- The plan is immutable once constructed.

There is no scenario where a fast-path directory discovers mid-unlink
that it should have used the full pipeline.

### 5.2 Recursive directory discovery

One edge case: a fast-path directory contains a subdirectory that is
itself `ENOTEMPTY`. The `dispatch_dir` logic (rmdir -> ENOTEMPTY ->
recursive peel) may discover a nested directory with its own plan. This
is handled identically to the existing emitter:

- If the nested directory has a published plan in `DeletePlanMap`, drain
  it inline (matching `DeleteEmitter::dispatch_dir`'s existing logic).
- If not, fall back to `remove_dir_all` (matching upstream's
  `delete_dir_contents`).

The fast path does not consult `DeletePlanMap` for its own directory
(it was never published there), but it does consult it for nested
directories encountered during recursive peel. This matches the
existing `dispatch_dir` behavior.

### 5.3 INC_RECURSE segment boundaries

With INC_RECURSE, segments arrive incrementally and each segment may
describe additional child directories. The fast-path decision is made
per segment's per-directory output, not at segment boundaries. A
directory appearing in segment N is evaluated independently of
directories in segment N+1. No promotion or demotion between paths
occurs based on segment ordering.

## 6. Benchmark targets

### 6.1 Latency targets

The fast path must achieve parity with upstream rsync's delete latency
for small directories:

| Scenario | Upstream 3.4.1 | oc-rsync (full PDD) | oc-rsync (fast path) | Target |
|----------|----------------|---------------------|----------------------|--------|
| 10 files, 1 dir, flat | ~30 us | ~80-120 us | <= 40 us | <= 1.3x upstream |
| 50 files, 1 dir, flat | ~150 us | ~250-350 us | <= 200 us | <= 1.3x upstream |
| 100 files, 5 dirs, nested | ~400 us | ~700-900 us | <= 520 us | <= 1.3x upstream |

### 6.2 Overhead budget

The fast path's constant-factor overhead (beyond the unlink syscalls
themselves) must not exceed:

- **0 heap allocations** per directory beyond the `DeletePlan` itself
  (which is allocated by `compute_extras` regardless of path).
- **0 atomic operations** (no `ReorderBuffer` rank tracking, no
  `AtomicBool` panic latch).
- **0 PathBuf clones** for cohort key construction.
- **1 dirfd open** per directory (SEC-1.q sandbox, same as full path).

### 6.3 Benchmark validation

The DML-3 bench harness validates the fast path by comparing:

1. Full PDD pipeline on N-file directories (N = 1, 5, 10, 25, 50, 64).
2. Fast path on the same directories.
3. Upstream rsync on the same directories (via hyperfine wrapper).

The fast path passes when it is within 1.3x of upstream's wall-clock
time at every measured point. The full PDD pipeline serves as the
regression baseline - the fast path must always be faster than the full
pipeline at these scales.

## 7. Feature flag gating strategy

### 7.1 No new feature flag

The fast path does not introduce a new Cargo feature flag. Rationale:

- The fast path is a pure optimization with no behavioral divergence
  from the full pipeline. Both paths produce byte-identical wire output.
- Feature flags exist to gate incomplete or experimental code paths. The
  fast path is a complete, production-ready bypass for a well-understood
  hot path.
- Adding a feature flag for an internal optimization threshold creates
  combinatorial testing burden without user-facing benefit.

### 7.2 Compile-time constant

The threshold is a module-level `const`:

```rust
/// Maximum extras count for which the fast-path inline delete is used
/// instead of the full parallel-deterministic-delete pipeline.
///
/// Directories with `plan.extras.len() <= FAST_PATH_EXTRAS_THRESHOLD`
/// are deleted inline without publishing to `DeletePlanMap` or routing
/// through `ReorderBuffer` + `CohortBatcher`.
///
/// Value rationale: 64 matches `PARALLEL_STAT_THRESHOLD` and is the
/// crossover point where PDD pipeline setup cost exceeds the sequential
/// unlink loop cost on typical ext4/APFS filesystems.
const FAST_PATH_EXTRAS_THRESHOLD: usize = 64;
```

### 7.3 Interaction with parallel-delete-consumer feature

The `parallel-delete-consumer` feature gates the full parallel consumer
(DEL-2.c). The fast path operates upstream of that feature boundary:

- When `parallel-delete-consumer` is **off**: all directories route
  through the sequential `DeleteEmitter`. The fast path saves the
  BTreeMap/seal/drain overhead even in this sequential mode.
- When `parallel-delete-consumer` is **on**: small directories skip
  both the reorder buffer and the parallel consumer. Large directories
  route through the parallel consumer as before.

The fast path is always active regardless of the `parallel-delete-consumer`
feature state.

### 7.4 Runtime override (future)

If future benchmarks reveal that the threshold should be tunable (for
example, because NVMe-class storage shifts the crossover point), a
runtime environment variable can be added:

```
OC_RSYNC_DELETE_FAST_PATH_THRESHOLD=128
```

This is out of scope for DML-3 but the const-based design does not
preclude a future runtime override via `OnceLock<usize>`.

## 8. Implementation plan

### 8.1 Deliverables

| Task | Description | Depends on |
|------|-------------|------------|
| DML-3.a | Extract `dispatch_entry` and `record_nonfatal` as free functions from `DeleteEmitter` methods | None |
| DML-3.b | Implement `fast_path_delete_dir` using extracted functions | DML-3.a |
| DML-3.c | Wire fast-path check into `DeleteContext::emit_one` and `emit_all` | DML-3.b |
| DML-3.d | Unit tests: fast-path produces identical events to emitter for < 64 entries | DML-3.c |
| DML-3.e | Benchmark harness: compare fast-path vs full pipeline vs upstream at N = 1..64 | DML-3.c |
| DML-3.f | DEL-3.b parity tests pass with fast path active | DML-3.d |

### 8.2 Non-goals

- Changing the PDD pipeline's behavior for large directories.
- Adding user-visible flags or configuration.
- Modifying the `ReorderBuffer` or `CohortBatcher` implementations.
- Touching the `parallel-delete-consumer` feature flag.

## 9. Upstream reference

- `target/interop/upstream-src/rsync-3.4.1/generator.c:272-347`
  (`delete_in_dir`): the sequential per-directory loop the fast path
  mirrors.
- `target/interop/upstream-src/rsync-3.4.1/generator.c:351-387`
  (`do_delete_pass`): the pre-transfer walk for `--delete-before`.
- `target/interop/upstream-src/rsync-3.4.1/delete.c:130-225`
  (`delete_item`): per-entry dispatch by kind.

## 10. Cross-references

- PDD design: `docs/design/parallel-deterministic-delete.md`
- DEL-4.c threshold decision: `docs/design/del-4c-delete-threshold-decision.md`
- DML series tracker: `project_delete_module_latency.md`
- ReorderBuffer: `crates/engine/src/delete/reorder_buffer.rs`
- CohortBatcher: `crates/engine/src/delete/cohort_batcher.rs`
- DeleteContext: `crates/engine/src/delete/context/core.rs`
- DeleteEmitter: `crates/engine/src/delete/emitter/mod.rs`
