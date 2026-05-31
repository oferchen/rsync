# DMB.c - DashMap vs Mutex<HashMap> throughput comparison at 100K delete scale

Date: 2026-06-01
Status: Design spec
Tracker: DMB.c. Predecessor: DMB.b (DashMap thread-count sweep at 100K).
Follow-up: DMB.d (comparison at 1M scale).

## 1. Motivation

DMB.b captured the DashMap-only cores-vs-throughput curve at the 100K entry
tier across seven thread counts (1/2/4/8/16/32/64). The missing piece is
the direct comparison: does DashMap actually outperform the simpler
`Mutex<HashMap>` at realistic delete workload sizes, and if so, at what
thread count does the crossover occur?

BR-3j replaced `Mutex<HashMap>` with `DashMap` in `ParallelDeltaApplier`
(PRs #4634-#4636) based on the audit prediction that per-shard locking
would reduce contention under parallel delete workers. The DDP-B4
micro-bench (`delete_plan_map_contention.rs`) compared three stores but
only at fixed thread counts (4/8/16) and without latency percentiles or
contention diagnostics. DMB.c produces the definitive side-by-side curve
at the 100K production scale.

Key question: does DashMap justify its complexity (128+ shard RwLocks,
higher per-op overhead in the uncontended case) versus a single
`Mutex<HashMap>` for the delete workload's access pattern?

## 2. Bench architecture

### 2.1 Same workload, two backing stores

The bench uses the unified harness from DMB.a
(`crates/engine/benches/dmb_a_dashmap_delete_bench.rs`) with both stores
exercised through the `MapStore` trait. The timed code path is identical
for both stores - only the underlying data structure differs:

| Store | Implementation | Locking model |
|-------|---------------|---------------|
| `dashmap` | `DashMap<K, V>` | Per-shard `RwLock`. Default shard count = `available_parallelism() * 4` rounded to next power-of-two. On a 16-core host: 128 shards. |
| `mutex_hashmap` | `Mutex<HashMap<K, V>>` | Single `Mutex` over the entire map. Zero sharding overhead. Minimum per-op cost when uncontended. |

### 2.2 Workload fixture (100K entries)

Both Target A (DeletePlanMap) and Target B (ParallelDeltaApplier files map)
use the same 100K-entry fixture defined in DMB.b section 4:

- **Target A:** 100K directory keys, each mapping to a `DeletePlan` with one
  `DeleteEntry`. Operations: insert (phase 1 producers) + take (phase 2
  consumer). Total ops per iteration: 200K.
- **Target B:** 100K `FileNdx` entries, each mapping to a `SlotEntry`
  wrapping a `CountingSink`. Operations: register + lookup + finish. Total
  ops per iteration: 300K.

Keys, payloads, and RNG seed (`0xDEAD_BEEF_CAFE_D00D`) are identical to
DMB.b so the DashMap numbers are directly comparable between runs.

### 2.3 Isolation guarantees

- Pre-built payloads cloned into each sample via `iter_batched` with
  `BatchSize::LargeInput` - timed section captures only map operations.
- Rayon pool pinned to the target thread count via
  `ThreadPoolBuilder::new().num_threads(N)`.
- No I/O in the timed section (`CountingSink` for Target B writes).
- Separate criterion groups for throughput and latency so instrumentation
  overhead does not contaminate throughput numbers.

## 3. Scale

100K entries is the production-relevant scale for the delete workload:

- Matches the DDP-B4 bench entry count.
- Represents the receiver file-count threshold where parallelism starts to
  pay off (below ~10K, single-threaded drain is faster due to thread pool
  spin-up cost).
- Large enough to expose lock contention effects but small enough to run
  within CI time budgets (< 5 minutes per full sweep).

The 100K tier is where the DashMap vs Mutex decision matters most. At 10K
entries the single mutex is uncontended regardless of thread count. At 1M
entries (DMB.d) memory pressure and allocator behaviour become confounding
variables.

## 4. Thread counts

| Threads | DashMap shards (16-core host) | Threads per shard | Expected contention |
|---------|------------------------------|-------------------|---------------------|
| 1 | 128 | 0.008 | None. Both stores uncontended. Measures raw per-op overhead. |
| 4 | 128 | 0.031 | Negligible. DashMap overhead may cause it to lose to Mutex. |
| 8 | 128 | 0.063 | Low. Crossover region - DashMap begins to amortise shard overhead. |
| 16 | 128 | 0.125 | Moderate. DashMap should show clear advantage as Mutex serialises. |
| 32 | 128 | 0.25 | High. Mutex severely contended. DashMap shards absorb concurrency. |

The 2 and 64 thread counts from DMB.b are omitted from the comparison
focus (though still captured by the harness) because 2 threads is too low
for meaningful contention and 64 threads is heavy over-subscription on most
hardware. The decision-relevant range is 1/4/8/16/32.

## 5. Metrics

### 5.1 Operations per second

Primary metric. Criterion reports via `Throughput::Elements(total_ops)`:

- **Target A:** `total_ops = 200K` (100K inserts + 100K takes).
- **Target B:** `total_ops = 300K` (100K registers + 100K lookups + 100K
  finishes).

Reported as median ops/sec per (store, thread count) cell.

### 5.2 P50 and P99 operation latency

Per-operation latency captured via `Instant::now()` / `elapsed()` per map
call. Samples collected into pre-allocated per-thread `Vec<Duration>`,
merged and sorted offline.

Metrics:
- **p50** - median per-op latency. Reflects typical lock-acquisition cost.
- **p99** - tail latency. Reflects worst-case contention (lock-convoy,
  shard collision, OS preemption during critical section).
- **p99/p50 ratio** - contention amplification factor. A ratio > 5x
  signals problematic tail latency.

### 5.3 Lock contention events (supplementary, offline only)

Captured via `perf lock record` on Linux bare-metal runs:

- **Futex contention count** per lock site. DashMap's per-shard `RwLock`
  maps to futex ops when contended; `Mutex<HashMap>` has a single futex.
- **Total contention wait time** aggregated across all lock sites.
- **Contention-per-op ratio** = total contention events / total ops.
  Directly comparable between stores.

Expected:
- `Mutex<HashMap>` contention count scales linearly with thread count past
  the crossover (every op must acquire the single lock).
- `DashMap` contention count stays flat until threads > shards / 2, then
  rises sub-linearly (collisions on same-shard keys).

### 5.4 Speedup ratio

The primary comparison metric, computed per thread count:

```
speedup(N) = dashmap_ops_per_sec(N) / mutex_ops_per_sec(N)
```

| Speedup | Interpretation |
|---------|----------------|
| < 1.0 | DashMap slower. Mutex wins at this thread count. |
| 1.0-1.3 | DashMap marginal. Not enough to justify added complexity. |
| >= 1.3 | DashMap clearly wins. Justifies the per-shard locking overhead. |

## 6. Expected outcome

### 6.1 Thread-count crossover prediction

Based on the locking models:

- **1-4 threads:** Mutex wins or ties. DashMap's per-op overhead (shard
  selection via hash, `RwLock` acquisition on the selected shard) exceeds
  the benefit of reduced contention when contention is near-zero. Expected
  speedup: 0.85-1.0x.

