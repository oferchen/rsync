# ROB-7 - Adaptive ReorderBuffer ring sizing spec

Parent series: ROB (ReorderBuffer normal-operation spill prevention).
Status: spec, no code change. Date: 2026-06-11.
Predecessors: ROB-1 (cap-formula audit, shipped), ROB-4 (pressure-paths audit,
shipped).
Successors: ROB-8 (implementation), ROB-9 (bench), ROB-10 (default-on
decision), ROB-11 (env override), ROB-13 (CI regression cell).

## 1. Problem statement

ROB-1's audit (`docs/audits/rob-1-reorder-ring-cap-audit.md`) identifies two
production reorder rings on the parallel-receive-delta hot path:

- **Formula A - pipeline ring**. Capacity is `2..8 * worker_count` (clamped
  to `>= 2`), computed at
  `crates/transfer/src/delta_pipeline/parallel.rs:77-100,146-159`. Wraps the
  bounded work queue and the `DeltaConsumer` reorder buffer. Spill is
  reachable via `SpillableReorderBuffer` when a caller opts in via
  `spawn_with_config`, but the default `DeltaConsumer::spawn` path uses the
  bare `ReorderBuffer` with no spill backend at all.
- **Formula B - per-file ring**. Hard constant
  `DEFAULT_PER_FILE_REORDER_CAPACITY = 64` at
  `crates/engine/src/concurrent_delta/parallel_apply/mod.rs:419-490`. One
  `ReorderBuffer<DeltaChunk>::new(64)` per registered destination file inside
  the `ParallelDeltaApplier`. No spill backend exists at this layer at all -
  the only signal an over-capacity insert produces today is a
  `force_insert_count` increment on the applier's metric.

The audit's headline finding (section 3, "Headline finding for ROB-7 to
address") is that at 64 fixed slots per file, **the per-file ring is the
binding constraint long before the pipeline ring is**. A single large file
with dense reorder pressure can saturate its 64-slot ring even when the
pipeline ring (8-32 slots) still has headroom. Adversarial chunk orderings
amplify the problem to the point where even non-adversarial workloads with
unfortunate worker scheduling can trip the pressure paths.

The matching memory note (`project_reorder_capacity_hard_default`)
acknowledges the smell but observes it has not been re-prioritised because
the per-file path has no spill fallback - the failure mode is silent
`force_insert` growth, not user-visible spill activations.

The ROB parent task ("ReorderBuffer normal-operation spill prevention")
treats spill activation as the symptom and ring saturation as the root
cause. ROB-7 is the spec for closing the gap one level upstream of spill:
**grow the ring when memory allows, fall back to spill only when growth
hits its bound**.

### 1.1 What "normal operation" means here

A non-adversarial workload that nonetheless saturates a fixed-64 per-file
ring today:

- Receiver has 16 rayon workers. Sender pushes a 1 GiB destination file
  containing 65,536 16 KiB chunks.
- Chunks 0-15 dispatch to 16 workers. Worker 4 stalls briefly on a
  page-cache miss. Workers 0-3, 5-15 complete and return chunks
  out-of-order ahead of worker 4's chunk 4.
- The ring fills up with chunks 16..N as long as `next_expected == 4`
  is unfilled. With 64 slots, capacity is exhausted by chunk 67 -
  worker 5's next dispatch returns chunk 67 to the applier and
  `apply_one_chunk` returns `CapacityExceeded`.

