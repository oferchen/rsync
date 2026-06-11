# DMC-CON.4 - Alternative data structures for `ParallelDeltaApplier.files`

Date: 2026-06-10
Status: Decision audit (read-only, no code changes)
Tracker: DMC-CON.4. Predecessors: DMC-CON.1 (contention profile),
DMC-CON.2/.3 (adaptive shard sizing, PR #5604), DMB.f
(`docs/design/dashmap-scalability-decision.md`), BR-3j.a (DashMap
selection, `docs/audits/br-3j-a-dashmap-vs-sharded-2026-05-20.md`).

## 1. Question

DMC-CON.3 (PR #5604) added adaptive shard sizing for the
`ParallelDeltaApplier::files` `DashMap` so its shard count tracks the
applier's worker count rather than `available_parallelism()`. That fix
addresses the wrong-input axis. DMC-CON.4 asks the orthogonal question:
**is DashMap the right data structure at all**, or does a different
concurrent map shape close the residual gap better than tuning DashMap
ever can?

The applier's access pattern (see BR-3j.a section "Access pattern
summary"):

- `register_file`: one insert per file, cold path.
- `slot_for`: one read-and-clone-`Arc` per chunk, hot path, called from
  every `apply_chunk_parallel` / `apply_batch_parallel` invocation.
- `finish_file`: one remove per file, cold path.
- No iteration in production code; `Debug::fmt` reads `.len()` only.
- Value type: `SlotEntry` (carries `Arc<SlotData>` + `Arc<BarrierState>`).
- Key type: `FileNdx` (a `#[repr(transparent)] u32`, dense small integers,
  monotonically allocated per transfer).

The N-thread symmetric read-mostly-with-write-bookend shape is the
workload every concurrent map paper benchmarks. The candidates below are
the credible alternatives in 2026 Rust.

## 2. Candidates

### 2.1 `dashmap::DashMap` (current baseline)

- **Shape:** `N` shards, each a `RwLock<HashMap>`. Default
  `available_parallelism() * 4` shards; DMC-CON.3 retunes to
  `worker_count * 4`.
- **Pros:** drop-in API close to `HashMap`. Already a workspace
  dependency, used by daemon (`concurrent-sessions`). `Entry` API gives
  atomic insert-if-vacant. `parking_lot_core` locks never poison.
  Mature 6.x stable line.
- **Cons:** residual shard contention at peak fan-out (DMC-CON.1
  finding). Adaptive sharding (DMC-CON.3) bounds the worst case but
  cannot eliminate same-shard collisions: with `N` threads and
  `N * 4` shards the per-op collision probability stays around 22%.
  `Ref` guard footgun (deadlock if held across re-entry into the same
  shard) is a known caller hazard.
- **Memory per entry:** baseline (1.0x). Per-shard fixed cost is
  ~72 B/shard for an empty shard; DMC-CON.3's 1024-shard cap keeps that
  total under ~72 KiB.

### 2.2 Hand-rolled sharded `Arc<[Mutex<HashMap<FileNdx, SlotEntry>>; N]>`

- **Shape:** caller-owned shard array, shard index =
  `(ndx.get() as usize) & (N - 1)`. No hashing for shard pick (dense
  `FileNdx` keys are already uniform).
- **Pros:** zero new dependencies, pure `std`. Explicit shard count
  (one constant). Cheaper shard selection (mask vs hash + modulo).
  Behaviour is transparent to readers of the code.
- **Cons:** same-shard collisions serialise via `Mutex`, where DashMap
  serialises via `RwLock`. Readers on the same shard cannot proceed in
  parallel - a regression vs DashMap for the read-and-clone-`Arc` hot
  path. Hand-rolled atomic insert-if-vacant must re-derive the `Entry`
  TOCTOU guarantee that DashMap provides natively. Mutex-poisoning
  surface returns (every shard is a `std::sync::Mutex`).
- **Memory per entry:** ~0.9x (one `Mutex` per shard instead of one
  `RwLock`; saves ~8 B/shard but the difference is in the noise at
  populated scale per BR-3j.a section "Memory overhead").

### 2.3 `crossbeam-skiplist::SkipMap<FileNdx, SlotEntry>`

- **Shape:** lock-free concurrent skip list. Each entry is a heap-
  allocated node with `O(log N)` expected forward/backward pointers.
- **Pros:** truly lock-free. No shard contention, no `Ref` guard
  footgun. Iteration is concurrent-safe with O(N) cost.
- **Cons:** ~2-3x memory per entry vs a hash map (skip-list node
  carries multiple `Atomic<*mut Node>` forward pointers plus `Crossbeam`
  epoch reclamation metadata). `O(log N)` lookup vs hash map's O(1)
  amortised. Crossbeam-skiplist's benchmarks show wins only above
  ~32 concurrent threads on uniform-key workloads; the applier is
  routinely below that on production hosts (BR-3j.a documented
  `available_parallelism()` at 8-16 on typical production hardware).
  Insert is markedly slower than hash map under any thread count.
- **Memory per entry:** ~2.5x (skip-list node overhead dominates).

### 2.4 `papaya::HashMap<FileNdx, SlotEntry>`

- **Shape:** lock-free concurrent hash map, Java `ConcurrentHashMap`-
  style. Uses epoch-based reclamation (via `seize` crate).
- **Pros:** lock-free reads with no shard contention. API close to
  `HashMap`. Recent benchmarks (papaya README, 2026) claim 2-4x read
  throughput over DashMap at 32+ threads and parity at 8 threads.
  No `Ref` guard hazard - lookups return a `Guard`-scoped reference
  but the guard's lifetime is independent of any single bucket lock.
- **Cons:** newer crate (0.x line as of 2026-06), smaller ecosystem
  footprint than DashMap. Workspace would acquire a new transitive
  dep (`seize`). Insert/remove paths still pay an epoch-reclamation
  cost on hot paths. Behaviour at peak fan-out is well-understood in
  the published benchmarks but unmeasured for our applier's specific
  mix (one insert per file at register, N reads per chunk, one remove
  per file at finish). Maturity gap matters here because the applier
  is a correctness-critical receiver path - a regression in a
  papaya release could silently corrupt a transfer.
- **Memory per entry:** ~1.1x (lock-free buckets carry small per-entry
  atomic metadata).

### 2.5 `flurry::HashMap<FileNdx, SlotEntry>`

- **Shape:** Rust port of Java `ConcurrentHashMap`. Lock-free reads,
  lock-striped writes.
- **Pros:** same theoretical read advantages as papaya. Older project
  than papaya (started ~2020).
- **Cons:** development cadence on flurry has slowed since 2023; the
  papaya project has overtaken it for new work. API is awkward in
  current Rust (requires explicit `Guard`s from `flurry::epoch`).
  Workspace would acquire `seize`-equivalent transitive deps.
- **Memory per entry:** ~1.2x.

Verdict for 2.5: dominated by papaya on every axis (cadence, API,
benchmark numbers). Drop from further consideration; if a lock-free
hash map is the answer, papaya is the candidate.

### 2.6 Per-thread map + barrier merge

- **Shape:** each rayon worker keeps a thread-local `HashMap`. A barrier
  merges them when cross-thread visibility is required (e.g. at
  `finish_file` shutdown).
- **Pros:** zero contention per access. Cache-line ownership per worker
  is clean.
- **Cons:** structurally incompatible with the applier's access pattern.
  `slot_for` is the hot path and lookups are **random-dispatch**:
  worker `i` may receive a chunk for file `f` that worker `j` registered.
  A per-thread map cannot answer worker `i`'s `slot_for(f)` query
  without consulting worker `j`'s map, which requires either a global
  merge or a cross-thread message - both of which re-introduce
  exactly the contention this design tries to avoid. The pattern works
  for write-only sharded accumulators (each worker writes its own
  bucket) but the applier's read-and-clone-`Arc` shape is the opposite
  workload.
- **Memory per entry:** N/A (would store the same entry in multiple
  per-thread shadows, ballooning total RSS).

Verdict for 2.6: rejected on access-pattern mismatch, not on
implementation effort.

## 3. Comparison matrix

Numbers are reasoned estimates against the BR-3j.a access pattern (read-
heavy, dense `u32` keys, no iteration in production code), normalised
to DashMap-with-DMC-CON.3-adaptive-shards as the 1.0x baseline.
**Estimated, not measured** - DMB.c/d numbers are still pending offline
capture per DMB.f section 2.5 and the dmb_a_dashmap_delete_bench is the
nightly harness that will populate measured cells.

| Structure | Memory/entry | Insert TP @ 8t | Insert TP @ 32t | Lookup TP @ 8t | Lookup TP @ 32t | Maturity | Recommendation |
|---|---|---|---|---|---|---|---|
| `dashmap::DashMap` (DMC-CON.3) | 1.0x | baseline | baseline | baseline | baseline | mature (6.x stable, daemon prod usage) | **keep** + adaptive shard |
| Sharded `Arc<[Mutex<HashMap>]>` | 0.9x | -10% (Mutex vs RwLock for same-shard hits) | +5% (less per-op fixed cost) | -15% (Mutex blocks parallel reads) | -10% | mature (std only) | reject (cost > benefit on the read path) |
| `crossbeam-skiplist::SkipMap` | 2.5x | -30% (node alloc per insert) | +5% | -25% (`O(log N)` vs `O(1)`) | -15% | mature | reject (memory + cold-path lookup cost) |
| `papaya::HashMap` | 1.1x | -5% (epoch overhead) | +25% (no shard contention) | +5% (lock-free reads) | +40% (paper-grade scaling) | new (0.x as of 2026-06) | candidate for DMC-CON.5+ follow-up; needs measured bench |
| `flurry::HashMap` | 1.2x | -5% | +20% | +0% | +30% | stalled cadence | reject (dominated by papaya) |
| Per-thread map + barrier merge | N/A (multi-shadow) | N/A | N/A | N/A | N/A | N/A | reject (access-pattern mismatch) |

### 3.1 Read of the matrix

- DashMap-with-DMC-CON.3 dominates every alternative for the
  `worker_count <= 16` regime that BR-3j.a identified as production
  hardware. The lookup row at 8t shows DashMap leading every challenger
  except papaya (tied within estimation noise).
- The only structurally interesting challenger at the read tail
  (32+ threads) is papaya, where lock-free reads pay off vs DashMap's
  per-shard `RwLock`. The +40% estimate at 32t is the headline of
  the candidate's own benchmarks; whether it transfers to the
  applier's mix requires bench evidence the audit cannot fabricate.
- Sharded `Arc<[Mutex<HashMap>]>` is worse than DashMap on every
  read-path cell because it converts DashMap's parallel-readers-on-
  same-shard guarantee into mutual exclusion. The 0.1x memory
  savings does not buy back the read-path regression.
- crossbeam-skiplist is rejected on two independent axes: 2.5x memory
  per entry breaks the RSS-A budget at the million-file tier, and
  `O(log N)` lookup is a structural regression vs hash map on the
  applier's hot path.

## 4. Decision

**Keep DashMap + DMC-CON.3 adaptive shard sizing.** No migration
justified at current production scale (8-16 worker concurrency).

### 4.1 Rationale

1. The BR-3j migration ranked DashMap on **read-and-clone-`Arc` under
   `RwLock` shared reads** as the deciding criterion. DMC-CON.3 closes
   the wrong-input axis. The matrix above shows no challenger beats
   DashMap on that exact criterion below 32 concurrent workers, and
   the applier dispatches into `concurrency` workers sized off
   `available_parallelism()` which is 8-16 on the hosts BR-3j.a
   documented.
2. Memory budget. The applier's `files` map is one of several maps
   the receiver holds; the RSS-A hardening track (`project_rss_arena_
   hardening.md`) is the constraining envelope. A 2.5x per-entry cost
   from crossbeam-skiplist or a 1.2x cost from flurry conflicts with
   that envelope directly.
3. Maturity. The applier is the receiver-side delta path; a
   correctness regression there silently corrupts a transfer. The
   PIP series cautionary tales (`feedback_concurrent_path_discipline.
   md`) reinforce that any new concurrent-path code ships with
   adversarial-ordering stress fuzz BEFORE the default flips. papaya
   has not paid that cost in our tree, while DashMap has been the
   daemon's `concurrent-sessions` workhorse for multiple releases.
4. Cost of churn. Migrating the applier's map type touches every
   public-API surface BR-3j.a abridged at sketch level
   (`register_file`, `slot_for`, `finish_file`, `Debug::fmt`,
   `bytes_written`). That churn is justifiable only if a measured
   bench shows a win the audit cannot demonstrate from first
   principles.

### 4.2 Rollback / revisit criteria

Open DMC-CON.5+ to migrate the applier to `papaya::HashMap` if **all
three** of the following land:

1. A `dmb_*_papaya_applier_bench.rs` harness (cloned from
   `crates/engine/benches/delete_plan_map_contention.rs`) shows
   papaya **>= 1.30x** lookup throughput over DashMap-with-DMC-CON.3
   at 32 worker threads on the applier's exact insert/lookup/remove
   mix at 100K files, with **<= 1.2x** memory per entry.
2. Production telemetry (or a representative interop bench) shows
   the applier routinely operating at **>= 32 concurrent workers** -
   the regime where papaya's win in (1) actually matters. As of
   2026-06-10, no measured production deployment hits that bar.
3. papaya 1.0 ships with a stable epoch-reclamation API and at least
   one full release cycle of bug-fix backports - the maturity gate
   the BR-3j.a "Decision driver" column applied to DashMap.

If only one or two of the three land, DMC-CON.4 stays "no migration".
Document the missing gate in a follow-up addendum to this audit.

### 4.3 What this audit does NOT do

- It does not measure papaya, flurry, or crossbeam-skiplist against
  the applier's bench harness. The numbers in section 3 are estimates;
  the DMB.c/d nightly harness is the canonical place to land measured
  cells. CLAUDE.md (rule 12) requires surfacing this rather than
  asserting numbers we did not run.
- It does not change any code. DMC-CON.3 (PR #5604) is the active
  implementation track for the DashMap shard count; DMC-CON.4 is
  read-only.
- It does not retire the BR-3j sharded `Mutex<HashMap>` rejection
  recorded in `docs/audits/br-3j-a-dashmap-vs-sharded-2026-05-20.md`.
  Section 2.2 above only extends that rejection forward to the
  post-DMC-CON.3 baseline.

## 5. Operator implications

None. The `files` map structure stays unchanged. Operators continue to
have `OC_RSYNC_DASHMAP_SHARDS` (from DMC-CON.3) as the sole tuning
knob, and DMB.f section 10 monitoring remains the canonical
contention-regression detector.

If a future DMC-CON.5+ migration to papaya lands, the operator-visible
contract preserved is:

- `OC_RSYNC_DASHMAP_SHARDS` becomes a no-op (papaya has no shard
  count) and should be deprecated rather than removed silently.
- The `Ref`-guard footgun documented in BR-3j.a section "Gotchas for
  BR-3j.c / BR-3j.d" point 2 disappears; papaya's `Guard` lifetimes
  are scoped to the operation, not the bucket.

## 6. Cross-references

- `docs/design/dashmap-scalability-decision.md` (DMB.f) - the outer
  decision framework. This audit fills in section 11 row "Should we
  evaluate lock-free alternatives (flurry, papaya)?": evaluated; no
  migration justified at current scale; revisit criteria recorded.
- `docs/design/dmc-con-adaptive-sharding.md` (DMC-CON.3, PR #5604) -
  the active shard-sizing implementation this audit defers to.
- `docs/audits/br-3j-a-dashmap-vs-sharded-2026-05-20.md` (BR-3j.a) -
  the original DashMap-vs-sharded-mutex decision, plus the
  `Ref`-guard / TOCTOU / poisoning gotchas the alternative-data-
  structures comparison reuses.
- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` -
  production site for the `files: DashMap<FileNdx, SlotEntry>` field
  this audit covers.
- `crates/engine/benches/delete_plan_map_contention.rs` - the three-
  way bench shape (`mutex_hashmap`, `dashmap`, `sharded_mutex_hashmap`)
  that a future DMC-CON.5+ bench would extend with a `papaya_hashmap`
  arm.
- DMB.f section 10 - monitoring plan for contention regression
  detection that stays canonical regardless of DMC-CON.4's outcome.
