# DMB.f - DashMap scalability decision framework

Date: 2026-06-01
Status: Decision framework (pending bench data from DMB.c/d/e)
Tracker: DMB.f. Predecessors: DMB.a (harness), DMB.b (thread sweep),
DMB.c (DashMap vs Mutex at 100K), DMB.d (comparison at 1M), DMB.e
(shard tuning evaluation).

## 1. Purpose

This document synthesises findings from DMB.a through DMB.e into a
go/no-go decision for DashMap in two production sites:

1. **DeletePlanMap** (`crates/engine/src/delete/plan_map.rs`) - currently
   `Mutex<HashMap<PathBuf, DeletePlan>>`. N producer threads insert plans,
   1 consumer thread drains via `take()`.
2. **ParallelDeltaApplier** (`crates/engine/src/concurrent_delta/
   parallel_apply/mod.rs`) - migrated to `DashMap<FileNdx, SlotEntry>` in
   BR-3j (PRs #4634-#4636). N threads perform register/lookup/finish
   symmetrically.

The decision applies to both sites independently because their access
patterns differ materially.

## 2. Summary of DMB.a-e findings

### 2.1 DMB.a - Harness design

Established the unified bench harness (`dmb_a_dashmap_delete_bench.rs`)
with:

- `MapStore` trait abstracting DashMap vs `Mutex<HashMap>` vs sharded-mutex.
- Scale tiers: 10K / 100K / 1M entries.
- Thread sweep: 1 / 2 / 4 / 8 / 16 / 32 / 64 workers.
- Metrics: throughput (ops/sec), latency percentiles (p50/p99), memory
  overhead (VmHWM), contention events (perf lock).
- CI integration via non-required advisory workflow.

### 2.2 DMB.b - DashMap thread-count sweep (100K)

Captured DashMap-only cores-vs-throughput curve at 100K entries. Key
deliverables:

- Scaling inflection point per target (thread count where efficiency < 0.5).
- p99/p50 tail-latency ratio at each thread count.
- Confirmation of no negative scaling below 16 threads.

Placeholder: insert DMB.b inflection points once measured.

| Target | Inflection thread count | Efficiency at inflection |
|--------|------------------------|-------------------------|
| A (DeletePlanMap) | TBD | TBD |
| B (Applier files) | TBD | TBD |

### 2.3 DMB.c - DashMap vs Mutex at 100K

Side-by-side comparison at the production-relevant 100K tier. Key
deliverables:

- Crossover point: thread count where DashMap first exceeds Mutex throughput.
- Speedup ratio per thread count.
- Tail-latency comparison (p99/p50 for both stores).

Placeholder: insert DMB.c speedup ratios once measured.

| Threads | Target A speedup | Target B speedup |
|---------|-----------------|-----------------|
| 1 | TBD | TBD |
| 4 | TBD | TBD |
| 8 | TBD | TBD |
| 16 | TBD | TBD |
| 32 | TBD | TBD |

### 2.4 DMB.d - Comparison at 1M

Extends DMB.c to 1M entries where memory-allocator pressure, shard
imbalance, and cache-capacity effects become visible. Key deliverables:

- Whether DashMap advantage widens or narrows at 1M vs 100K.
- Memory overhead: RSS difference between DashMap and Mutex at 1M.
- Whether resize events (HashMap rehash) shift the crossover point.

### 2.5 DMB.e - Shard tuning evaluation

Tests non-default shard counts (128, 256) against the DashMap default to
determine whether explicit shard sizing provides measurable benefit at
32+ threads on the 1M tier.

## 3. Decision framework

The framework evaluates three possible outcomes per site:

| Decision | Description |
|----------|-------------|
| **Keep DashMap** | Current DashMap usage is justified. No changes needed. |
| **Replace with Mutex** | DashMap overhead not justified. Revert to `Mutex<HashMap>`. |
| **Hybrid** | DashMap for one site, Mutex for the other, based on access pattern. |

The decision is per-site because the two targets have fundamentally
different access patterns (section 4).

## 4. Per-site assessment

### 4.1 DeletePlanMap - asymmetric producer/consumer

**Access pattern:**

- Phase 1: N rayon workers insert `DeletePlan` entries (write-heavy, disjoint keys).
- Phase 2: 1 emitter thread drains via `take()` (read-then-remove, sequential).
- Phases overlap: inserts continue during drain.

**DashMap characteristics:**

- Producers benefit from shard distribution - N concurrent inserts hit
  independent shards most of the time with sequential directory keys.
- Consumer benefits less - single thread accessing one shard at a time,
  no contention from the consumer's perspective.
- The real contention arises from producer-consumer overlap: a producer
  inserting into a shard the consumer is reading from.

**Mutex characteristics:**

- All inserts and takes serialise behind one lock.
- Lock hold time is short (HashMap insert/remove is O(1) amortised).
- At low producer counts (1-4), lock acquisition overhead is negligible
  relative to the I/O cost of actually performing deletions.

**Decision driver:** The number of concurrent producer threads that
insert while the consumer drains. If this is typically <= 4 (matching
most CI runner core counts and the default rayon pool on modest hardware),
Mutex is sufficient. If production workloads routinely run 8-16 producers,
DashMap's shard distribution prevents the single lock from becoming a
serialisation bottleneck.

### 4.2 ParallelDeltaApplier - symmetric register/lookup/finish

**Access pattern:**

- N worker threads concurrently register files (insert).
- N worker threads concurrently look up file slots (read).
- N worker threads concurrently finish files (remove + Arc unwrap).
- All three operations are interleaved within the same rayon pool.

**DashMap characteristics:**

- Symmetric access means every thread may contend with every other thread.
- With sequential `FileNdx` keys hashing uniformly across shards, the
  probability of two threads hitting the same shard is approximately
  `N / shard_count`. At 128 shards and 16 threads: ~12.5% collision rate.
- Read operations (lookup) use shared `RwLock` reads - multiple threads
  can read the same shard concurrently.
- Write operations (register, finish) require exclusive shard access.

**Mutex characteristics:**

- Every operation serialises. With N threads performing 3 operations
  each per file, the single lock becomes a hard serialisation point.
- At 8+ threads the mutex contention directly caps throughput: only one
  thread progresses at a time.

**Decision driver:** The worker count in the parallel delta applier. The
PIP-3+5 heuristic dispatches into parallel mode when
`file_count > 100 || total_size > 64 MiB`. In parallel mode, the worker
count matches `available_parallelism()` which is 8-16 on typical
production hardware. This is the regime where DashMap's shard distribution
provides the most benefit.

## 5. Crossover analysis

The crossover point is the thread count at which DashMap throughput first
exceeds Mutex throughput for a given target and scale tier.

### 5.1 Theoretical model

DashMap per-op cost consists of:

1. Key hash computation (~5 ns for u32/PathBuf).
2. Shard selection (hash modulo shard_count, ~1 ns).
3. Shard RwLock acquisition (uncontended: ~15 ns; contended: variable).
4. HashMap operation within shard (~20 ns for insert/remove).

Total uncontended DashMap per-op: ~40 ns.

Mutex per-op cost:

1. Mutex acquisition (uncontended: ~10 ns; contended: variable).
2. HashMap operation (~20 ns).

Total uncontended Mutex per-op: ~30 ns.

**Overhead delta (uncontended):** DashMap is ~10 ns slower per-op due to
hash-based shard selection and the RwLock layer. This 33% per-op penalty
must be amortised by reduced contention at higher thread counts.

**Crossover formula (simplified):**

```
DashMap wins when:
  mutex_contention_wait(N) > dashmap_per_op_overhead * ops_per_thread

Where:
  mutex_contention_wait(N) ~ (N - 1) * lock_hold_time * ops_per_thread / N
  dashmap_per_op_overhead ~ 10 ns
```

Solving for N (thread count):

```
N > 1 + (dashmap_overhead / lock_hold_time)
N > 1 + (10 ns / 30 ns)
N > 1.33
```

This naive model predicts DashMap wins at N >= 2. In practice, the
crossover is higher because:

- OS scheduling overhead adds latency that the mutex amortises.
- DashMap's `RwLock` has higher uncontended cost than `Mutex`.
- False sharing between adjacent shards adds cache-line bouncing.

**Empirical prediction:** Crossover at 4-8 threads for both targets,
with Target A potentially crossing later due to the asymmetric access
pattern (single consumer rarely contends with producers on the same shard).

### 5.2 Measured crossover (pending DMB.c data)

| Target | Predicted crossover | Measured crossover |
|--------|--------------------|--------------------|
| A (DeletePlanMap) | 4-8 threads | TBD |
| B (Applier files) | 4-8 threads | TBD |

## 6. Memory overhead analysis

DashMap carries per-shard fixed costs that `Mutex<HashMap>` does not:

### 6.1 Per-shard overhead

Each DashMap shard contains:

- 1 `RwLock` (8 bytes on most platforms).
- 1 `HashMap` with its own control bytes and bucket array.
- Alignment padding to avoid false sharing (DashMap 6.1 does not pad;
  shards may share cache lines).

**Empty DashMap with 128 shards:**

- 128 RwLock instances: 128 x 8 = 1,024 bytes.
- 128 empty HashMap allocations: 128 x ~64 = 8,192 bytes (minimum
  bucket array per HashMap).
- Total fixed overhead: ~9 KB.

**Empty Mutex<HashMap>:**

- 1 Mutex: 8 bytes.
- 1 empty HashMap: ~64 bytes.
- Total fixed overhead: ~72 bytes.

### 6.2 Populated overhead at scale

| Scale | Mutex<HashMap> RSS (est.) | DashMap RSS (est.) | Overhead |
|-------|--------------------------|-------------------|----------|
| 10K entries | ~2 MB | ~2.1 MB | ~5% |
| 100K entries | ~20 MB | ~20.5 MB | ~2.5% |
| 1M entries | ~200 MB | ~201 MB | ~0.5% |

At production scale (100K-1M entries), the per-shard fixed cost is
negligible relative to entry storage. Memory overhead is NOT a decision
factor at these scales.

### 6.3 When memory matters

Memory overhead becomes significant only when:

- Shard count is grossly oversized relative to entry count (e.g., 512
  shards for 100 entries - 5 entries per shard means 512 independent
  HashMap allocations for minimal data).
- Many DashMap instances exist simultaneously (not the case here - one
  DeletePlanMap and one applier files map per transfer).

### 6.4 Measured memory (pending DMB.d data)

| Store | 100K RSS | 1M RSS |
|-------|----------|--------|
| Mutex<HashMap> (Target A) | TBD | TBD |
| DashMap (Target A) | TBD | TBD |
| Mutex<HashMap> (Target B) | TBD | TBD |
| DashMap (Target B) | TBD | TBD |

## 7. Recommendation template

### 7.1 Decision thresholds

The decision rests on three quantitative criteria from DMB.c:

| Criterion | Threshold | Rationale |
|-----------|-----------|-----------|
| Low-thread regression | Speedup >= 0.85 at 1 and 4 threads | DashMap must not penalise the common case (CI runners, modest hardware) by more than 15%. |
| High-thread advantage | Speedup >= 1.3x at 16 threads | DashMap must deliver meaningful throughput gain at production concurrency to justify its complexity (128 shard RwLocks, hash-based dispatch). |
| Tail-latency health | DashMap p99/p50 < 5x at all thread counts | DashMap must not introduce pathological tail latency from shard contention or false sharing. |

### 7.2 Decision matrix

| Low-thread | High-thread | Tail latency | Decision |
|------------|-------------|--------------|----------|
| Pass | Pass | Pass | **Keep DashMap.** Validated for the site. |
| Pass | Fail | Pass | **Grey zone.** Keep existing DashMap (no revert churn), do not expand to new sites. |
| Fail | Pass | Pass | **Conditional keep.** Guard with thread-count threshold: use Mutex below N threads, DashMap above. |
| Fail | Fail | Any | **Replace.** DashMap not justified. Revert to Mutex<HashMap>. |
| Any | Any | Fail | **Replace.** Tail-latency pathology overrides throughput gains. |

### 7.3 Per-site recommendation (pending data)

| Site | Recommendation | Rationale |
|------|---------------|-----------|
| DeletePlanMap | TBD | Depends on DMB.c Target A results and typical production thread count. |
| ParallelDeltaApplier | TBD | Depends on DMB.c Target B results. Already migrated - revert has a cost. |

## 8. Implementation plan: if decision is "replace"

### 8.1 ParallelDeltaApplier revert

If DMB.c shows DashMap is not justified for Target B:

1. Replace `files: DashMap<FileNdx, SlotEntry>` with
   `files: Mutex<HashMap<FileNdx, SlotEntry>>`.
2. Update `register_file()`, `slot_for()`, `finish_file()` to acquire the
   mutex.
3. Remove the `dashmap` dependency from `crates/engine/Cargo.toml` (if no
   other consumer remains).
4. Re-run the BR-3j.f cores-vs-throughput bench to confirm no regression
   at the PIP-3+5 dispatch threshold.

Estimated effort: 1 PR, mechanical. The `MapStore` trait from the bench
harness already abstracts both shapes.

### 8.2 DeletePlanMap: no action (current shape is Mutex)

If DMB.c shows DashMap is not justified for Target A, no code change is
needed - `DeletePlanMap` already uses `Mutex<HashMap>`. Close the DashMap
migration proposal (DDP-A3, #2253) as wontfix.

### 8.3 If decision is "tune" (shard count adjustment)

If DMB.e shows that non-default shard counts provide > 15% throughput
improvement at 32+ threads:

1. Expose `shard_amount` as a constructor parameter:
   ```rust
   impl ParallelDeltaApplier {
       pub fn with_shard_count(workers: usize, shards: usize) -> Self { ... }
   }
   ```
2. Default to the DashMap library default (`available_parallelism() * 4`).
3. Document the tuning knob in rustdoc with guidance: "Override only on
   hosts with 32+ cores where the default shard count causes measurable
   contention."
4. Add a bench regression gate: if future DMB nightly runs show the tuned
   count regressing, revert to default.

**Status (2026-06-10):** Implemented via DMC-CON.2/.3/.5
(`docs/design/dmc-con-adaptive-sharding.md`). The applier's
`with_strategy` constructor now picks `shard_count = (concurrency * 4)
.next_power_of_two().clamp(4, 1024)`, with the heuristic input changed
from "host CPUs" to "applier worker count" so the shard table tracks the
actual fan-out rather than physical hardware. Operator override:
`OC_RSYNC_DASHMAP_SHARDS=<n>` (clamped, rounded to power of two on input,
falls back to the heuristic on parse failure or unset). The constructor
parameter sketched in step 1 above was rejected in favour of the env
override: a parameter would fork every call site between callers that
know `worker_count` and callers that do not, while the env override is
opt-in and process-wide.

## 9. Implementation plan: if decision is "keep"

### 9.1 ParallelDeltaApplier (already on DashMap)

No code changes. Close the open question in
`project_parallel_delta_apply_outer_mutex.md` with a reference to this
decision document and the supporting DMB.c numbers.

### 9.2 DeletePlanMap migration (conditional)

If DMB.c Target A shows speedup >= 1.3x at 16 threads AND the delete
pipeline's production concurrency is routinely >= 8 threads:

1. Replace `Mutex<HashMap<PathBuf, DeletePlan>>` with
   `DashMap<PathBuf, DeletePlan>` in `plan_map.rs`.
2. Update `register()` and `take()` methods.
3. Run the DMB.a bench to confirm no regression.
4. Close DDP-A3 (#2253) as completed.

If DMB.c Target A shows speedup in the grey zone (1.0-1.3x at 16
threads): do NOT migrate. The marginal benefit does not justify a second
DashMap consumer in the delete pipeline.

## 10. Monitoring plan

### 10.1 Detecting contention regression in production

DashMap contention is invisible at the application level (no metrics
emitted by the library). Regression detection relies on:

**Approach 1: Nightly bench CI (primary)**

The `.github/workflows/bench-dashmap-delete.yml` workflow runs nightly at
the 100K tier. Criterion's `--baseline` comparison detects throughput
regressions > 5% between runs. A sustained regression across 3+
consecutive nightly runs signals contention growth (e.g., from a code
change that increased lock hold time within a shard).

**Approach 2: Transfer wall-clock regression (secondary)**

If a release shows unexplained wall-clock regression on the parallel
delta path (measured via the interop harness or `delta_transfer_benchmark`),
check whether the regression correlates with:

- Increased file count (more entries in the DashMap).
- Increased worker count (more threads contending for shards).
- A DashMap version bump (shard internals changed).

**Approach 3: perf lock profiling (diagnostic)**

When a regression is suspected, profile on bare-metal:

```sh
perf lock record -- cargo bench -p engine \
    --bench dmb_a_dashmap_delete_bench -- '100k.*16_threads'
perf lock report --sort acquired
```

Compare contention-per-op ratio against the baseline captured during
DMB.c. A > 2x increase in contention events per op confirms shard
saturation as the regression cause.

### 10.2 Version-bump protocol

When upgrading the `dashmap` crate:

1. Run the full DMB.a bench suite (all tiers, all thread counts).
2. Compare against the saved baseline (`--baseline dmb-production`).
3. Flag any cell with > 10% throughput regression or > 2x p99 increase.
4. If flagged: investigate the upstream changelog for shard-layout or
   RwLock changes before accepting the upgrade.

### 10.3 Alert thresholds

| Metric | Warning | Critical |
|--------|---------|----------|
| Nightly bench throughput delta | -5% sustained 3 runs | -15% any single run |
| p99/p50 ratio (any cell) | > 5x | > 10x |
| Contention events per op (perf lock) | > 2x baseline | > 5x baseline |

## 11. Open questions resolved by this framework

| Question | Resolution |
|----------|-----------|
| Should DeletePlanMap migrate to DashMap? | Conditional on DMB.c Target A speedup >= 1.3x at 16 threads (section 9.2). |
| Was the BR-3j DashMap migration for ParallelDeltaApplier justified? | Validated if DMB.c Target B meets the keep thresholds (section 7.2). |
| Should we evaluate lock-free alternatives (flurry, papaya)? | Only if DashMap shows negative scaling at tested thread counts (DMB.a section 11.3). |
| What shard count should we use? | Default unless DMB.e shows > 15% gain from tuning (section 8.3). |
| How do we detect DashMap regression after the decision? | Nightly bench CI + perf lock diagnostics (section 10). |

## 12. Timeline and dependencies

```
DMB.a (harness)     [DONE - spec shipped]
DMB.b (sweep)       [DONE - spec shipped, numbers deferred]
DMB.c (100K cmp)    [DONE - spec shipped, numbers deferred]
DMB.d (1M cmp)      [pending offline capture]
DMB.e (shard tune)  [pending DMB.d results]
DMB.f (this doc)    [framework complete; decision pending bench data]
```

Once DMB.c and DMB.d numbers are captured on bare-metal hardware,
populate the TBD cells in sections 2.2, 2.3, 5.2, 6.4, and 7.3. The
decision becomes final when all three quantitative criteria in section
7.1 are evaluated with measured data.

## 13. Cross-references

- `docs/design/dmb-a-dashmap-delete-bench-harness.md` - Harness design,
  MapStore trait, CI workflow, scale tiers.
- `docs/design/dmb-b-dashmap-thread-sweep.md` - DashMap-only 100K sweep,
  inflection point identification.
- `docs/design/dashmap-vs-mutex-100k-delete-bench.md` - DMB.c comparison
  methodology, decision criteria with specific thresholds.
- `docs/design/br-3j-f-dashmap-rebench-2026-05-21.md` - BR-3j.f re-bench
  methodology, number capture procedure.
- `crates/engine/benches/dmb_a_dashmap_delete_bench.rs` - Unified bench
  harness.
- `crates/engine/benches/delete_plan_map_contention.rs` - DDP-B4
  micro-bench (predecessor).
- `crates/engine/src/delete/plan_map.rs` - DeletePlanMap production code.
- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` -
  ParallelDeltaApplier production code.
- `docs/audits/br-3j-a-dashmap-vs-sharded-2026-05-20.md` - DashMap
  selection audit.
