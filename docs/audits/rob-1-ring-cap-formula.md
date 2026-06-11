# ROB-1 - ReorderBuffer ring-cap formula and call sites (fresh audit)

Parent series: ROB (ReorderBuffer normal-operation spill prevention).
Date: 2026-06-11. Status: audit-only, no code change.
Companion doc: `docs/audits/rob-1-reorder-ring-cap-audit.md` (earlier today
revision; treats ROB-11 as future work). This refresh re-runs the audit
against the current master and incorporates the ROB-11 env override that
landed at commit 22b16806e (`feat(engine): add OC_RSYNC_REORDER_RING_CAP env
override (ROB-11)`).

## Scope

Three reorder structures share the `ReorderBuffer` name; only the engine
concurrent-delta ring is in scope for ROB:

1. `engine::concurrent_delta::reorder::ReorderBuffer<T>` - pre-allocated ring,
   optionally wrapped by `SpillableReorderBuffer`. **In scope.**
2. `engine::concurrent_delta::parallel_apply::FileSlot::reorder` - one
   `ReorderBuffer<DeltaChunk>` per registered file inside
   `ParallelDeltaApplier`. Same type as (1) with a separate capacity knob.
   **In scope.**
3. `engine::delete::reorder_buffer::ReorderBuffer` - delete-cohort `BTreeMap`
   keyed by rank, hard cap `MAX_BUFFERED_COHORTS = 64`. Different data
   structure. **Out of scope.**
4. `transfer::reorder_buffer::BoundedReorderBuffer<T>` - `BTreeMap`-backed
   sliding-window with `DEFAULT_WINDOW_SIZE = 64`. No production callers
   today (only `tests/` and `benches/`). **Adjacent; tracked for ROB-12.**

## 1. Current ring-cap formula

### 1.1 Constructor (no internal heuristic)

`crates/engine/src/concurrent_delta/reorder/mod.rs:200`:

```rust
pub fn new(capacity: usize) -> Self {
    assert!(capacity > 0, "reorder buffer capacity must be non-zero");
    let slots: Vec<Option<T>> = (0..capacity).map(|_| None).collect();
    ...
}
```

The ring takes `capacity` verbatim. Internal grow/shrink logic exists only
when the buffer is constructed via `with_adaptive_policy()`
(`crates/engine/src/concurrent_delta/reorder/mod.rs:279`), which is not
wired into any production call site today.

### 1.2 Caller-side formulas

Two formulas compute the value handed to `ReorderBuffer::new`:

**Formula A - parallel pipeline cap**
(`crates/transfer/src/delta_pipeline/parallel.rs:77-100, 146-159`):

```rust
pub fn new(worker_count: usize) -> Self {
    let capacity = worker_count.saturating_mul(2).max(2);
    Self::with_capacity(capacity)
}

pub(super) fn adaptive_capacity(worker_count: usize, avg_target_size: u64) -> usize {
    const SMALL_FILE_THRESHOLD: u64 = 64 * 1024;
    const LARGE_FILE_THRESHOLD: u64 = 1024 * 1024;
    let multiplier: usize = if avg_target_size == 0 { 2 }
        else if avg_target_size < SMALL_FILE_THRESHOLD { 8 }
        else if avg_target_size > LARGE_FILE_THRESHOLD { 2 }
        else { 4 };
    worker_count.saturating_mul(multiplier).max(2)
}
```

Net result: `capacity = (2..=8) * worker_count`, clamped `>= 2`. Same value
also bounds the upstream work queue.

**Formula B - per-file applier cap**
(`crates/engine/src/concurrent_delta/parallel_apply/mod.rs:429, 478-489`):

```rust
pub const DEFAULT_PER_FILE_REORDER_CAPACITY: usize = 64;

pub fn with_strategy(concurrency: usize, strategy: Arc<dyn ChecksumStrategy>) -> Self {
    let shard_count = shard_sizing::resolve_shard_count(concurrency);
    let per_file_reorder_capacity =
        ring_cap_env::resolve_ring_capacity(Self::DEFAULT_PER_FILE_REORDER_CAPACITY);
    Self {
        files: DashMap::with_shard_amount(shard_count),
        per_file_reorder_capacity,
        concurrency,
        strategy,
    }
}
```

Hard constant `64` per file, **now overridable** by `OC_RSYNC_REORDER_RING_CAP`
since ROB-11 shipped (see Section 2).

### 1.3 Plain-English summary

