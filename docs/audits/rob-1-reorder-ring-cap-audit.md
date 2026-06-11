# ROB-1 — Audit: ReorderBuffer ring-cap formula and call sites

Parent series: ROB (ReorderBuffer normal-operation spill prevention).
Date: 2026-06-11. Status: greenfield audit, no code change.

This audit inventories the ring-cap formulas, the production call sites that
construct a `ReorderBuffer` or `SpillableReorderBuffer`, and the workload
classes that flow through each site. It feeds forward into ROB-2 (spill
activations counter), ROB-3 (one-shot spill warning), ROB-4 (already shipped
pressure-paths audit), and ROB-7 (adaptive ring sizing spec).

## Scope clarification

The ROB parent task talks about "ReorderBuffer normal-operation spill
prevention". Three distinct reorder buffers exist in the workspace; only one
is in scope for ROB:

1. `engine::concurrent_delta::reorder::ReorderBuffer<T>` — the pre-allocated
   ring used by the parallel delta pipeline, wrapped optionally by
   `SpillableReorderBuffer`. **In scope.**
2. `engine::concurrent_delta::parallel_apply::FileSlot::reorder` — a per-file
   `ReorderBuffer<DeltaChunk>` instance held inside the parallel applier's
   `DashMap`. Same type as (1) but with its own capacity knob. **In scope.**
3. `engine::delete::reorder_buffer::ReorderBuffer` — a delete-cohort
   re-ordering map keyed by `(parent_dir, rank)`. Compile-time cap
   `MAX_BUFFERED_COHORTS = 64`. Different data structure (BTreeMap-of-cohorts,
   not a ring). **Out of scope.**
4. `transfer::reorder_buffer::BoundedReorderBuffer<T>` — a `BTreeMap`-backed
   window-bounded reorder buffer with `DEFAULT_WINDOW_SIZE = 64`. Used by the
   transfer crate's pipeline layer; cannot spill. Listed for completeness so
   ROB-7 can decide whether the adaptive policy should extend here.
   **Adjacent, not the focus of ROB but tracked below.**

ROB-4's audit (`docs/audits/rob-4-reorder-pressure-paths.md`, shipped) already
mapped which transfer paths feed sequence-numbered work into (1). This audit
focuses on the cap formula and call sites that set capacity.

## 1. Current ring-cap formula(s)

### 1.1 `ReorderBuffer<T>` (engine concurrent delta)

`crates/engine/src/concurrent_delta/reorder/mod.rs:199-220`:

```rust
pub fn new(capacity: usize) -> Self {
    assert!(capacity > 0, "reorder buffer capacity must be non-zero");
    let slots: Vec<Option<T>> = (0..capacity).map(|_| None).collect();
    Self {
        slots: slots.into_boxed_slice(),
        head: 0,
        next_expected: 0,
        count: 0,
        capacity,
        ...
    }
}
```

The constructor takes `capacity` verbatim. There is **no internal heuristic** -
every call site supplies its own value. Internal headroom comes only from the
optional `AdaptiveCapacityPolicy` composed via `with_adaptive_policy()`
(`crates/engine/src/concurrent_delta/adaptive.rs`). The adaptive path is
**not** wired into any production call site at present; only tests and benches
exercise it.

### 1.2 `SpillableReorderBuffer<T>`

`crates/engine/src/concurrent_delta/spill/buffer/lifecycle.rs:29-50`:

```rust
pub fn new(capacity: usize, threshold: usize) -> Self {
    Self {
        inner: ReorderBuffer::new(capacity),
        memory_used: 0,
        threshold,
        ...
    }
}
```

Same shape - `capacity` is the in-memory ring cap, `threshold` is the byte
cap that triggers spill-to-tempfile. Cap is passed in by the caller.

### 1.3 Capacity is sourced upstream from two places

The capacity that finally lands in the constructor is computed at the parallel
pipeline entry point. There are two formulas in production today:

**Formula A — `transfer::delta_pipeline::parallel`** (the production receive-
delta pipeline; `crates/transfer/src/delta_pipeline/parallel.rs:77-100, 146-159`):

```rust
pub fn new(worker_count: usize) -> Self {
    let capacity = worker_count.saturating_mul(2).max(2);
    Self::with_capacity(capacity)
}

pub fn new_adaptive(worker_count: usize, avg_target_size: u64) -> Self {
    let capacity = adaptive_capacity(worker_count, avg_target_size);
    Self::with_capacity(capacity)
}

pub(super) fn adaptive_capacity(worker_count: usize, avg_target_size: u64) -> usize {
    const SMALL_FILE_THRESHOLD: u64 = 64 * 1024;
    const LARGE_FILE_THRESHOLD: u64 = 1024 * 1024;
    let multiplier: usize = if avg_target_size == 0 {
        2
    } else if avg_target_size < SMALL_FILE_THRESHOLD {
        8
    } else if avg_target_size > LARGE_FILE_THRESHOLD {
        2
    } else {
        4
    };
    worker_count.saturating_mul(multiplier).max(2)
}

fn with_capacity(capacity: usize) -> Self {
    let (work_tx, work_rx) = work_queue::bounded_with_capacity(capacity);
    let consumer = DeltaConsumer::spawn(work_rx, capacity);
    ...
}
```

The same `capacity` value is used for both the bounded work queue and the
reorder ring. Net effect under the default `new()`:
`capacity = 2 * worker_count` (clamped to `>= 2`).
Under `new_adaptive()` with a known average file size: `2..8 * worker_count`.

Formula A mirrors the engine's `work_queue::adaptive_queue_depth`
(`crates/engine/src/concurrent_delta/work_queue/capacity.rs:66-76`) but
operates on the caller-supplied `worker_count` rather than `rayon::current_num_threads()`.

**Formula B — `ParallelDeltaApplier::DEFAULT_PER_FILE_REORDER_CAPACITY`**
(`crates/engine/src/concurrent_delta/parallel_apply/mod.rs:419-490`):

```rust
pub const DEFAULT_PER_FILE_REORDER_CAPACITY: usize = 64;

impl ParallelDeltaApplier {
    pub fn with_strategy(concurrency: usize, strategy: Arc<dyn ChecksumStrategy>) -> Self {
        ...
        Self {
            files: DashMap::with_shard_amount(shard_count),
            per_file_reorder_capacity: Self::DEFAULT_PER_FILE_REORDER_CAPACITY,
            concurrency,
            strategy,
        }
    }

    pub fn with_per_file_reorder_capacity(mut self, capacity: usize) -> Self {
        assert!(capacity > 0, "per-file reorder capacity must be non-zero");
        self.per_file_reorder_capacity = capacity;
        self
    }
}
```

This is **the** "`reorder_capacity = 64` hard default" flagged in the
`project_reorder_capacity_hard_default` memory note. It is the capacity
plumbed into every per-file `FileSlot::new(writer, reorder_capacity)`
inside the parallel applier (`crates/engine/src/concurrent_delta/parallel_apply/mod.rs:238-241,
519`). It does **not** scale with worker count or with the file's expected
chunk count - it is a fixed compile-time constant overridable only by an
explicit `.with_per_file_reorder_capacity()` builder call (no production
caller invokes the override today).

### 1.4 Plain English summary

| Buffer | Cap formula | Spill-capable? | Adaptive policy wired? |
|---|---|---|---|
| `transfer::delta_pipeline::parallel` reorder | `2..8 * worker_count`, default 2x | Optional via `spawn_with_config` (off by default) | No |
| `ParallelDeltaApplier` per-file ring | hard `64` | No (bare ring only) | No |
| `transfer::BoundedReorderBuffer` (legacy bypass-capable) | hard `DEFAULT_WINDOW_SIZE = 64` | No | No |

## 2. Construction-site inventory