- **8 threads:** Crossover region. The single Mutex becomes a serialisation
  bottleneck for 8 concurrent workers performing insert/take/lookup cycles.
  DashMap's 128 shards distribute the load. Expected speedup: 1.0-1.3x.

- **16+ threads:** DashMap wins clearly. 16 threads competing for a single
  Mutex lock means ~15 threads blocked at any moment. DashMap distributes
  across 128 shards so only 0.125 threads per shard on average. Expected
  speedup: 1.5-3.0x at 16 threads, 2.0-5.0x at 32 threads.

### 6.2 Predicted curves

```
ops/sec (normalised to Mutex@1-thread)
  ^
  |                              DashMap
  |                         .---*
  |                       ./
  |                     ./
  |                  ../        Mutex<HashMap>
  |               ../      .---*---*
  |            ../       ./
  |          ./        ./
  |        ./        ./
  |      ./        ./
  |    ./        ./
  |  ./        ./
  | /        ./
  |/       ./
  *------*
  +--+---+---+----+----+-------> threads
     1   4   8   16   32
```

### 6.3 Target A vs Target B differences

Target A (DeletePlanMap) has a mixed producer-consumer pattern: N producers
insert while 1 consumer takes. The consumer holds the lock for the full
take duration, creating head-of-line blocking in the Mutex case. DashMap
benefits more here because producers and consumer likely hit different
shards.