This produces the silent `force_insert_count` increment described in
ROB-1 section 2. No spill happens, but the wire path is forced to grow
the buffer in-place to keep the invariant. The applier already does this
correctly (PR #4552, PR #4556 cover the smell). The cost is allocation +
copy at the per-file boundary, not data corruption.

## 2. Existing primitives

Two pieces of infrastructure are already in tree and unwired (or partially
wired) for this work. Both are reusable verbatim.

### 2.1 `AdaptiveCapacityPolicy` (already implemented, unwired)

File: `crates/engine/src/concurrent_delta/adaptive.rs`.

Public API:

```rust
pub struct AdaptiveCapacityPolicy {
    pub min: usize,           // starting capacity, must be >= 1
    pub max: usize,           // ceiling, must be >= min
    pub growth_factor: f32,   // multiplicative grow factor, must be > 1.0
    pub sample_window: usize, // window length for shrink decision
}

impl AdaptiveCapacityPolicy {
    pub fn new(min, max, growth_factor) -> Self;
    pub fn with_window(min, max, growth_factor, sample_window) -> Self;
}
```

Internal feedback signals consumed (private to the crate, threaded through
`AdaptiveState`):

- **Grow predicate** (called on every `push()`):
  `utilization >= 0.80 AND gap_window > capacity / 2`. The gap-window
  condition is critical - sustained-near-capacity transient that resolves
  quickly should not grow.
- **Shrink predicate** (called when the sample window fills):
  rolling mean over `sample_window` (default 32) of `count / capacity`
  drops below 0.25.
- `reset_window()` on every capacity change so the next decision reflects
  the new geometry.

`ReorderBuffer` exposes the composition seam:

```rust
impl<T> ReorderBuffer<T> {
    pub fn with_adaptive_policy(policy: AdaptiveCapacityPolicy) -> Self;
}
```

Tests and benches exercise both the policy and the wired path. Production
call sites do not currently invoke `with_adaptive_policy`. The
`ReorderStats { grow_events, shrink_events, capacity }` snapshot is already
plumbed through `ReorderBuffer::stats()`.

### 2.2 `work_queue::adaptive_queue_depth` (production, sister formula)

File: `crates/engine/src/concurrent_delta/work_queue/capacity.rs:66-76`.

Computes a static depth multiplier from `avg_file_size` at construction
time, ranging from 2x (large-file, I/O bound) to 8x (small-file,
syscall-bound). Mirrors Formula A but expressed in terms of
`rayon::current_num_threads()` rather than an explicit `worker_count`.

This is **not** an adaptive policy - it is sized once at pipeline
construction. ROB-7 should reuse the same workload-class table for the
**`max` ceiling** of the adaptive policy so the new code does not invent a
second set of thresholds.

### 2.3 What is missing

The adaptive policy is wired into `ReorderBuffer::with_adaptive_policy` but
neither the production `DeltaConsumer::spawn` site nor the
`ParallelDeltaApplier::with_strategy` site invokes it. ROB-7's job is to
spec the wiring shape per-policy-choice; ROB-8 implements.

## 3. Proposed adaptive policy - three options

Three design points are on the table. Each grows the per-file ring before
spill activation; they differ in feedback signal complexity and operator
ergonomics. We spec all three so ROB-8/.9 can choose based on bench data
rather than upfront opinion.

### Option A - Grow-on-near-capacity (cheap, no feedback loop)

**Mechanism.** Reuse the existing `AdaptiveCapacityPolicy` verbatim.
Wire `ParallelDeltaApplier::with_strategy()` to construct the per-file
ring via `ReorderBuffer::with_adaptive_policy(AdaptiveCapacityPolicy::new(
    DEFAULT_PER_FILE_REORDER_CAPACITY,
    Self::max_per_file_capacity(),
    2.0,
))` where `max_per_file_capacity()` returns a hard ceiling.

**Ceiling.** Two candidates:

1. **Fixed 1024.** 16x the current default. Defensible upper bound that
   absorbs the large-file pressure case without unbounded growth. Cap is
   independent of worker count.
2. **`worker_count * 16`.** Scales with concurrency. With 16 workers this
   matches option 1; with 64 workers it lifts to 1024. ROB-7 prefers this
   because the audit's pressure model already scales pressure with worker
   count.

Pick (2). Floor stays at 64 to preserve the fast path - non-adversarial
workloads never grow past the existing default.

**Feedback signals.** None beyond what `AdaptiveCapacityPolicy` already
consumes (utilization + gap window). The grow predicate fires as soon as
the ring crosses 80% full with sustained gap; no external trigger.

**Cost.** Cheap. The policy's `should_grow` is called on every `push()`
already (when wired). The only overhead is the `Vec<f32>` sample window
allocation per file, sized at 32 floats (128 bytes) per `FileSlot`. With
the existing DashMap+shard model, this is bounded by concurrent file count.

**Pros.** No new feedback infrastructure. Drop-in reuse. The same policy
shape works for the pipeline ring (Formula A) and the per-file ring
(Formula B).

**Cons.** Memory-blind. A `min=64, max=worker_count*16` policy can grow
every concurrent file to its max simultaneously under pressure, which at
1000 concurrent open files and worker_count=16 gives
1000 * 1024 * sizeof(Option<DeltaChunk>) of headroom slots even when RSS
budget is tight. Option B addresses this.

### Option B - RSS-feedback adaptive (uses STN-6 trigger)

**Mechanism.** Combine Option A's `AdaptiveCapacityPolicy` with a
per-policy RSS callback. STN-6 ("Memory-pressure trigger (RSS-aware)") is
already shipped and exposes a runtime RSS query. Today STN-6 only fires
on spill activation. ROB-7's variant fires the same query in `push()`:

```rust
impl AdaptiveState {
    fn should_grow(
        &self,
        count: usize,
        capacity: usize,
        gap_window: usize,
        rss_budget: RssBudget,  // NEW input
    ) -> bool {
        if !rss_budget.has_headroom() {
            return false;  // grow blocked by RSS budget
        }
        // existing grow predicate
        ...
    }
}
```

**Semantics.** When RSS budget remains, behave like Option A. When budget
is tight, **block growth** so the ring stays at its current capacity.
`force_insert` still kicks in on saturation; the pipeline ring's
`SpillableReorderBuffer` (when configured) still spills. The applier-level
per-file ring still has no spill backend, so a saturated per-file ring
under RSS pressure produces `force_insert_count` growth same as today.

**Feedback signals.** Two:

1. Same utilization + gap-window predicate as Option A.
2. RSS budget query, gated to fire at most every N pushes (cheap path)
   or every M ms (timer path) to avoid hot-loop syscalls.

**Cost.** Per-push RSS query is expensive (Linux `getrusage` + parse,
~5 us). Must be amortised. Two amortisation strategies:

- **Counter-gated**: query every Nth push per policy instance. N=1024 means
  per-file query frequency tracks chunk arrival rate.
- **Timer-gated**: query every M ms (M=100 ms candidate) from a shared
  worker thread, with the result cached in an `AtomicBool` for
  lock-free read.

ROB-8 picks; counter-gated is simpler, timer-gated is cheaper at scale.

**Pros.** Memory-aware. The "1000 concurrent files all grow to max" worst
case from Option A becomes self-limiting under RSS pressure. Aligns with
the spill module's existing RSS-pressure trigger (STN-6) so operators see
a coherent memory-pressure model.

**Cons.** Two-input feedback loop adds correctness surface. RSS query
amortisation needs benchmarking. STN-6's RSS query API needs to land an
explicit "headroom-poll" method (today it only emits on spill activation).
The grow-block path needs to distinguish "RSS blocks me" from "policy
declines to grow" so observers can attribute correctly.

### Option C - Workload-class hint (caller-driven)

**Mechanism.** Caller (the receiver pipeline setup) passes an explicit
`WorkloadClass` enum to `ParallelDeltaApplier::with_strategy()`:

```rust
pub enum WorkloadClass {
    /// Small transfer (< 100 files). Fast path; ring stays at min.
    Small,
    /// Medium transfer (100 - 100K files). Default growth.
    Medium,
    /// Large transfer (> 100K files). Aggressive growth.
    Large,
    /// Adversarial / parallel-receive-delta (PIP-9.b path). Max growth.
    ParallelReceive,
}
```

The class maps to a `(min, max, growth_factor)` triple via a calibration
table. ROB-6's bench output (when it runs) populates the table. ROB-7's
spec proposes initial table values:

| class | min | max | growth_factor | rationale |
|---|---|---|---|---|
| `Small` | 16 | 64 | 1.5 | Single-handful workers; ring rarely contested. |
| `Medium` | 64 | 512 | 2.0 | Today's default as min; ceiling absorbs occasional large-file pressure. |
| `Large` | 128 | 2048 | 2.0 | Higher floor reduces fast-path force-insert under multi-file pressure. |
| `ParallelReceive` | 256 | `worker_count * 32` | 2.0 | PIP-9.b path optimised explicitly for adversarial ordering. |

**Feedback signals.** None - the class is a static hint. The
`AdaptiveCapacityPolicy` still uses utilization+gap-window predicates for
the grow/shrink decisions; the class only picks the policy's parameters.

**Cost.** No runtime cost beyond Option A. One enum branch at applier
construction.

**Pros.** Operator-legible. The class hint surfaces in logs ("running with
ParallelReceive policy: min=256 max=512"), so a tuning gone wrong can be
traced to a misclassification rather than to the policy internals.
Calibration is honest about the per-workload tradeoff.

**Cons.** Requires the caller to classify accurately. The receiver
pipeline does not always know file counts upfront (INC_RECURSE streams
arrive segment by segment). Misclassification produces silent
underutilisation rather than user-visible failure. The four-class shape is
itself a guess until ROB-6 numbers exist.

### 3.4 Recommendation matrix

| Option | Implementation cost | Default-on risk | Operator legibility | RSS safety |
|---|---|---|---|---|
| A | Lowest | Low (drop-in reuse) | Opaque | Memory-blind |
| B | Medium (RSS-poll plumbing) | Medium (extra feedback) | Same as A + RSS log | RSS-aware |
| C | Medium (class plumbing) | Medium (calibration risk) | High (class in logs) | Memory-blind |

ROB-7 does **not** pick. ROB-8 implements Option A first as the lowest-risk
shippable variant. ROB-9 benches A vs the fixed-64 baseline. If A wins, B
and C become follow-ups (B as the next default candidate, C as an operator
tuning surface). If A loses, B is implemented next on the theory that
memory-blind growth was the regression cause.

This staged approach matches `feedback_concurrent_path_discipline.md` (any
new concurrent code path ships with adversarial-ordering stress before
flipping default-on). ROB-8 ships behind feature flag; ROB-10 decides
flip; bench-without-feedback (A) ships first because it has the smallest
failure surface.

## 4. Default-on bake plan

Feature flag: `adaptive-reorder-ring` on the `engine` crate. Off by
default. Bake plan:

1. **ROB-8** lands the wiring behind the flag. `cargo test --features
   adaptive-reorder-ring` and `cargo bench --features
   adaptive-reorder-ring` build and pass. Default builds use the existing
   hard 64 / hard 2x ring sizes.
2. **ROB-9** runs the bench harness from ROB-6 on three workloads
   (100-file, 10K-file, 100K-file) at flag-on vs flag-off, capturing
   wall-clock, peak RSS, `force_insert_count`, `spill_activations` (where
   applicable), and `grow_events`. Numbers land in
   `docs/benchmarks/rob-9-adaptive-reorder-ring-results.md`.
3. **ROB-10** synthesises ROB-9 into a flip decision. Criteria for flip:
   - Wall-clock within +-2% of flag-off on the 100-file workload (no
     fast-path regression).
   - `force_insert_count` reduction of >= 30% on the 10K-file workload
     under adversarial chunk ordering.
   - Peak RSS overhead <= +5% over flag-off at all workload sizes.
   - Zero `spill_activations` regressions on the workloads where the
     pipeline ring is wired with `SpillableReorderBuffer`.
4. If criteria pass, ROB-10's flip PR sets the `engine` crate's
   `default-features` to include `adaptive-reorder-ring`. 14-day bake
   window begins. Memory note `project_reorder_capacity_hard_default`
   updated to SHIPPED with bake-window dates.
5. ROB-11 adds the `OC_RSYNC_REORDER_RING_CAP` env override per the
   ROB-1 audit's section 4.4 recommendation. The env override sets the
   policy's `min` (and `max` if adaptive is enabled). Parsing lands in
   `consumer/spawn.rs::from_config()` alongside the existing
   `apply_env_overrides` path. Default override is unset; setting it
   forces a non-adaptive ring at the supplied size regardless of feature
   flag state.

## 5. Rollback criteria

Trigger a rollback to the hard 64 default if any of the following land
during the 14-day bake window:

1. **CI bench regression**. ROB-13's CI cell (the regression-detection cell
   downstream of this spec) reports > 5% wall-clock regression on the
   nightly bench against the bake-start baseline. CI auto-comments on the
   ROB-10 flip PR; ROB-10 reverter opens the unflip PR within 24h.
2. **Production RSS regression**. Any user report of a >= 10% peak RSS
   increase on a workload comparable to the bench fixtures triggers
   investigation. If reproducible, rollback within 48h.
3. **Wire-byte corruption**. The PIP-7 cautionary case
   (`project_parallel_delta_apply_phase2`) is in scope. Any wire-byte
   divergence introduced by the adaptive path triggers immediate
   rollback, no investigation window.
4. **CapacityExceeded floor breach**. If the adaptive path produces
   `CapacityExceeded` at a higher rate than the fixed 64 baseline on any
   ROB-9 workload, the policy's `max` is too low or the grow predicate is
   not firing soon enough. ROB-10 holds the flip until the cause is
   addressed.

Rollback shape: revert the `default-features` change on `engine`. The
feature flag remains compilable; users with explicit `--features
adaptive-reorder-ring` can keep the adaptive path.

## 6. Feed-forward to dependent tasks

| Task | Inputs from ROB-7 spec | Owns |
|---|---|---|
| ROB-8 | Option A wiring shape (section 3.1); `min=64, max=worker_count*16, growth_factor=2.0` at the per-file ring; same wiring for the pipeline ring with `min=2*worker_count, max=8*worker_count` matching Formula A's adaptive ceiling. | Feature-gated implementation. |
| ROB-9 | Bench workload list (section 4 step 2); flip criteria (section 4 step 3). | Three-workload bench; design doc with numbers. |
| ROB-10 | Decision criteria (section 4 step 3); rollback shape (section 5). | Flip PR + memory note update. |
| ROB-11 | Env override insertion point (section 4 step 5; ROB-1 audit section 4.5). | `OC_RSYNC_REORDER_RING_CAP` parsing. |
| ROB-12 | Workload-class table for Option C (section 3.3) if Option A fails ROB-10. | User-facing docs on expected spill rate. |
| ROB-13 | CI bench cell tracking wall-clock, force_insert_count, grow_events, peak RSS. | CI regression detection cell. |

## 7. Open questions deferred to implementation

1. **Per-file vs shared policy instance.** ROB-1 section 5.5 asks whether
   the pipeline ring and the per-file ring should share a single
   `AdaptiveCapacityPolicy` instance. ROB-7 answers: **independent
   instances per ring**. Per-file pressure is decorrelated across files;
   per-pipeline pressure is correlated with stream backlog. A shared
   instance would average two uncorrelated signals into a single decision,
   producing worst-of-both behaviour. ROB-8 instantiates two policies.
2. **`min` floor under low worker count.** ROB-1 section 5.3 flags that
   Formula A produces `cap = 2` at `worker_count = 1`. ROB-7 raises the
   absolute floor to 8 for the pipeline ring so the adaptive `min` is at
   least 8 regardless of `worker_count`. The per-file ring's `min` stays
   at 64 because the audit's pressure model never produces sub-64 pressure
   in the fast path. ROB-8 implements `pipeline_min = max(2 *
   worker_count, 8)`.
3. **`force_insert` interaction with adaptive grow.** Today, the per-file
   ring's `CapacityExceeded` is funnelled into `force_insert_count`. With
   adaptive growth, the same path **first** attempts a grow before
   falling back to force-insert. The interaction must be sequential:
   `push()` returns `CapacityExceeded`, callsite invokes
   `try_grow_to_fit(ndx)` on the applier, retries `push()`, only then
   force-inserts on second failure. ROB-8 specifies the retry contract in
   code.
4. **Shrink behaviour mid-transfer.** The shrink predicate fires when the
   rolling window mean drops below 25%. For per-file rings, this is fine -
   shrinking reclaims memory as files complete. For the pipeline ring,
   shrink mid-transfer can cause a producer block on the very next burst.
   ROB-8 either disables shrink on the pipeline ring or doubles its
   `sample_window` to 64 so shrinks are more conservative.
5. **`BoundedReorderBuffer` (transfer crate).** ROB-1 section 5.4 flags
   this as adjacent. ROB-7 explicitly scopes it **out** - it uses
   `BTreeMap`, not the ring, and cannot reuse `AdaptiveCapacityPolicy`
   directly. If ROB-9 bench numbers show the `BoundedReorderBuffer` is
   actually on a hot path, ROB-12 files a follow-up; otherwise it stays
   on the hard `DEFAULT_WINDOW_SIZE = 64`.

## 8. Cross-references

- ROB-1 audit: `docs/audits/rob-1-reorder-ring-cap-audit.md` (cap formula,
  call sites, recommendations).
- ROB-4 audit: `docs/audits/rob-4-reorder-pressure-paths.md` (which
  transfer paths feed pressure into the rings).
- STN-6 (shipped): RSS-aware spill trigger. Option B reuses its RSS query.
- SRO-6 (shipped): one-shot warning on first spill activation. ROB-3
  extends to per-file ring.
- Memory note: `project_reorder_capacity_hard_default` (the smell that
  motivates this series).
- Memory note: `project_reorder_spill_fragility` (FS-error edges; ROB-7
  must not regress SPL-32/.33/.34's graceful degradation).
- Feedback note: `feedback_concurrent_path_discipline.md` (adversarial-
  ordering stress before flipping default-on; PIP-7 cautionary case).
- Feedback note: `feedback_design_systems_costs_upfront.md` (Option A
  picked first because it has the smallest failure surface, not because
  it is "best"; B and C are pre-spec'd so the cost is known upfront).