| Site | file:line | ring_cap value | Workload class | Spill risk at default |
|---|---|---|---|---|
| `ParallelDeltaPipeline::new(worker_count)` → `with_capacity()` → `DeltaConsumer::spawn(work_rx, capacity)` | `crates/transfer/src/delta_pipeline/parallel.rs:77-100` | `2 * worker_count`, min 2 | Production receive-delta path (PIP-9.b default-on). Multi-file, parallel-receive-delta. | Low. With workers in single digits (4-16) cap is 8-32. Adversarial chunk orderings or stragglers can fill the ring. Spill is not engaged unless caller opts in via `spawn_with_config`. |
| `ParallelDeltaPipeline::new_adaptive(worker_count, avg_target_size)` → `with_capacity()` → `DeltaConsumer::spawn` | `crates/transfer/src/delta_pipeline/parallel.rs:93-96, 146-159` | `(2..8) * worker_count` by file-size class | Production parallel pipeline when receiver knows the average target size. Small-file workloads get an 8x multiplier. | Lower than Formula A default for small-file workloads (deeper queue); same as default for large-file. |
| `ParallelDeltaPipeline::new_bypass(worker_count)` → `with_bypass_capacity()` → `DeltaConsumer::spawn_bypass` | `crates/transfer/src/delta_pipeline/parallel.rs:116-137` | Capacity ignored - passthrough mode (no ring) | Production parallel pipeline when sequence ordering is unnecessary (e.g., `--delay-updates` off). | None - no ring, no spill possible. |
| `FileSlot::new(writer, per_file_reorder_capacity)` → `ReorderBuffer::new(capacity)` | `crates/engine/src/concurrent_delta/parallel_apply/mod.rs:238-241` | `64` (the `DEFAULT_PER_FILE_REORDER_CAPACITY`) | Per-file slot inside `ParallelDeltaApplier`. Holds DeltaChunks for one open destination file under the parallel-receive-delta path. | **Medium.** Files emitting more than 64 in-flight chunks ahead of `next_expected` will hit `CapacityExceeded`. The applier turns that into `force_insert_count` increments rather than a spill; the per-file path has no `SpillableReorderBuffer` option today. |
| `ConcurrentDeltaConfig::with_spill_threshold(threshold)` (used by `DeltaConsumer::spawn_with_config(rx, reorder_capacity, cfg)`) | `crates/engine/src/concurrent_delta/consumer/mod.rs:227-232`, `consumer/spawn.rs:48-66, 168-220` | `reorder_capacity` (caller-supplied) | Opt-in `SpillableReorderBuffer` path. Only exercised when `--spill-threshold-bytes` / `OC_RSYNC_SPILL_THRESHOLD_BYTES` is set. | Bounded by `threshold` not by `capacity`; spill fires when memory > threshold. The ring still uses the caller-supplied `capacity`, so a low cap forces early spill regardless of byte usage. |
| `transfer::BoundedReorderBuffer::new(window_size)` | `crates/transfer/src/reorder_buffer/state.rs:16-18, 70-84` | Caller-supplied; `DEFAULT_WINDOW_SIZE = 64` is the only constant exported | Legacy sequential reorder path inside the transfer crate (BTreeMap-backed, not the ring path). | Cannot spill - producer is signalled `BackpressureError`. Out of scope for ROB-7 unless we want unified adaptive policy. |

There are **two production code paths** that construct the in-scope rings on
the parallel-receive-delta hot path:

- `ParallelDeltaPipeline::{new, new_adaptive, new_bypass}` constructs the
  pipeline-level reorder ring fed by `DeltaConsumer`.
- `ParallelDeltaApplier::with_strategy()` constructs per-file rings inside the
  applier. The default fans out to one `ReorderBuffer<DeltaChunk>::new(64)`
  per registered file.