Target B (ParallelDeltaApplier) has symmetric access: all threads perform
register/lookup/finish. DashMap benefit is purely from shard distribution
with no producer-consumer asymmetry to exploit.

Expected: DashMap advantage is more pronounced for Target A than Target B
at the same thread count.

## 7. Comparison methodology

### 7.1 Side-by-side criterion run

Both stores run in the same criterion group, producing directly comparable
numbers without environmental drift:

```sh
# Full comparison at 100K (both stores, all thread counts):
cargo bench -p engine --bench dmb_a_dashmap_delete_bench -- '100k'

# Side-by-side output shows relative performance:
# dmb_c_delete_plan_map/throughput/100k/dashmap/16_threads
# dmb_c_delete_plan_map/throughput/100k/mutex_hashmap/16_threads
```

### 7.2 Criterion baseline comparison

For comparing against DMB.b's previously captured DashMap numbers:

```sh
# Load DMB.b baseline:
cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --baseline dmb-b '100k.*mutex_hashmap'
```

### 7.3 Statistical rigour

- **Sample size:** 20 per cell (matching DMB.b).
- **Measurement time:** 8 seconds per cell.
- **Warm-up:** 3 seconds.
- **CoV threshold:** < 5%. Cells exceeding 5% are re-run with
  `--sample-size 50 --measurement-time 15`.
- **Outlier detection:** Criterion's built-in outlier classification
  (mild/severe) reported per cell. Cells with > 10% severe outliers are
  flagged for investigation.

### 7.4 Contention profiling (offline, bare-metal only)

Run on a 16+ physical core Linux host to produce production-grade numbers:

```sh
# Mutex contention profile at 32 threads:
perf lock record -- \
    cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --filter 'mutex_hashmap.*100k.*32_threads'
perf lock report --sort acquired

# DashMap contention profile at 32 threads:
perf lock record -- \
    cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --filter 'dashmap.*100k.*32_threads'
perf lock report --sort acquired

# Cache-line analysis (false sharing detection):
perf stat -e cache-misses,cache-references,L1-dcache-load-misses \
    cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --filter '100k.*32_threads'
```

## 8. Decision criteria

### 8.1 DashMap justified (keep current production shape)

DashMap is justified for the delete workload if ALL of the following hold:

1. **No regression at low threads:** Speedup >= 0.85 at 1 and 4 threads.
   DashMap's per-op overhead must not exceed 15% penalty in the
   uncontended case.
2. **Clear win at scale:** Speedup >= 1.3x at 16 threads AND >= 1.5x at
   32 threads. The sharding benefit must be substantial enough to justify
   the added complexity (128 RwLocks, hash-based shard selection, larger
   memory footprint).
3. **Healthy tail latency:** p99/p50 ratio < 5x at all thread counts for
   DashMap. If DashMap has worse tail latency than Mutex at any thread
   count, the speedup threshold rises to 2.0x to compensate.

If all three criteria are met, DashMap remains the production default for
both `ParallelDeltaApplier` (already migrated) and as a candidate for
`DeletePlanMap` (currently `Mutex<HashMap>`).

### 8.2 DashMap not justified (revert to Mutex<HashMap>)

Revert if ANY of the following hold:

