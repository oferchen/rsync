# DMC-CON.4 - Alternative concurrent data structures for the delete workload

Date: 2026-06-11
Status: Design analysis (no implementation)
Tracker: DMC-CON.4 (#3998). Predecessors: DMC-CON.1 (#3995, contention
profile), DMC-CON.2 (#3996, adaptive shard heuristic spec), DMC-CON.3
(#3997, heuristic implementation). Companions:
`docs/design/dmc-con-adaptive-sharding.md` (DMC-CON.2/.3/.5),
`docs/design/dashmap-scalability-decision.md` (DMB.f decision framework),
`docs/design/dashmap-shard-contention-profile.md` (DMB.e shard model).

## 1. Purpose and scope

DMC-CON.1-.3 took the migration of `ParallelDeltaApplier::files` from
`Mutex<HashMap>` to `DashMap` and tuned the shard count to the applier's
own worker fan-out rather than the host's CPU count. Those tasks left
one open question on the table: **was DashMap the right data structure,
or would a different concurrent map serve the delete workload better?**

This document answers that question on paper. It does not change any
production code, does not add a benchmark, and does not promote a new
dependency. It compares the current DashMap usage against two named
alternatives - a hand-rolled sharded `Mutex<HashMap>` and a lock-free
skiplist (`crossbeam-skiplist`) - across the two delete-workload sites
that hold concurrent state today:

1. `DeletePlanMap` in `crates/engine/src/delete/plan_map.rs` -
   currently `Mutex<HashMap<PathBuf, DeletePlan>>`, kept on the
   single-mutex shape because no measured bottleneck has yet justified
   churn (the file's own rustdoc records the rationale).
2. `ParallelDeltaApplier::files` in
   `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` -
   `DashMap<FileNdx, SlotEntry>` since BR-3j (PRs #4634-#4636), with
   the adaptive shard count from DMC-CON.3.

The applier is not strictly a "delete" workload; it is the parallel
delta-apply path. It is included here because (a) DMC-CON.1-.3 framed
DashMap selection around it, and (b) the delete pipeline's emitter
plans live in the same concurrency regime - `N` rayon producers writing
into a shared map drained by `1` (delete) or `N` (applier) consumers.
The decision matrix in section 6 treats the two sites independently.

## 2. Workload profile

### 2.1 DeletePlanMap

Source: `crates/engine/src/delete/plan_map.rs:31-48`.

| Property | Value |
|---|---|
| Key type | `PathBuf` (heap-allocated, variable length) |
| Value type | `DeletePlan` (one `PathBuf` directory + `Vec<DeleteEntry>`) |
| Producer threads | `N` (rayon pool, typically 4-16 in production) |
| Consumer threads | `1` (the `DeleteEmitter` drain thread) |
| Insert frequency | One per content directory (phase 1) |
| Take frequency | One per content directory (phase 2) |
| Read-without-remove | None on the hot path; only tests/`Debug` |
| Iteration | None (`len`, `is_empty`, `contains` only) |
| Lifetime | Per-transfer; populated then drained |
| Steady-state size | Bounded by number of directories - typically 10^2-10^5; 10^6 possible at extreme scale |

**Access pattern**: write-heavy in phase 1, drain-heavy in phase 2,
overlap during the transition. Inserts come from disjoint keys (one
plan per directory, exactly one producer per plan), so producer-vs-
producer contention only happens at the shard level, not at the key
level. The single consumer never contends with itself.

The most contention-relevant moment is the phase-1/phase-2 overlap
window where one producer is publishing into the same map a consumer
is draining from. With a single global `Mutex<HashMap>`, every insert
and every take serialise through one lock; with shard-based maps
(DashMap, sharded `Mutex<HashMap>`), they only serialise when they
land on the same shard.

### 2.2 ParallelDeltaApplier::files

Source: `crates/engine/src/concurrent_delta/parallel_apply/mod.rs`
(per the DMB.f doc, lines 17-22).

| Property | Value |
|---|---|
| Key type | `FileNdx` (`#[repr(transparent)] u32`, `Copy + Hash + Eq`) |
| Value type | `SlotEntry` (currently `Arc<Mutex<FileSlot>>`-shaped) |
| Producer threads | `N` (rayon pool, typically 4-16) |
| Consumer threads | `N` (same pool; lookup is symmetric) |
| Register frequency | One per registered file (cold path) |
| Lookup frequency | One per chunk submitted (HOT path) |
| Finish frequency | One per finished file (cold path) |
| Iteration | None outside `Debug::fmt` (`.len()`) |
| Lifetime | Per-applier instance; spans the parallel delta-apply pass |
| Steady-state size | Bounded by `concurrency * pipeline_depth`; typically 10^2-10^3 in flight |

**Access pattern**: lookup-dominated. For every chunk the applier
dispatches, `slot_for(ndx)` runs once to clone the file's `Arc`. With
`N` workers and many chunks per file, lookups outnumber inserts by
two to three orders of magnitude. Register and finish bracket each
file's lifetime exactly once.

This is exactly the read-mostly, fine-grained symmetric pattern that
DashMap's per-shard `RwLock` is designed for - multiple readers on the
same shard proceed in parallel, where a sharded `Mutex<HashMap>` would
serialise them.

### 2.3 Cited references

- DMB.f section 4.1 (DeletePlanMap access pattern) -
  `docs/design/dashmap-scalability-decision.md`.
- DMB.f section 4.2 (Applier access pattern) - same file.
- BR-3j.a section "Access pattern summary" -
  `docs/audits/br-3j-a-dashmap-vs-sharded-2026-05-20.md`.
- DeletePlanMap rustdoc (sections "Concurrent Map Choice", `NOTE(DDP-B4)`)
  - `crates/engine/src/delete/plan_map.rs:9-29,45-48`.

## 3. DashMap baseline characteristics

Numbers below restate the DMB.e/.f and DMC-CON.2 model. They are the
basis against which alternatives are compared in sections 4 and 5.

### 3.1 Per-op latency

From DMB.f section 5.1 (theoretical model, uncontended):

| Operation | DashMap | Mutex<HashMap> | Delta |
|---|---|---|---|
| Insert | ~40 ns (hash + shard + RwLock-write + HashMap-insert) | ~30 ns (Mutex + HashMap-insert) | +10 ns |
| Lookup | ~40 ns | ~30 ns | +10 ns |
| Remove | ~40 ns | ~30 ns | +10 ns |

The 33% per-op penalty under no contention is the cost DashMap must
amortise via parallel shard access at higher thread counts. DMB.f
predicts crossover at 4-8 threads for both targets.

### 3.2 Memory overhead

From DMB.f section 6 and DMC-CON.2 section 2.4:

| Shard count | Empty DashMap fixed cost | Notes |
|---|---|---|
| 32 (8-core default) | ~2-3 KB | 32 RwLocks + 32 empty HashMaps |
| 128 (16-core default) | ~9 KB | 128 RwLocks + 128 empty HashMaps |
| 1024 (MAX cap) | ~72 KiB | Worst case under DMC-CON.2 heuristic |

At 100K-1M populated entries, the per-shard fixed cost is < 0.5% of
total RSS. Memory is not a decision factor at production scale (DMB.f
section 6.2).

### 3.3 Tuning knobs (post-DMC-CON.3)

- `shard_count = (worker_count * 4).next_power_of_two().clamp(4, 1024)`
  (DMC-CON.2 section 2).
- `OC_RSYNC_DASHMAP_SHARDS=<n>` operator override (DMC-CON.2 section 3).
- DashMap version pinned at `6.1` workspace-wide
  (`Cargo.toml:244`).

### 3.4 Operational observations from DMC-CON.1-.3

- Applier's adaptive shard count tracks **worker concurrency**, not
  host CPU count - the original `available_parallelism() * 4` default
  was wrong for low-worker callers (test/bench/concurrency=1) and for
  very-high-core hosts with modest applier fan-out.
- `DeletePlanMap` was deliberately left on `Mutex<HashMap>` because
  DDP-B4 has not yet produced bench evidence of a bottleneck (the
  file's own rustdoc records this).
- DashMap's `Entry` API removes a TOCTOU window in `register_file`
  that any non-`Entry` alternative would need to re-derive with held
  locks (BR-3j.a "Gotchas" section 1).

## 4. Alternative: sharded-by-hash `Mutex<HashMap>`

### 4.1 Shape

```text
ShardedMap<K, V> {
    shards: Box<[Mutex<HashMap<K, V>>]>,
    hasher: RandomState,
}

shard_for(k) = hash(k) & (N - 1)   // N is power-of-two
```

For `FileNdx` keys (dense `u32`), the hash can be skipped entirely
(BR-3j.a side-by-side, row "Lookup-time hashing"):

```text
shard_for(ndx) = (ndx.get() as usize) & (N - 1)
```

This is the shape the existing `ShardedMutexStore` already uses in
`crates/engine/benches/delete_plan_map_contention.rs:151-193`, with
`SHARD_COUNT = 16`.

### 4.2 Per-op latency

From DMB.f model (uncontended) + BR-3j.a comparison row "Read under
contention":

| Operation | Sharded `Mutex<HashMap>` | DashMap delta |
|---|---|---|
| Lookup, uncontended | ~30 ns (Mutex + HashMap-get) | -10 ns vs DashMap |
| Lookup, same-shard concurrent | Serialises (Mutex) | DashMap's `RwLock` lets reads parallel - **DashMap wins** |
| Lookup, different-shard concurrent | Parallel | Equal |
| Insert, uncontended | ~30 ns | -10 ns vs DashMap |
| Insert, same-shard concurrent | Serialises | Equal (DashMap also write-locks the shard) |
| Insert, different-shard concurrent | Parallel | Equal |

The asymmetry matters for the **applier** (read-heavy: same-shard
reads serialise under sharded `Mutex` but parallelise under DashMap)
and is muted for **DeletePlanMap** (write-heavy phase 1, drain-heavy
phase 2 - no same-shard concurrent reads on the hot path).

### 4.3 Memory overhead

Lower than DashMap at the same shard count: one `std::sync::Mutex`
(~48 bytes including poison flag) is comparable to one DashMap shard's
`RwLock` (~8 bytes via `parking_lot_core`), but the sharded variant
typically uses fewer shards (BR-3j.a row "Memory overhead (empty)"
records 8 vs 32 on an 8-core box). For the applier under DMC-CON.3,
shard count tracks worker count, so the gap narrows further.

At 100K-1M entries, the per-shard fixed cost is irrelevant against
entry storage. Not a decision factor.

### 4.4 API ergonomic fit

- **DeletePlanMap**: a direct swap. The trait surface in
  `delete_plan_map_contention.rs` (`PlanStore::insert`, `take`) is
  already implemented for `ShardedMutexStore`. The current
  `DeletePlanMap` exposes `insert`/`take`/`is_empty`/`len`/`contains`;
  each maps to one shard lookup plus the inner HashMap op.
- **ApplierFiles**: equivalent in shape (BR-3j.a section "Side-by-side
  comparison", row "API ergonomics"), but the TOCTOU window noted in
  BR-3j.a "Gotchas" 1 returns: `register_file` cannot use a DashMap-
  style `Entry::Occupied/Vacant` in one call. The atomic
  "register exactly once" invariant is restored by holding the shard
  lock across `contains_key` + `insert`, which the sharded variant
  already does by construction; the change is mechanical but adds
  ~3 LoC vs DashMap.

### 4.5 Existing crate availability and maturity

No new dependency. Hand-rolled, std-only. The bench harness already
contains a working implementation (`delete_plan_map_contention.rs:151-
193`); promoting it from bench to production code would be a copy-paste.

### 4.6 Poison behaviour

`std::sync::Mutex` poisons on panic. Two layers of `std::Mutex` (the
shard mutex plus the per-file `FileSlot` mutex on the applier) each
need a poison-mapping arm. This is the **current** state for
`DeletePlanMap` (`plan_map.rs` already maps poison to `expect`-panic)
and is one extra rung relative to DashMap, where the outer map cannot
poison (BR-3j.a row "Mutex-poisoning surface").

For the applier specifically, the BR-3j.c migration removed the outer
"parallel applier file map poisoned" string. A revert to sharded
`Mutex<HashMap>` would reintroduce it - a small but real surface
expansion.

### 4.7 When this wins

- **Applier under low concurrency** (`worker_count <= 2`): the 10 ns
  per-op overhead of DashMap's shard selection is unamortised by any
  parallel-shard benefit at that fan-out. DMC-CON.2 already clamps
  shards to `MIN_SHARDS = 4` at low worker counts; a sharded
  `Mutex<HashMap>` with 1-2 shards would be even cheaper. The trade-
  off: 1-shard sharded `Mutex` is literally `Mutex<HashMap>`, which
  is already an option for that regime (see section 5).
- **DeletePlanMap at 8+ producers** when DMB.c eventually shows
  Mutex contention > 15% of phase-1 wall clock: sharded `Mutex` would
  partition contention without the DashMap dep promotion that DDP-B4
  was created to evaluate.

### 4.8 When this loses

- **Applier under typical production concurrency** (8-16 workers): the
  read-heavy lookup mix benefits from DashMap's per-shard `RwLock`
  parallel-read property. Sharded `Mutex<HashMap>` does not have it.
- **DeletePlanMap when contention is not the bottleneck**: a sharded
  map at low producer counts is strictly more code than the single
  `Mutex<HashMap>` shape today, with no measurable throughput gain.

## 5. Alternative: lock-free skiplist (`crossbeam-skiplist`)

### 5.1 Shape

```text
SkipList<K, V> {
    inner: crossbeam_skiplist::SkipMap<K, V>,
}
```

`SkipMap` is the concurrent skiplist analog of `BTreeMap`: ordered by
key, lock-free for both reads and writes via per-node atomic pointers.
Lookups and inserts are `O(log N)` expected, not `O(1)`.

### 5.2 Per-op latency

`crossbeam-skiplist`'s own README and the broader literature place
per-op cost at **3-10x slower than a sharded hash-based map** under
low contention, because:

- Lookup walks `O(log N)` nodes, each with an atomic load + tag check.
- Inserts allocate a new node (heap alloc per op, vs HashMap's
  amortised `O(1)` bucket placement).
- Removes do not free the node immediately; they need epoch-based
  reclamation (crossbeam's `epoch` crate) to avoid use-after-free with
  concurrent readers.

| Operation | crossbeam-skiplist (uncontended) | DashMap delta |
|---|---|---|
| Lookup | ~120-400 ns (`log N` node walk + atomics) | +80 to +360 ns vs DashMap |
| Insert | ~200-600 ns (alloc + `log N` linkage) | +160 to +560 ns |
| Remove | ~200-600 ns (mark + epoch-defer free) | +160 to +560 ns |

These are order-of-magnitude estimates from the crossbeam crate's
public bench results; precise numbers depend on key type and N. For
the delete workload's key types (`PathBuf`, `FileNdx`), they suggest
the skiplist is between 3x and 10x slower per op than DashMap, which
no plausible level of contention can amortise. The skiplist's
advantage is *unbounded scaling under contention*, not single-op cost.

### 5.3 Memory overhead

Higher than both alternatives. Per-entry overhead includes:

- One node allocation per entry (vs HashMap's shared bucket array).
- Per-node `Atomic<*mut Node>` next/prev pointers, one per skiplist
  level. With expected level depth ~`log2(N)`, an entry at level 4
  carries ~80 bytes of overhead before the K/V.
- Epoch-based reclamation: deferred frees mean transient overhead can
  reach ~`active_writers * batch_size` extra nodes.

At 100K entries this is on the order of 8-16 MB extra RSS vs DashMap
(8 levels * 8 bytes * 100K = 6.4 MB, plus reclamation deferred batches).
At 1M, 60-160 MB extra. The applier's typical in-flight size
(10^2-10^3) makes this a non-issue for that site; the DeletePlanMap
can grow to 10^6 at extreme scale where the overhead **does** matter.

### 5.4 API ergonomic fit

- **DeletePlanMap**: `PathBuf` is `Ord`, so `SkipMap<PathBuf,
  DeletePlan>` compiles. The `insert`/`remove` shapes match. There
  is no `Entry::Vacant` equivalent that returns the slot atomically;
  the `compare_insert` API exists but is more verbose than DashMap's
  `Entry`.
- **ApplierFiles**: `FileNdx` is `Ord`. Same story. The applier's
  `Arc<Mutex<FileSlot>>` clone-on-lookup pattern is supported via
  `SkipMap::get` returning an `Entry<'_, K, V>` guard that borrows
  the value; clone the `Arc` and drop the guard immediately, same
  rule as DashMap's `Ref`.

The skiplist also supports **ordered iteration** (which neither
DashMap nor sharded `Mutex<HashMap>` do efficiently), but neither
delete workload site iterates ordered today. This advantage is wasted.

### 5.5 Existing crate availability and maturity

`crossbeam-skiplist` is published under the crossbeam project. Status
(2026-06-11):

- Workspace already pulls `crossbeam-channel` and `crossbeam-queue`
  (`Cargo.toml:242,273`); `crossbeam-skiplist` is a sibling crate but
  **not currently in the workspace**.
- `crossbeam-skiplist` `0.1.x` is the current stable line. The crate
  has shipped since 2019 but has had fewer prod-deployment hours than
  DashMap. Its real production users are smaller and the surface area
  is narrower (no `Entry`-style atomic upsert).
- MSRV is `1.61` per `crossbeam-skiplist 0.1`, below the workspace
  MSRV of `1.88` - no blocker.
- Pulls in `crossbeam-epoch` as a transitive dependency. The epoch
  crate is a known source of subtle behaviour around dropped pinned
  guards; mishandled, it leaks memory until process exit.

### 5.6 Poison behaviour

No mutex layer to poison. Panic during insert leaves the skiplist in
a recoverable state (epoch reclamation handles in-flight nodes). This
is a small simplification, but does not compensate for the per-op
cost penalty.

### 5.7 When this wins

- **Pathological contention regimes** where every thread is fighting
  for the same shard and the shard's `RwLock` write queue grows
  unboundedly. The skiplist has no shards, so it has no shard-pinned
  hot spots. The applier's adaptive shard count (DMC-CON.2) makes
  this unlikely in production.
- **Workloads that need ordered iteration** (range queries, lowest-
  key drain). Neither delete workload site needs this.

### 5.8 When this loses

- **The common case**: 3-10x per-op slowdown for both targets,
  swamping any reduction in contention wait.
- **At 1M scale on the DeletePlanMap**: 60-160 MB extra RSS would
  meaningfully widen the open `project_rss_3_11x_upstream` gap.

## 6. Decision matrix

Three candidates evaluated against the two sites. Cells encode
"recommended", "viable but worse", or "rejected".

| Candidate | DeletePlanMap | ApplierFiles |
|---|---|---|
| **DashMap (current)** | Viable but worse (no measured bottleneck, dep promotion cost) | **Recommended** (read-heavy benefits from `RwLock`-per-shard; DMC-CON.3 tunes shards) |
| **Sharded `Mutex<HashMap>`** | Viable; would be a strict win over `Mutex<HashMap>` at 8+ producers, neutral at 1-4 (no dep added) | Viable but worse than DashMap at production fan-out (loses parallel-read on same shard); marginally better below `worker_count = 2` |
| **`Mutex<HashMap>` (current for DeletePlanMap)** | **Recommended** (no measured bottleneck, simplest correct shape) | Rejected (single mutex caps applier scaling, BR-3j's original problem) |
| **crossbeam-skiplist** | Rejected (3-10x per-op slowdown; +60-160 MB at 1M scale) | Rejected (3-10x per-op slowdown on a hot lookup path) |

## 7. Recommended path

### 7.1 DeletePlanMap: keep `Mutex<HashMap>`

No change. The site has no measured bottleneck (the file's own
rustdoc records this), and the DDP-B4 bench harness already exists
to capture evidence if one appears
(`crates/engine/benches/delete_plan_map_contention.rs`). The fall-
back path, if DMB.c eventually shows > 15% contention at 8+
producers, is to migrate to the **sharded `Mutex<HashMap>`** that the
bench already prototypes - not DashMap. The reason: DashMap's
parallel-read advantage is unused at this site (single consumer
never races itself; producers register disjoint keys), and the dep
promotion is unjustified without a corresponding ergonomic gain.

### 7.2 ApplierFiles: keep `DashMap` with DMC-CON.3 shard sizing

No change. DMC-CON.1-.3 already validated DashMap for this site and
tuned its shard count to the applier's worker fan-out. The two
alternatives in this document underperform here: sharded
`Mutex<HashMap>` loses parallel-same-shard reads on a read-heavy
workload, and crossbeam-skiplist's per-op cost makes it a non-starter.

### 7.3 What would change this recommendation

Per-site triggers, in priority order:

| Site | Trigger | Action |
|---|---|---|
| DeletePlanMap | DMB.c shows `Mutex<HashMap>` contention > 15% of phase-1 wall clock at production producer counts (8-16) | Migrate to sharded `Mutex<HashMap>` (not DashMap). 16 shards is the existing bench default; tune via the same heuristic as DMC-CON.2 if the producer count is materially higher. |
| DeletePlanMap | Future site emerges that needs ordered drain over the map | Re-evaluate skiplist. Today nothing needs it. |
| ApplierFiles | DMB.c shows DashMap throughput delta < 1.3x at 16 threads (DMB.f section 7.1 keep-criterion fails) | Apply DMB.f section 8.1 revert plan: replace DashMap with `Mutex<HashMap>`. The sharded variant is not recommended here - the applier's read-heavy mix specifically benefits from DashMap's per-shard `RwLock`. |
| Either site | DashMap publishes a known soundness or scaling regression in a future release | Apply DMB.f section 10.2 version-bump protocol; if the regression is real, the sharded `Mutex<HashMap>` is the documented fallback for both sites. |

### 7.4 What this recommendation **does not** do

- It does not add `crossbeam-skiplist` to the workspace. The per-op
  cost makes it unsuitable for either site under any plausible load.
- It does not add a feature flag for "experimental skiplist backing".
  A flag would commit the team to maintaining the alternative without
  a workload that exercises its advantages.
- It does not change the DDP-B4 bench layout. The three-way contest
  (`mutex_hashmap`, `dashmap`, `sharded_mutex_hashmap`) is already
  the right shape for this question; skiplist would be a fourth
  candidate the bench can absorb if the trigger conditions in 7.3
  ever fire.

## 8. Bench harness sketch (only if 7.3 triggers fire)

If DMB.c results justify adding **crossbeam-skiplist** to the
existing three-way contest as a fourth candidate, the trait shape
already exists (`PlanStore` in `delete_plan_map_contention.rs:86-89`).
A fourth strategy is purely additive:

### 8.1 Trait impl

```rust
use crossbeam_skiplist::SkipMap;

struct SkiplistStore {
    inner: SkipMap<PathBuf, DeletePlan>,
}

impl SkiplistStore {
    fn with_capacity(_capacity: usize) -> Self {
        // SkipMap does not preallocate.
        Self { inner: SkipMap::new() }
    }
}

impl PlanStore for SkiplistStore {
    fn insert(&self, plan: DeletePlan) -> Option<DeletePlan> {
        let key = plan.directory.clone();
        // SkipMap::insert overwrites and does not return the prior value;
        // emulate the existing API by removing first.
        let prior = self.inner.remove(&key).map(|e| e.value().clone());
        self.inner.insert(key, plan);
        prior
    }

    fn take(&self, dir: &Path) -> Option<DeletePlan> {
        self.inner.remove(dir).map(|e| e.value().clone())
    }
}
```

The two-step `remove` + `insert` in `insert()` loses atomicity but
preserves the `Option<previous>` shape the trait promises. A true
production migration would use the typed
`SkipMap::compare_insert` to keep insert atomic; the bench shape
above is intentionally simplified to keep the harness honest about
the per-op cost without engineering around it.

### 8.2 Bench list extension

```rust
const STRATEGIES: &[&str] = &[
    "mutex_hashmap",
    "dashmap",
    "sharded_mutex_hashmap",
    "crossbeam_skiplist",
];

fn build_store(strategy: &str, capacity: usize) -> Arc<dyn PlanStore> {
    match strategy {
        // ... existing arms ...
        "crossbeam_skiplist" => Arc::new(SkiplistStore::with_capacity(capacity)),
        other => panic!("unknown strategy: {other}"),
    }
}
```

### 8.3 Decision thresholds for adopting the skiplist

The skiplist must clear all three (mirroring DMB.f section 7.1):

| Criterion | Threshold |
|---|---|
| Per-op cost vs DashMap, single thread | Ratio <= 2x (today's prediction is 3-10x) |
| Throughput at 16 threads | Within 10% of DashMap |
| Memory overhead at 100K entries | < 5 MB extra (today's prediction is ~6-12 MB) |

If any criterion fails, the skiplist is rejected and the bench
candidate is removed. None of these are plausible at the data-
structure's expected per-op cost; the bench exists to confirm the
prediction is right, not to invite the alternative.

### 8.4 Applier-side bench

The applier bench is `br_3j_f_dashmap_cores_vs_throughput.rs`
(already in `crates/engine/benches/`). It can be extended with a
fourth strategy in the same shape as 8.1, with `FileNdx` keys
instead of `PathBuf`. Order-of-magnitude expectations: the skiplist's
per-op slowdown is amplified by the applier's lookup-heavy mix,
since lookups dominate the wall-clock cost and the skiplist's
per-op penalty is most acute on lookups.

## 9. Cross-references

- `crates/engine/src/delete/plan_map.rs` - DeletePlanMap production
  code; rustdoc section "Concurrent Map Choice" documents the current
  decision to stay on `Mutex<HashMap>`.
- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` -
  ParallelDeltaApplier files DashMap usage.
- `crates/engine/src/concurrent_delta/parallel_apply/shard_sizing.rs`
  - DMC-CON.3 adaptive shard count.
- `crates/engine/benches/delete_plan_map_contention.rs` - DDP-B4
  three-way bench harness; section 8 sketches the fourth-candidate
  extension.
- `crates/engine/benches/br_3j_f_dashmap_cores_vs_throughput.rs` -
  applier-side throughput bench.
- `docs/design/dmc-con-adaptive-sharding.md` - DMC-CON.2/.3 spec.
- `docs/design/dashmap-scalability-decision.md` - DMB.f outer
  decision framework; section 7.1 keep-criterion, section 8.1 revert
  plan, section 10.2 version-bump protocol.
- `docs/design/dashmap-shard-contention-profile.md` - DMB.e shard
  cost model used for the DashMap baseline in section 3.
- `docs/audits/br-3j-a-dashmap-vs-sharded-2026-05-20.md` - DashMap
  vs sharded `Mutex<HashMap>` comparison for the applier site; the
  side-by-side table in section "Side-by-side comparison" is the
  source for many of the per-row claims in section 4.
- `Cargo.toml:242-273` - existing crossbeam crates in the workspace
  (`crossbeam-channel`, `crossbeam-queue`); `crossbeam-skiplist` is
  not currently in the workspace.