All other call sites with `ReorderBuffer::new(...)` are tests
(`crates/engine/src/concurrent_delta/reorder/tests.rs:*`, `consumer/tests.rs:*`,
`pipeline_reorder_integration.rs:*`, multi-producer audit, integration tests)
or benches (`engine/benches/reorder_buffer_*.rs`,
`engine/benches/parallel_dispatch_overhead.rs:235`,
`engine/benches/spill_policy_perf.rs:175-176`).

## 3. Workload-class spill-risk matrix

The matrix below is **heuristic**, derived from the cap formula plus the
already-shipped pressure-paths audit (ROB-4,
`docs/audits/rob-4-reorder-pressure-paths.md`). ROB-5/.6 will replace the
heuristics with measured numbers.

The two failure modes a too-tight ring can produce:

- **Pipeline level (Formula A):** producer (rayon worker pool) blocks on the
  bounded stream channel. With `spill_policy.threshold_bytes = None` (the
  default) the ring rejects the insert with `CapacityExceeded`, which the
  consumer's force-insert path swallows by growing capacity to push the item
  through. With `threshold_bytes = Some(_)` set, the spillable variant
  serialises the excess to a tempfile.
- **Per-file level (Formula B):** the applier's `apply_one_chunk` path returns
  `CapacityExceeded`, which is currently funnelled into `force_insert_count`
  (`crates/engine/src/concurrent_delta/parallel_apply/...`).

| Workload class | Per-file in-flight chunks | Pipeline ring cap (Formula A) | Per-file ring cap (Formula B = 64) | Pipeline spill at default? | Per-file pressure at default? |
|---|---|---|---|---|---|
| Single small file (< 1 MB), 1-4 workers | < 16 chunks | 8 | 64 | None | None |
| 100 mixed files, 4 workers | average 4 in-flight per file | 8 | 64 | Unlikely | None |
| 1K mixed files, 8 workers | average 4 in-flight per file | 16 | 64 | Unlikely | None |
| 10K mixed files, 16 workers | average 4 in-flight per file | 32 | 64 | Unlikely | None |
| 100K mixed files, 16 workers | average 4 in-flight per file | 32 | 64 | Possible under stragglers | Possible if any single file has > 64 simultaneous out-of-order chunks |
| Large-file (> 1 GB), 16 workers, dense delta | per-file chunks dominate | 32 (adaptive: 32) | 64 | Likely if chunks land out-of-order and one is slow | **Likely** — large delta producing 100s of chunks per file outpaces the per-file 64-slot ring |
| Multi-file parallel-receive-delta under adversarial chunk ordering | by construction adversarial | 32 | 64 | Likely (matches the PIP-7 corruption regime, now correctness-fixed but still pressure-prone) | Likely |
| `--delete-before` over small dir | N/A (delete cohort path) | N/A | N/A | N/A | N/A |
| Sequential bypass-mode (`new_bypass`, `--delay-updates` off) | N/A | passthrough | N/A | None | None |

**Headline finding for ROB-7 to address.** At 64 fixed slots per file, the
per-file ring is the binding constraint long before the pipeline ring is. A
single large file with dense reorder pressure can saturate its 64-slot ring
even when the pipeline ring (8-32 slots) still has headroom. The "spill
prevention" target of the ROB series should focus on the per-file ring first,
since today it has **no spill fallback** at all - `CapacityExceeded` is the
only signal. Pipeline-level spill is already on a feature switch
(`SpillableReorderBuffer`) but unused by default.

## 4. Recommendations (feed-forward)

### ROB-2 — `spill_activations` counter

`SpillableReorderBuffer::spill_activations` already exists as a `u64` counter
(see `crates/engine/src/concurrent_delta/spill/buffer/lifecycle.rs:41`). ROB-2
should:

1. Add a public accessor on `SpillableReorderBuffer` that exposes
   `spill_activations` to the consumer thread (parallel to the existing
   `force_insert_counter()` plumbing in
   `crates/engine/src/concurrent_delta/consumer/spawn.rs:192-200`).