| Buffer | Cap formula | Spill backend? | Adaptive policy wired? | Env override? |
|---|---|---|---|---|
| Pipeline ring (`DeltaConsumer::spawn{,_bypass,_with_config}`) | `(2..8) * worker_count` | Optional (`spawn_with_config` + `spill_policy.threshold_bytes = Some(_)`) | No | No (Formula A is computed in the caller) |
| Per-file ring (`ParallelDeltaApplier`) | hard `64` default | None | No | **Yes** - `OC_RSYNC_REORDER_RING_CAP` (ROB-11) |
| `transfer::BoundedReorderBuffer` | `DEFAULT_WINDOW_SIZE = 64`, caller-supplied | Cannot spill (backpressure only) | No | No |

## 2. ROB-11 env override - confirmed shipped

The parent task asked us to check whether `OC_RSYNC_REORDER_RING_CAP` had
shipped. **Yes**: commit 22b16806e (2026-06-11 07:02:45 +0300, earlier today)
added `crates/engine/src/concurrent_delta/parallel_apply/ring_cap_env.rs`.

Key properties verified by source-read:

- `RING_CAP_ENV = "OC_RSYNC_REORDER_RING_CAP"`
  (`ring_cap_env.rs:46`).
- `resolve_ring_capacity(default)` is wired into
  `ParallelDeltaApplier::with_strategy` at
  `crates/engine/src/concurrent_delta/parallel_apply/mod.rs:481-482`.
- Read-once via `OnceLock<Option<usize>>` (`ring_cap_env.rs:54`); env access
  is not on the hot construction path.
- Parser accepts any positive `usize`. Zero is rejected with a one-shot
  `eprintln!` warning; trailing-garbage and negative values fall back to the
  default with a warning. Whitespace-trimmed.
- No upper clamp by design (`ring_cap_env.rs:27-32`): an operator setting
  `8192` for an adversarial workload gets exactly `8192`.
- Tests cover the parser (`ring_cap_env.rs:96-213`) and the single-process
  cache lives at `crates/engine/tests/parallel_apply_ring_cap_env.rs` (per
  comment block).

### 2.1 Coverage gap

ROB-11 covers **Formula B only**. The pipeline ring (Formula A) reads its
capacity from `worker_count` passed by the receiver pipeline and is not
gated through `resolve_ring_capacity`. An operator who exports
`OC_RSYNC_REORDER_RING_CAP=512` to absorb pipeline pressure will see no
effect on `DeltaConsumer::spawn`'s ring - only on each
`ParallelDeltaApplier`'s per-file slot.

This is consistent with the parent task's framing (the 64 hard default was
always the per-file ring) but is a follow-on item for ROB-2..13 to either
extend the env knob into Formula A or document the asymmetry.

## 3. Call-site inventory

Production call sites that construct an in-scope ring, ordered by hot-path
proximity:

| Site | file:line | Cap value | Workload class | Spill backend |
|---|---|---|---|---|
| `ParallelDeltaPipeline::new(worker_count)` -> `with_capacity()` -> `DeltaConsumer::spawn(work_rx, capacity)` -> `ReorderBuffer::new(capacity)` | `crates/transfer/src/delta_pipeline/parallel.rs:77-100`; `crates/engine/src/concurrent_delta/consumer/spawn.rs:168` | `2 * worker_count`, min 2 | Default parallel receive-delta pipeline. | None unless `spawn_with_config` is used. |
| `ParallelDeltaPipeline::new_adaptive(worker_count, avg_target_size)` -> same chain | `crates/transfer/src/delta_pipeline/parallel.rs:93-96, 146-159`; `consumer/spawn.rs:168` | `(2..8) * worker_count` | File-size-aware variant. | Same. |
| `ParallelDeltaPipeline::new_bypass{,_adaptive}(worker_count)` -> `DeltaConsumer::spawn_bypass(work_rx)` -> `ReorderBuffer::passthrough()` | `crates/transfer/src/delta_pipeline/parallel.rs:116-137`; `consumer/spawn.rs:167` | passthrough (no ring) | `--delay-updates` off path. | None - no ring, no spill possible. |
| `DeltaConsumer::spawn_with_config(rx, reorder_capacity, cfg)` -> `ReorderMode::Spillable {..}` -> `SpillableReorderBuffer::new(capacity, threshold)` -> `ReorderBuffer::new(capacity)` (via `spill::buffer::lifecycle.rs`) | `crates/engine/src/concurrent_delta/consumer/mod.rs:226-233`; `consumer/spawn.rs:48-66, 169-220`; `spill/buffer/lifecycle.rs:31, 74` | `reorder_capacity` (caller) + `threshold` bytes | Opt-in spill path triggered by `OC_RSYNC_SPILL_THRESHOLD_BYTES` or `ConcurrentDeltaConfig::with_spill_threshold`. | `SpillableReorderBuffer` (tempfile, optional dir, optional zstd). |
| `ParallelDeltaApplier::{new, with_strategy}` -> `per_file_reorder_capacity = ring_cap_env::resolve_ring_capacity(64)` -> `FileSlot::new(writer, cap)` -> `ReorderBuffer::new(cap)` | `crates/engine/src/concurrent_delta/parallel_apply/mod.rs:441-489, 552, 242` | `64` default, env-overridable via `OC_RSYNC_REORDER_RING_CAP` (ROB-11) | Per-file slot in parallel applier. Allocated **per registered file**. | None. `CapacityExceeded` is funnelled into `force_insert_count`. |
| `ParallelDeltaApplier::with_per_file_reorder_capacity(cap)` builder | `parallel_apply/mod.rs:506-510` | Caller-supplied (overrides env + default) | None today - no production caller invokes the builder. | None. |