1. **Regression at low threads:** Speedup < 0.85 at 1 or 4 threads AND
   the workload's typical concurrency is <= 4 threads in production.
2. **Insufficient win at scale:** Speedup < 1.3x at 16 threads. The
   sharding benefit does not compensate for the added complexity.
3. **Pathological tail latency:** p99/p50 ratio > 10x at any thread count
   for DashMap while Mutex p99/p50 stays below 5x.

Action: file a follow-up to revert `ParallelDeltaApplier` to
`Mutex<HashMap>` and close the DashMap migration for `DeletePlanMap` as
wontfix.

### 8.3 Grey zone (1.0-1.3x at 16 threads)

If DashMap speedup is between 1.0x and 1.3x at 16 threads:

- **Keep DashMap for ParallelDeltaApplier** (already migrated, working,
  no reason to churn).
- **Do NOT migrate DeletePlanMap** to DashMap. The marginal benefit does
  not justify a second DashMap dependency in the delete pipeline.
- **Proceed to DMB.d** (1M scale) to check whether the advantage widens
  at larger entry counts where hash-table resize events become the
  bottleneck.

### 8.4 Summary decision table

| Speedup @ 16 threads | Speedup @ 32 threads | Low-thread regression? | Decision |
|----------------------|---------------------|----------------------|----------|
| >= 1.3x | >= 1.5x | No (>= 0.85x) | DashMap justified. Keep and consider migrating DeletePlanMap. |
| >= 1.3x | >= 1.5x | Yes (< 0.85x) | DashMap justified for high-concurrency paths only. Guard with thread-count threshold. |
| 1.0-1.3x | Any | No | Grey zone. Keep existing DashMap, do not expand. Proceed to DMB.d. |
| < 1.0x | Any | Any | DashMap not justified. Evaluate revert. |

## 9. Results template

Numbers captured offline and appended here once measured.

### 9.1 Target A - DeletePlanMap (100K entries)

| Threads | DashMap ops/sec | Mutex ops/sec | Speedup | DashMap p50 (ns) | Mutex p50 (ns) | DashMap p99 (ns) | Mutex p99 (ns) |
|---------|----------------|--------------|---------|-----------------|----------------|-----------------|----------------|
| 1 | | | | | | | |
| 4 | | | | | | | |
| 8 | | | | | | | |
| 16 | | | | | | | |
| 32 | | | | | | | |

### 9.2 Target B - ParallelDeltaApplier files map (100K entries)

| Threads | DashMap ops/sec | Mutex ops/sec | Speedup | DashMap p50 (ns) | Mutex p50 (ns) | DashMap p99 (ns) | Mutex p99 (ns) |
|---------|----------------|--------------|---------|-----------------|----------------|-----------------|----------------|
| 1 | | | | | | | |
| 4 | | | | | | | |
| 8 | | | | | | | |
| 16 | | | | | | | |
| 32 | | | | | | | |

### 9.3 Contention events (32 threads, offline perf lock)

| Store | Total contention events | Contention wait (ms) | Contention per op |
|-------|------------------------|---------------------|-------------------|
| DashMap (Target A) | | | |
| Mutex (Target A) | | | |
| DashMap (Target B) | | | |
| Mutex (Target B) | | | |

### 9.4 Decision outcome

Pending bench results.

## 10. Implementation notes

### 10.1 Mutex<HashMap> reconstruction for Target B

