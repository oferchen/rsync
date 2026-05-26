# DMB.a - DashMap cores-vs-throughput bench harness for the delete workload

Date: 2026-05-26
Status: Design spec
Tracker: DMB.a. Predecessor: BR-3j.f (#2508, DashMap re-bench for
ParallelDeltaApplier). Related: DDP-B4 (#2258, DeletePlanMap backing-store
bench at `crates/engine/benches/delete_plan_map_contention.rs`).

## 1. Motivation

BR-3j replaced `Mutex<HashMap>` with `DashMap` in `ParallelDeltaApplier`
(PRs #4634-#4636). The cores-vs-throughput re-bench at BR-3j.f validated the
switch for the delta-apply workload. Two DashMap consumers remain
uncharacterised at production scale:

1. **DeletePlanMap** (`crates/engine/src/delete/plan_map.rs`) - currently
   wraps `Mutex<HashMap<PathBuf, DeletePlan>>`. The DDP-B4 micro-bench
   (`delete_plan_map_contention.rs`) pits it against DashMap and a manual
   16-shard layout at 100K ops, but does not sweep core counts at the 1M
   scale or measure latency percentiles.
2. **ParallelDeltaApplier files map** (`crates/engine/src/concurrent_delta/
   parallel_apply/mod.rs`) - already migrated to `DashMap<FileNdx,
   SlotEntry>`. BR-3j.f exercises verify + dispatch throughput but does not
   isolate the map-operation cost from the checksum cost, nor does it
   measure memory overhead or shard contention at high core counts.

This spec designs a unified harness that benchmarks both targets across a
thread-count sweep, scale tiers, and backing-store alternatives so the
project has empirical data to decide whether to keep DashMap defaults, tune
shard counts, or evaluate alternatives.

## 2. Bench targets

### Target A - DeletePlanMap (insert + take)

Exercises the DDP phase-1/phase-2 access pattern:

- **Phase 1 (producer):** N rayon workers insert `DeletePlan` values into the
  map, each publishing a disjoint set of directory keys.
- **Phase 2 (consumer):** A single emitter thread drains plans via `take()`,
  one directory at a time, interleaved with ongoing phase-1 inserts.

The bench isolates map-operation cost by pre-building `DeletePlan` values
outside the timed section (following the pattern in
`delete_plan_map_contention.rs`). Three backing stores are compared via a
`PlanStore` trait, reusing the pattern from the existing bench:

| Store | Description |
|-------|-------------|
| `mutex_hashmap` | `Mutex<HashMap<PathBuf, DeletePlan>>` - current production shape. |
| `dashmap` | `DashMap<PathBuf, DeletePlan>` - drop-in candidate. |
| `sharded_mutex_hashmap` | `Vec<Mutex<HashMap>>` with configurable shard count - manual sharding baseline. |

### Target B - ParallelDeltaApplier files map (register + lookup + finish)

Exercises the DashMap-backed `files: DashMap<FileNdx, SlotEntry>` without the
checksum verify cost that dominates BR-3j.f. The bench drives three phases
per iteration:

1. **Register:** N threads call `register_file()` with disjoint `FileNdx`
   ranges, each inserting a `SlotEntry` into the DashMap.
2. **Lookup:** N threads call `slot_for()` on random registered NDX values,
   cloning the `SlotEntry` and immediately dropping the shard guard.
3. **Finish:** N threads call `finish_file()` to remove entries and unwrap
   the `Arc<SlotData>`.

A `CountingSink` writer (matching BR-3j.f) keeps the per-file write cost at
zero so the bench isolates map operations from I/O. Two backing stores are
compared:

| Store | Description |
|-------|-------------|
| `dashmap` | `DashMap<FileNdx, SlotEntry>` - current production shape. |
| `mutex_hashmap` | `Mutex<HashMap<FileNdx, SlotEntry>>` - pre-migration baseline, reconstructed for comparison. |

The `mutex_hashmap` baseline wraps the DashMap API surface behind the same
trait so the timed loop is identical between stores. This quantifies the
DashMap advantage the BR-3j.a audit predicted but BR-3j.f did not isolate
from verify cost.

## 3. Thread count sweep

Both targets sweep the following worker counts:

```
1 / 2 / 4 / 8 / 16 / 32 / 64
```

Each cell pins the ambient rayon pool to the target worker count via
`ThreadPoolBuilder::new().num_threads(N)` and dispatches work through
`pool.scope()` or `pool.install()`, matching the pattern in BR-3j.f and
`delete_plan_map_contention.rs`.

Counts above the host's `available_parallelism()` still produce meaningful
data: they expose whether the map implementation scales past physical core
count (hyper-threading headroom) or degrades under over-subscription
(lock-convoy effects, shard contention). The sweep deduplicates against
`available_parallelism()` so hosts with exactly 32 or 64 cores do not
produce duplicate cells.

## 4. Scale tiers

Each (target, store, thread count) cell runs at three scale tiers:

| Tier | Entry count | Purpose |
|------|-------------|---------|
| 10K | 10,000 | Warm-up. Small enough that single-mutex overhead is negligible. Establishes the uncontended baseline. |
| 100K | 100,000 | Production scale. Matches the DDP-B4 bench and the receiver file count where parallelism starts to pay. |
| 1M | 1,000,000 | Stress. Exposes memory-allocator pressure, shard imbalance, and cache-capacity effects at scale. |

For Target A the entry count is the number of directory keys. For Target B
it is the number of registered files (NDX values).

Keys are deterministic: `PathBuf::from(format!("dir/{group}/{n}"))` for
Target A, `FileNdx::new(n as u32)` for Target B. Combined with a fixed RNG
seed root (distinct from BR-3j.f's `0xB33D_BEEF_5EE0_C0DE`) the bench is
reproducible across machines.

## 5. Comparison baseline

Every cell for every store runs through the same timed code path (behind a
`trait MapStore`) so the only variable is the backing data structure. The
`mutex_hashmap` store is the comparison baseline for both targets.

For Target A, this extends the existing DDP-B4 bench (`delete_plan_map_
contention.rs`) with more thread counts (adding 32/64), more scale (adding
1M), and latency percentiles. The existing bench stays untouched so
criterion baselines remain comparable.

For Target B, the `mutex_hashmap` baseline reconstructs the pre-BR-3j shape
by wrapping a `Mutex<HashMap<FileNdx, SlotEntry>>` behind the same
`register/lookup/finish` API. This is a bench-only reconstruction - the
production `ParallelDeltaApplier` stays on DashMap.

## 6. Metrics

### 6.1 Throughput

- **ops/sec** via `Throughput::Elements(N)` where N is the total number of
  map operations (inserts + lookups + takes/finishes) per iteration.
- Criterion reports median, lower bound, and upper bound per cell.

### 6.2 Latency percentiles

- **p50 / p99 per-operation latency** captured by instrumenting each
  `insert` / `take` / `register` / `lookup` / `finish` call with
  `std::time::Instant::now()` elapsed.
- Latency samples are collected into a pre-allocated `Vec<Duration>` per
  thread, merged after the iteration, and sorted offline. The collection
  overhead is minimal (one `Instant::elapsed()` per op, ~25 ns on x86).
- Reported as a separate criterion group so the throughput numbers are
  uncontaminated by the instrumentation overhead. Two groups per target:
  - `dmb_a_delete_plan_map/throughput/...`
  - `dmb_a_delete_plan_map/latency/...`
  - `dmb_a_applier_files_map/throughput/...`
  - `dmb_a_applier_files_map/latency/...`

### 6.3 Shard contention (sampling)

DashMap 6.1 does not expose per-shard lock contention counters. To estimate
contention without modifying the crate:

- **perf stat** on Linux: capture `cache-misses`, `cache-references`, and
  `L1-dcache-load-misses` for the bench process. High cache-miss rates at
  high thread counts indicate cache-line bouncing between shards.
- **lock:contention tracepoint** (Linux 5.10+, `perf lock`): counts the
  number of times a futex wait was entered per lock site. DashMap's internal
  `RwLock` per shard maps to futex ops when contended. A rising
  `lock:contention` count at higher thread counts with diminishing throughput
  improvement is the signature of shard saturation.
- **DashMap::len() polling**: a background thread polls `map.len()` at 1 ms
  intervals during the timed section and records (timestamp, len) pairs.
  A non-monotonic `len()` curve during the mixed insert/take workload
  signals that the consumer is keeping up with producers (healthy) or
  falling behind (backpressure - not a DashMap issue but an access pattern
  issue).

These are supplementary diagnostics captured during offline number-capture
runs, not part of the CI criterion harness.

### 6.4 Memory overhead

Measure peak RSS via `/proc/self/status` (`VmHWM` on Linux) or the
`jemalloc_ctl` stats API if jemalloc is the allocator. Capture at each
scale tier for each store:

| Store | 10K RSS | 100K RSS | 1M RSS |
|-------|---------|----------|--------|
| `mutex_hashmap` | ... | ... | ... |
| `dashmap` (default shards) | ... | ... | ... |
| `dashmap` (128 shards) | ... | ... | ... |
| `dashmap` (256 shards) | ... | ... | ... |
| `sharded_mutex_hashmap` (16 shards) | ... | ... | ... |

DashMap's default shard count is `std::thread::available_parallelism() * 4`
rounded up to the next power of two. On a 16-core host this is 128 shards;
on a 64-core host, 512. Each shard carries a `RwLock` and a `HashMap`
allocation, so the fixed overhead scales with shard count even when the map
is empty.

The expected baseline: a single `HashMap<PathBuf, DeletePlan>` with 1M
entries and ~200 bytes per entry (PathBuf + DeletePlan) is ~200 MB. DashMap
with 128 shards adds 128 `RwLock` + 128 `HashMap` allocations, negligible
at 1M entries. The overhead becomes significant only if the shard count is
grossly oversized relative to the entry count (e.g., 512 shards for 10K
entries means ~20 entries per shard, and 512 allocations for metadata).

## 7. Measurement methodology

### 7.1 Criterion configuration

```rust
group.throughput(Throughput::Elements(total_ops as u64));
group.sample_size(20);       // 10K/100K tiers
group.sample_size(10);       // 1M tier (to stay within wall-clock budget)
group.measurement_time(Duration::from_secs(8));  // 10K/100K
group.measurement_time(Duration::from_secs(15)); // 1M
group.warm_up_time(Duration::from_secs(3));
```

Each sample rebuilds the map from scratch so population contention is
measured, not amortised by a pre-warmed map. `iter_batched` with
`BatchSize::LargeInput` keeps setup cost outside the timed window, matching
the pattern in `delete_plan_map_contention.rs`.

### 7.2 Reproducibility

- Deterministic keys: `format!("dir/{group}/{n}")` for Target A,
  `FileNdx::new(n as u32)` for Target B.
- Fixed RNG seed root: `0xDEAD_BEEF_CAFE_D00D` (distinct from BR-3j.f).
- Pre-built `DeletePlan` / `SlotEntry` values cloned into each sample so
  the timed loop only captures map operations.
- Rayon pool pinned to the target thread count per cell.

### 7.3 Offline number capture

Target hardware: bare-metal Linux host with 16+ physical cores, following
the BR-3j.f procedure (section 3 of
`docs/design/br-3j-f-dashmap-rebench-2026-05-21.md`). CI runners are
insufficient for shard contention measurement due to vCPU sharing and noisy
neighbours.

Commands:

```sh
cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --save-baseline dmb-a-post

# Compare against baseline from a pre-DashMap checkout for Target A:
git checkout <pre-dashmap-rev>
cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --save-baseline dmb-a-pre
git checkout -

# Contention profiling (Linux only, offline):
perf stat -e cache-misses,cache-references,L1-dcache-load-misses \
    cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --filter 'dashmap.*1M.*64_threads'

perf lock record -- \
    cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --filter 'dashmap.*1M.*64_threads'
perf lock report
```

## 8. Contention profiling

### 8.1 Cache-line bouncing

DashMap shards live on separate cache lines only if the shard stride exceeds
64 bytes (x86 L1 line size) or 128 bytes (Apple M-series). DashMap 6.1
does not pad shards; two adjacent shards may share a cache line, causing
false sharing. Detect via:

```sh
perf stat -e L1-dcache-load-misses,LLC-load-misses \
    cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --filter 'dashmap.*1M.*32_threads'
```

If L1-dcache-load-misses per op rises super-linearly with thread count while
throughput flattens, false sharing is the suspect. The fix is
`DashMap::with_shard_amount()` at a power-of-two count that aligns shard
boundaries to cache lines, or switching to a padded alternative.

### 8.2 lock:contention tracepoint

Linux 5.10+ exposes `lock:contention_begin` / `lock:contention_end`
tracepoints that fire on futex contention:

```sh
perf lock record -- \
    cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --filter 'dashmap.*1M.*64_threads'
perf lock report --sort acquired
```

The output shows lock-site addresses with contention counts. Cross-reference
with `perf annotate` to confirm the contention is inside DashMap's shard
`RwLock` vs the per-file `Mutex<FileSlot>` (Target B) or some other lock.

### 8.3 Interpretation

| Observation | Diagnosis | Action |
|-------------|-----------|--------|
| Throughput scales linearly to physical core count, flattens at hyper-thread count | Healthy. DashMap sharding is adequate. | Keep defaults. |
| Throughput flattens before physical core count, lock:contention rising | Shard saturation. Too few shards for the thread count. | Test `with_shard_amount(128)` or `with_shard_amount(256)`. |
| Throughput degrades past a threshold (negative scaling) | Lock convoy or false sharing. | Profile cache lines; consider padded shard alternatives. |
| Memory overhead > 20% vs `mutex_hashmap` at 1M entries | Shard metadata dominates. | Reduce shard count or accept the tradeoff. |

## 9. Shard tuning

### 9.1 What to test

DashMap 6.1 exposes `DashMap::with_capacity_and_shard_amount(cap, shards)`.
The bench tests the following shard counts in addition to the default:

| Shard count | Rationale |
|-------------|-----------|
| Default (`available_parallelism() * 4`, rounded to power-of-two) | Production default. On a 16-core host this is 128. |
| 128 | Fixed count matching a 16-core default, to test behaviour on hosts with fewer or more cores. |
| 256 | Double the 16-core default. Tests whether more shards reduce contention at 32/64 threads. |

The shard-count axis is swept only for the `dashmap` store at the 1M tier
(the tier where shard effects are most visible). Lower tiers use the default
shard count.

### 9.2 Decision gate

If `with_shard_amount(256)` shows > 15% throughput improvement over the
default at 32+ threads on the 1M tier, file a follow-up to expose a
`shard_amount` configuration knob in `ParallelDeltaApplier::new()` and
`DeletePlanMap::new()`. Otherwise, keep the default and document the result
in this spec's captured-numbers section.

## 10. CI integration

### 10.1 Workflow shape

Non-required, advisory-only workflow at
`.github/workflows/bench-dashmap-delete.yml`. Pattern mirrors
`.github/workflows/bench-drain-throughput.yml` (DPC-8) and
`.github/workflows/bench-daemon-coldstart.yml` (DIS-8.a):

```yaml
name: Bench DashMap delete throughput
on:
  workflow_dispatch:
  schedule:
    # Nightly at 06:17 UTC. Offset from drain-throughput (05:47),
    # daemon-coldstart (03:17), and daemon-concurrency (04:37).
    - cron: '17 6 * * *'
  pull_request:
    paths:
      - 'crates/engine/src/delete/plan_map.rs'
      - 'crates/engine/src/concurrent_delta/parallel_apply/**'
      - 'crates/engine/benches/dmb_a_dashmap_delete_bench.rs'
      - '.github/workflows/bench-dashmap-delete.yml'
concurrency:
  group: bench-dashmap-delete-${{ github.ref }}
  cancel-in-progress: true
```

### 10.2 Job configuration

- **Runner:** `ubuntu-latest` (2-4 vCPUs). Adequate for regression detection
  (relative delta between stores on the same runner) even though absolute
  numbers are noisy. Offline capture on bare-metal hardware produces the
  production-grade curve.
- **Timeout:** 30 minutes job-level, 300 seconds per bench step.
- **continue-on-error: true** - advisory only; does not block PRs.
- **Artifact upload:** criterion bencher-format output uploaded as
  `bench-dashmap-delete` artifact with 30-day retention.
- **Step summary:** first 200 lines of bencher output rendered in the
  GitHub Actions step summary.

### 10.3 Bench filter

CI runs only the 100K tier (production scale) to stay within the 5-minute
bench soft cap. The 10K and 1M tiers are reserved for offline runs:

```sh
timeout "${BENCH_TIMEOUT_SECONDS}" \
    cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --output-format bencher --measurement-time 10 '100k'
```

### 10.4 Promotion path

This workflow starts as non-required. Promotion to a required check is
tracked separately as DMB.a-ci and is contingent on:

1. Two weeks of nightly runs without spurious failures.
2. Coefficient of variation (CV) below 15% across nightly runs for every
   100K cell.
3. Agreement from at least one maintainer that the cell set is stable enough
   to gate PRs.

## 11. Decision criteria

### 11.1 Keep DashMap (default shards)

The default recommendation. Justified when:

- DashMap throughput is >= Mutex<HashMap> throughput at all tested thread
  counts and scale tiers (no regression).
- DashMap throughput scales past single-mutex saturation at 8+ threads on
  the 100K and 1M tiers.
- Memory overhead is < 20% vs Mutex<HashMap> at all scale tiers.
- No negative scaling (throughput degradation) at any tested thread count.

### 11.2 Tune DashMap shard count

Justified when:

- Default shard count shows saturation before physical core count (e.g.,
  throughput flattens at 8 threads on a 16-core host while
  `with_shard_amount(256)` continues scaling).
- Shard tuning shows > 15% throughput improvement at 32+ threads on the 1M
  tier.
- Memory overhead of the tuned shard count stays < 20% vs Mutex<HashMap>.

Action: expose `shard_amount` as a constructor parameter on
`ParallelDeltaApplier` and (if DeletePlanMap migrates to DashMap)
`DeletePlanMap`. Default to the DashMap library default; allow override for
high-core-count deployments.

### 11.3 Evaluate alternatives (flurry, papaya)

Justified when:

- DashMap shows negative scaling at tested thread counts (throughput
  decreases as threads increase).
- DashMap memory overhead exceeds 30% vs Mutex<HashMap> at the 1M tier.
- Shard tuning fails to eliminate contention diagnosed via lock:contention
  tracepoint.

Action: add `flurry` and/or `papaya` as additional stores behind the
`MapStore` trait and run the same sweep. Flurry is a lock-free
`ConcurrentHashMap` (Java-style epoch-based); papaya is a lock-free Robin
Hood hash map. Both eliminate the shard-level write lock that DashMap uses
but have higher per-operation overhead due to epoch reclamation. The bench
determines whether the crossover point (where lock-free overhead is
amortised by eliminated contention) falls within the tested thread/scale
range.

### 11.4 Keep Mutex<HashMap> for DeletePlanMap

Justified when:

- DashMap shows no throughput advantage at thread counts up to 16 on the
  100K tier (the production-relevant scale for the delete workload).
- The DDP pipeline's access pattern (N producers, 1 consumer) is
  insufficiently contended to benefit from sharding because the lock
  hold time is shorter than the lock acquisition overhead.

This is the current production shape. The bench validates the DDP-B4 note
that the single mutex is "the simplest correct shape" and not yet a
measured bottleneck. If confirmed, close DDP-A3 (#2253) as wontfix.

## 12. Bench file layout

```
crates/engine/benches/dmb_a_dashmap_delete_bench.rs
```

Registered in `crates/engine/Cargo.toml`:

```toml
[[bench]]
name = "dmb_a_dashmap_delete_bench"
harness = false
```

No feature gate required - DashMap is already a non-optional dependency of
the engine crate. The `parallel-receive-delta` feature gate is NOT applied
to this bench because Target A (DeletePlanMap) exercises the delete pipeline
which is feature-independent.

## 13. Cross-references

- `crates/engine/src/delete/plan_map.rs` - DeletePlanMap, current
  `Mutex<HashMap>` backing store. DDP-B4 note at line 26.
- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` -
  `ParallelDeltaApplier`, `files: DashMap<FileNdx, SlotEntry>` field.
- `crates/engine/benches/delete_plan_map_contention.rs` - DDP-B4 micro-bench
  (three-way: mutex vs dashmap vs sharded). Predecessor to Target A.
- `crates/engine/benches/br_3j_f_dashmap_cores_vs_throughput.rs` - BR-3j.f
  re-bench (post-DashMap delta-apply sweep). Predecessor to Target B.
- `docs/design/br-3j-f-dashmap-rebench-2026-05-21.md` - BR-3j.f methodology
  and deferred-numbers template.
- `docs/audits/br-3j-a-dashmap-vs-sharded-2026-05-20.md` - DashMap selection
  audit; contention model and shard-guard window assumptions.
- `.github/workflows/bench-drain-throughput.yml` - CI template for advisory
  bench workflows (DPC-8).
- `.github/workflows/bench-daemon-coldstart.yml` - CI template for advisory
  bench workflows (DIS-8.a).