Non-production sites (excluded from the matrix above): all `crates/*/benches/*`
binaries, `tests/*` integration tests, and per-module `tests.rs`. They use a
mix of small fixed values (1, 2, 4, 8, 16, 32, 128, 1024) tuned to exercise
boundary conditions; none influence the production ring sizing.

## 4. Workload-shape spill matrix

Two failure modes a too-tight ring can produce:

- **Pipeline level (Formula A).** Producer (rayon worker pool) hits the
  bounded stream channel. When `spill_policy.threshold_bytes = None`, the
  ring rejects the insert with `CapacityExceeded`; the consumer's
  `force_insert` path grows the ring to push the item through (and bumps
  `force_insert_count`). When `threshold_bytes = Some(_)`, the spillable
  variant serialises excess to a tempfile.
- **Per-file level (Formula B).** `apply_one_chunk` returns
  `CapacityExceeded` for any file whose in-flight chunk-sequence offset
  exceeds 64 (or the env-override). Today the applier funnels this into
  `force_insert_count`; there is no per-file spill backend.

The matrix is heuristic (no measured numbers yet - that is ROB-5/6):

| Workload | Per-file in-flight chunks | Pipeline ring (Formula A) | Per-file ring (Formula B default 64) | Pipeline spill risk | Per-file pressure risk |
|---|---|---|---|---|---|
| Single small file (< 1 MB), 1-4 workers | < 16 | 8 | 64 | None | None |
| 100 mixed files, 4 workers | avg 4 | 8 | 64 | None | None |
| 1K mixed files, 8 workers | avg 4 | 16 | 64 | Low | None |
| 10K mixed files, 16 workers | avg 4 | 32 | 64 | Low | None |
| 100K mixed files, 16 workers, well-mixed | avg 4 | 32 | 64 | Possible under stragglers | Possible (any file with > 64 OOO chunks) |
| Single large file (> 1 GB), 16 workers, dense delta | hundreds | 32 (adaptive: 32) | 64 | Possible | **Likely** |
| Multi-file parallel-receive-delta + adversarial chunk ordering | adversarial by construction | 32 | 64 | Likely | Likely |
| Bypass mode (`new_bypass`, `--delay-updates` off) | N/A | passthrough | N/A | None | None |
| `spawn_with_config` with `threshold_bytes = Some(_)` | varies | caller-set | 64 | Spill engages on byte trigger, not cap | Per-file ring is unchanged |

**Headline finding (unchanged from earlier audit).** At 64 fixed slots per
file the per-file ring is the binding constraint long before the pipeline
ring is. ROB-11 gives operators a runtime escape hatch but does not change
the default. The "spill prevention" target of the ROB series should keep
focusing on the per-file ring first - it still has no spill fallback at all.

## 5. Recommended next steps for ROB-2..13

The previous audit's recommendations remain largely valid; the following
adjustments incorporate ROB-11 having shipped.

### ROB-2 - `spill_activations` accessor

Unchanged. `SpillableReorderBuffer::spill_activations` already exists as a
`u64` counter (`crates/engine/src/concurrent_delta/spill/buffer/lifecycle.rs:41`).
ROB-2 should expose it through `DeltaConsumer::stats` /
`DeltaConsumerStats` alongside `spill_events` / `force_inserts`, and ensure
the consumer thread plumbs the value over the existing
`Arc<AtomicU64>` channel.