The `ParallelDeltaApplier` was migrated to DashMap in BR-3j (PRs
#4634-#4636). The bench reconstructs the pre-migration shape purely for
comparison:

```rust
struct MutexMapStore<K, V> {
    inner: Mutex<HashMap<K, V>>,
}

impl<K: Eq + Hash, V> MapStore<K, V> for MutexMapStore<K, V> {
    fn insert(&self, key: K, value: V) {
        self.inner.lock().unwrap().insert(key, value);
    }

    fn get(&self, key: &K) -> Option<V> where V: Clone {
        self.inner.lock().unwrap().get(key).cloned()
    }

    fn remove(&self, key: &K) -> Option<V> {
        self.inner.lock().unwrap().remove(key)
    }
}
```

This is bench-only code. Production `ParallelDeltaApplier` remains on
DashMap regardless of bench outcome (section 8.3 - grey zone does not
trigger a revert for already-migrated code).

### 10.2 Fair comparison: pre-allocated capacity

Both stores are constructed with `with_capacity(100_000)` to eliminate
resize events from the measurement. DashMap's `with_capacity_and_shard_
amount(100_000, default_shards)` ensures capacity is distributed across
shards. HashMap's `with_capacity(100_000)` pre-allocates a single backing
array.

### 10.3 Key distribution

Keys are sequentially generated (`0..100_000`) and distributed to threads
in round-robin fashion. This models the production pattern where file
indices arrive in semi-sequential order from the wire. It also ensures
even shard distribution for DashMap (sequential u32 keys hash uniformly
across shards).

An adversarial distribution (all keys hashing to the same shard) is NOT
tested because it does not match production access patterns. If shard
imbalance is suspected from the contention profiling, a follow-up bench
with skewed key distributions can be added.

## 11. Invocation

### 11.1 Full DMB.c comparison

```sh
cargo bench -p engine --bench dmb_a_dashmap_delete_bench -- '100k'
```

### 11.2 Single-store drill-down

```sh
# DashMap only:
cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --filter 'dashmap.*100k'

# Mutex only:
cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --filter 'mutex_hashmap.*100k'
```

### 11.3 Single thread-count comparison

```sh
# Compare both stores at 16 threads (decision-critical cell):
cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --filter '100k.*16_threads'
```

### 11.4 Baseline workflow

```sh
# Step 1: capture DashMap baseline (from DMB.b)
cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --save-baseline dmb-c-dashmap '100k.*dashmap'

# Step 2: capture Mutex baseline
cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --save-baseline dmb-c-mutex '100k.*mutex_hashmap'

# Step 3: compare
critcmp dmb-c-dashmap dmb-c-mutex --filter '100k'
```

## 12. CI integration

DMB.c does not require a separate CI workflow. It uses the same
`.github/workflows/bench-dashmap-delete.yml` workflow defined in DMB.a
section 10. The workflow already filters to `'100k'` which captures both
DashMap and Mutex<HashMap> stores.

The comparison is implicit in criterion's output: both stores appear as
separate bench IDs within the same group, and criterion reports relative
change when a baseline exists.

## 13. Cross-references

- `docs/design/dmb-a-dashmap-delete-bench-harness.md` - DMB.a harness spec.
  Defines the `MapStore` trait, bench file layout, CI workflow, and decision
  criteria framework that DMB.c depends on.
- `docs/design/dmb-b-dashmap-thread-sweep.md` - DMB.b thread-count sweep.
  Provides the DashMap-only baseline numbers that DMB.c extends with the
  Mutex comparison.
- `crates/engine/benches/dmb_a_dashmap_delete_bench.rs` - The bench file.
- `crates/engine/benches/delete_plan_map_contention.rs` - DDP-B4
  micro-bench. Three-way comparison at 4/8/16 threads. DMB.c supersedes
  this with the full thread-count sweep and latency percentiles.
- `crates/engine/src/delete/plan_map.rs` - DeletePlanMap. Current
  `Mutex<HashMap>` production shape. DMB.c determines whether DashMap
  migration is warranted.
- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` -
  ParallelDeltaApplier. `files: DashMap<FileNdx, SlotEntry>`. DMB.c
  validates the BR-3j migration decision.
- `docs/audits/br-3j-a-dashmap-vs-sharded-2026-05-20.md` - DashMap
  selection audit. Predicted DashMap advantage at 8+ threads.
- `docs/design/br-3j-f-dashmap-rebench-2026-05-21.md` - BR-3j.f
  methodology. Offline number-capture procedure reused by DMB.c.