2. Plumb it through `DeltaConsumer` to the parent thread the same way
   `spill_events: Arc<AtomicU64>` already is in
   `crates/engine/src/concurrent_delta/consumer/spawn.rs:73, 109, 118`.
   `spill_events` today is incremented in `run_spillable_loop`; the new
   counter should differentiate "spill activations" (transitions from
   in-memory to disk) from per-batch spill events.
3. Surface in the `ReorderMetrics`/`DeltaConsumerStats` snapshot so callers
   (CLI `--info=progress2`, daemon logs) can read it without crossing the
   spill backend boundary.

### ROB-3 — one-shot `log::warn` on first spill

`SpillableReorderBuffer::spill_warned: bool`
(`crates/engine/src/concurrent_delta/spill/buffer/lifecycle.rs:48`) already
exists as the once-per-buffer flag and is referenced by SRO-6 ("Add runtime
warning when spill-to-disk activates"). ROB-3 should:

1. Verify the existing SRO-6 warning fires on the first spill activation per
   transfer (one-shot, not per-batch).
2. Cover the per-file ring path: per-file `ReorderBuffer<DeltaChunk>` has no
   spill backend, so ROB-3's coverage of "first spill activation" only applies
   to the pipeline-level `SpillableReorderBuffer`. The applier's
   `force_insert_count` increment is the analogue for per-file pressure;
   ROB-3 should emit a parallel "applier saturated, falling back to force
   insert" warning so operators can correlate.
3. Include the cap-policy fingerprint in the warning text:
   `cap=N (formula=A worker_count=W) | (formula=B per-file=64)` so the same
   log line tells the operator which formula tripped.

### ROB-4 — pressure-paths audit (already shipped)

ROB-4's findings (`docs/audits/rob-4-reorder-pressure-paths.md`, commit
22aeccffa / 79bea6b6e) enumerate which transfer paths feed sequence-numbered
work into the in-scope rings. ROB-1 cross-references it and asserts the
ring-cap formulas above are the right knobs to address those paths' pressure.
No change.

### ROB-7 — adaptive ring sizing spec

The audit converges on this concrete spec:

1. **Reuse `AdaptiveCapacityPolicy`.** It is already implemented in
   `crates/engine/src/concurrent_delta/adaptive.rs` with grow at 80% util +
   `gap_window > capacity/2`, shrink below 25% mean util over
   `DEFAULT_SAMPLE_WINDOW = 32` samples, `growth_factor` configurable. Tests
   exercise it. No new policy is needed.
2. **Wire it into the pipeline ring (Formula A).** Replace
   `ReorderBuffer::new(capacity)` in `consumer/spawn.rs:168` with
   `ReorderBuffer::with_adaptive_policy(AdaptiveCapacityPolicy::new(
       capacity, capacity * 8, 2.0))`. The `min` matches the existing default;
   the `max` is the pipeline's hard ceiling, sized at 8x the static default.
3. **Wire it into the per-file ring (Formula B).** Same change to
   `crates/engine/src/concurrent_delta/parallel_apply/mod.rs:241`. `min = 64`,
   `max = 1024` is a defensible upper bound that absorbs the large-file
   pressure case without unbounded growth. The fast path (no out-of-order
   chunks) stays at `min` because the policy never grows when util is low.
4. **Gate behind a feature flag.** `reorder-adaptive-ring` Cargo feature on
   the engine crate, default off until ROB-9/.10 benches justify flipping.
   That also satisfies the "do not regress non-adversarial workloads" bar in
   the parent task description.
5. **ROB-11 (env override).** The `OC_RSYNC_REORDER_RING_CAP` knob proposed
   in the parent task should override the policy `min` (and `max`, if
   adaptive is enabled). Parsing into `consumer/spawn.rs`'s `from_config()`
   alongside the existing `apply_env_overrides` for spill is the natural
   insertion point.

### Open question on Formula B's hard 64