### ROB-3 - one-shot warning on first pressure

Two pressure surfaces, each gets its own one-shot warning:

1. Pipeline spill: extend the existing SRO-6 path (`spill_warned: bool` at
   `spill/buffer/lifecycle.rs:48`). Verify the warning fires exactly once
   per transfer.
2. Per-file applier pressure: emit a parallel "applier ring saturated,
   falling back to force_insert" warning, gated by a `OnceLock<bool>` on
   `ParallelDeltaApplier` so it fires once per applier instance, not once
   per file.

Warning text should include the cap-policy fingerprint:
`cap=N source=(env|default|builder) formula=(A|B)`. After ROB-11 the source
is observable via `per_file_reorder_capacity()`.

### ROB-4 - pressure-paths audit

Already shipped (`docs/audits/rob-4-reorder-pressure-paths.md`). No change.

### ROB-5 - workload simulator

Build the simulator at `crates/engine/tests/rob_simulator/` that drives the
parallel applier with controlled chunk-ordering distributions (random,
adversarial worst-case, single-large-file). Reports
`force_insert_count` and (post-ROB-2) `spill_activations` per scenario.
Bench scaffolding can reuse the existing
`engine/benches/reorder_buffer_scaling.rs` and
`engine/benches/parallel_dispatch_overhead.rs` constructors.

### ROB-6 - measure default-load spill rate

Run ROB-5 against the v0.6.x bench corpora (the same ones used for the RSS
and CSM series). Capture force-insert rate and pipeline-spill rate as
percentages of total inserts. Output a CSV under
`docs/benchmarks/rob-6-default-load-spill.md`.

### ROB-7 - adaptive ring sizing wiring

The previous audit's spec stands. Concrete plan:

1. Reuse `AdaptiveCapacityPolicy` (`crates/engine/src/concurrent_delta/adaptive.rs`).
   No new policy needed - grow at >= 80% util + `gap_window > capacity/2`,
   shrink below 25% mean over `DEFAULT_SAMPLE_WINDOW = 32` samples.
2. Pipeline ring (Formula A): switch `consumer/spawn.rs:168`'s
   `ReorderBuffer::new(capacity)` to
   `ReorderBuffer::with_adaptive_policy(AdaptiveCapacityPolicy::new(capacity, capacity * 8, 2.0))`.
3. Per-file ring (Formula B): same switch at `parallel_apply/mod.rs:242`.
   `min = ring_cap_env::resolve_ring_capacity(64)` so ROB-11 still wins;
   `max = 1024` is a defensible upper bound that absorbs dense-large-file
   pressure without unbounded growth.
4. Gate behind a `reorder-adaptive-ring` Cargo feature on the engine crate,
   default off until ROB-9/.10 benches justify flipping on.
5. The env override stays in `ring_cap_env`; ROB-7 must NOT silently
   override the operator's pin. Recommended precedence:
   `with_per_file_reorder_capacity` (explicit builder) > `OC_RSYNC_REORDER_RING_CAP`
   (env) > adaptive policy `min`.

### ROB-8 - feature-flag gate

Cargo feature `reorder-adaptive-ring` on the engine crate, off by default.
Add a workspace-level passthrough so `transfer` and `cli` can opt in
without leaking the feature into non-engine consumers.

### ROB-9 / ROB-10 - bench harness

ROB-9: extend `crates/engine/benches/reorder_buffer_scaling.rs` with
adversarial-ordering scenarios. ROB-10: run with and without the adaptive
feature; publish a `docs/benchmarks/rob-10-adaptive-ring-results.md`.

### ROB-11 - env override (SHIPPED)

Commit 22b16806e. No further work needed except:

- ROB-11.followup.A: extend `OC_RSYNC_REORDER_RING_CAP` into Formula A
  (pipeline ring) so a single env knob covers both call sites. Currently
  the env var is per-file only.
- ROB-11.followup.B: document the precedence rules in `docs/troubleshooting/`
  once ROB-7 lands (today the env var has no competing knob in production).

### ROB-12 - `BoundedReorderBuffer` (transfer crate) - resolve dead-code status

Inventory confirms no production callers. Decide:

- Remove the type and downstream `DEFAULT_WINDOW_SIZE` constant if it is
  truly unused (recommended); or
- Wire it up as the legacy reorder path under a feature flag if there is a
  rollback plan.

Either way ROB-12 should not block ROB-7 - the adaptive policy can live
solely on the in-scope ring.

### ROB-13 - `--reorder-ring-cap` CLI surface

Promote `OC_RSYNC_REORDER_RING_CAP` to a first-class CLI flag once ROB-2/3
telemetry confirms operators reach for it in the field. The flag should
plumb through `CoreConfig` -> `ParallelDeltaApplier::with_per_file_reorder_capacity`,
giving CLI > env > default precedence and keeping the env var as the
container-orchestration knob.

## 6. Open questions deferred to ROB-5 / ROB-6

1. **Is normal-operation per-file spill happening today?** No production
   telemetry exists yet. ROB-2/3 are the prerequisite for answering.
2. **Per-file `CapacityExceeded` rate on the parallel-receive-delta path at
   100K+ files?** Currently funnelled into `force_insert_count`; the
   counter is not exposed in `DeltaConsumerStats`. ROB-2 must surface it.
3. **Formula A small-`worker_count` underflow.** With `worker_count = 1`,
   cap is `2`. Pipeline ring at cap 2 is fragile under any reorder
   pressure. ROB-7 should clamp the policy `min` at e.g. 8 even when
   `2 * worker_count < 8`.
4. **`BoundedReorderBuffer` liveness.** Inventory shows test/bench only.
   ROB-12 must confirm and either remove or document the suppressed
   integration.
5. **Should the pipeline ring and per-file ring share one adaptive policy
   or use independent ones?** Per-file pressure is decorrelated across
   files; per-pipeline pressure is correlated with stream backlog.
   Independent policies are the right default; ROB-7 must call this out.
6. **Should ROB-11 extend to Formula A?** A single env knob covering both
   surfaces is operationally cleaner. ROB-11.followup.A.

## 7. Cross-references

- `project_reorder_capacity_hard_default` memory note: confirmed by this
  audit. The 64-slot default still lives in
  `ParallelDeltaApplier::DEFAULT_PER_FILE_REORDER_CAPACITY` and is now
  reachable through `OC_RSYNC_REORDER_RING_CAP`. The note should be
  updated to reference ROB-11 having shipped and ROB-7 still pending.
- `docs/audits/rob-1-reorder-ring-cap-audit.md` - earlier-today revision
  of this audit. This refresh supersedes it for the call-site inventory
  and the ROB-11 status; the workload matrix is unchanged.
- `docs/audits/rob-4-reorder-pressure-paths.md` - which transfer paths
  feed sequence-numbered work into the in-scope rings. Unchanged.
- `docs/design/reorderbuffer-spill-to-tempfile.md` - `SpillableReorderBuffer`
  contract.
- `docs/design/rob-7-adaptive-reorder-ring.md` - adaptive policy spec the
  Section 5 ROB-7 recommendations build on.
- SPL-32/33/34 (shipped): ENOSPC + temp-vanish fault injection covers
  spill graceful-degradation; ROB does not re-derive those properties.
- SRO-3..7 (shipped): `SpillPolicy::InMemoryOnly`, `--no-spill` /
  `OC_RSYNC_NO_SPILL`, runtime warning on first spill. ROB-3's per-file
  one-shot warning extends SRO-6 rather than replacing it.

## 8. File-path appendix

In-scope code:

- `crates/engine/src/concurrent_delta/reorder/mod.rs`
- `crates/engine/src/concurrent_delta/reorder/{histogram.rs, tests.rs}`
- `crates/engine/src/concurrent_delta/adaptive.rs`
- `crates/engine/src/concurrent_delta/spill/buffer/lifecycle.rs`
- `crates/engine/src/concurrent_delta/spill/{mod.rs, policy.rs, env.rs, stats.rs, error.rs}`
- `crates/engine/src/concurrent_delta/config.rs`
- `crates/engine/src/concurrent_delta/consumer/{mod.rs, spawn.rs, loops.rs}`
- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs`
- `crates/engine/src/concurrent_delta/parallel_apply/ring_cap_env.rs` (ROB-11)
- `crates/engine/src/concurrent_delta/work_queue/capacity.rs`
- `crates/transfer/src/delta_pipeline/parallel.rs`

Out-of-scope but referenced:

- `crates/transfer/src/reorder_buffer/{mod.rs, state.rs, insert.rs, drain.rs}`
- `crates/engine/src/delete/reorder_buffer.rs`