The 64 figure has no recorded benchmark backing it - the doc comment says
only "sized to hold a handful of rayon workers' worth of in-flight chunks per
file without forcing the producer to block"
(`crates/engine/src/concurrent_delta/parallel_apply/mod.rs:420-422`). ROB-6
(bench normal-operation spill rate) should record the per-file
`CapacityExceeded` rate at 100/1K/10K/100K file scales so ROB-7's `max =
1024` can be substantiated or revised.

## 5. Open questions (defer to ROB-5 / ROB-6)

1. **Is normal-operation spill happening today?** No production telemetry
   exists yet. ROB-2/3 need to land before we can answer. Until then we are
   sizing the policy blind to real-world pressure.
2. **What is the per-file `CapacityExceeded` rate on the parallel-receive-
   delta path at 100K+ files?** The applier increments
   `force_insert_count` rather than spilling; we have no record of what that
   counter sits at in CI nor in the existing benches. ROB-6 must capture it.
3. **Does Formula A's `min(capacity, threads * 2)` underflow on small
   `worker_count`?** With `worker_count = 1`, formula A yields `cap = 2`. The
   pipeline ring at cap 2 is fragile under any reorder pressure. ROB-7 should
   bound `min` at e.g. 8 even when `2 * worker_count < 8`.
4. **Is `BoundedReorderBuffer` (transfer crate) reachable in production
   today?** Inventory shows only test/bench call sites; if the transfer-level
   reorder is dead code, ROB-12 should note it for removal. If it is live,
   ROB-7's adaptive policy needs a separate wiring there (it uses `BTreeMap`
   not the ring, so it cannot reuse `AdaptiveCapacityPolicy` directly).
5. **Should the pipeline ring and the per-file ring share a single policy
   instance or independent ones?** Per-file pressure is decorrelated across
   files; per-pipeline pressure is correlated with stream backlog. Independent
   policies seem right but ROB-7 should call this out explicitly.

## 6. Cross-references

- ROB-4 (shipped): `docs/audits/rob-4-reorder-pressure-paths.md`
- SPL-32/.33/.34 (shipped): ENOSPC + temp-vanish fault injection establishes
  the spill path's graceful degradation contract; the ROB series should not
  re-derive those properties.
- SRO-3/.4/.5/.6/.7: `SpillPolicy::InMemoryOnly`, `--no-spill` /
  `OC_RSYNC_NO_SPILL`, graceful spill-write-error propagation, runtime warning
  on first spill, pre-flight writability check. Already shipped; ROB-3's
  one-shot warning extends SRO-6 rather than replacing it.
- `project_reorder_capacity_hard_default` memory note: confirmed by this
  audit; the 64-slot default lives in
  `ParallelDeltaApplier::DEFAULT_PER_FILE_REORDER_CAPACITY`.
- `project_reorder_spill_fragility` memory note: SPL-32/.33/.34 closed the
  graceful-degradation gap. ROB's job is upstream of that - prevent the spill
  from firing in non-adversarial workloads in the first place.

## 7. File-path appendix

In-scope code:

- `crates/engine/src/concurrent_delta/reorder/mod.rs`
- `crates/engine/src/concurrent_delta/reorder/{drain.rs, insert.rs, state.rs}`
- `crates/engine/src/concurrent_delta/adaptive.rs`
- `crates/engine/src/concurrent_delta/spill/buffer/lifecycle.rs`
- `crates/engine/src/concurrent_delta/spill/{policy.rs, env.rs, stats.rs, error.rs}`
- `crates/engine/src/concurrent_delta/config.rs`
- `crates/engine/src/concurrent_delta/consumer/{mod.rs, spawn.rs, loops.rs}`
- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs`
- `crates/engine/src/concurrent_delta/work_queue/capacity.rs`
- `crates/transfer/src/delta_pipeline/parallel.rs`

Out-of-scope but referenced:

- `crates/transfer/src/reorder_buffer/{mod.rs, state.rs, insert.rs, drain.rs}`
- `crates/engine/src/delete/reorder_buffer.rs`
