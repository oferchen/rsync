# DMB.b - DashMap delete bench thread-count sweep

Date: 2026-05-26
Status: Design spec
Tracker: DMB.b. Predecessor: DMB.a (DashMap delete bench harness).
Follow-up: DMB.c (DashMap vs Mutex comparison at 100K), DMB.d (comparison
at 1M).

## 1. Motivation

DMB.a designed a unified criterion harness (`dmb_a_dashmap_delete_bench.rs`)
that benchmarks the delete workload's two DashMap consumers -
`DeletePlanMap` (Target A) and `ParallelDeltaApplier` files map (Target B) -
across backing-store alternatives. The harness sweeps worker counts
`{1, 2, 4, 8, 16, 32, 64}` at three scale tiers (10K, 100K, 1M entries).

DMB.b executes the 100K-entry tier of that sweep, captures the
cores-vs-throughput curve, and identifies the scaling inflection point where
DashMap's default shard layout saturates. The 100K tier is the
production-relevant scale: it matches the DDP-B4 bench
(`delete_plan_map_contention.rs`) entry count and the receiver file-count
threshold where parallelism starts to pay off.

The results feed directly into DMB.c and DMB.d, which add the
`Mutex<HashMap>` comparison baseline at 100K and 1M respectively.

## 2. Bench harness location

### 2.1 Source file

```
crates/engine/benches/dmb_a_dashmap_delete_bench.rs
```

Registered in `crates/engine/Cargo.toml` as:

```toml
[[bench]]
name = "dmb_a_dashmap_delete_bench"
harness = false
```

### 2.2 Invocation

Full sweep (all scale tiers, all thread counts, all stores):

```sh
cargo bench -p engine --bench dmb_a_dashmap_delete_bench
```

DMB.b's 100K-only sweep (filters to the production-scale tier):

```sh
cargo bench -p engine --bench dmb_a_dashmap_delete_bench -- '100k'
```

Single-cell drill-down (e.g., DashMap at 32 threads on 100K):

```sh
cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --filter 'dashmap.*100k.*32_threads'
```

Save a named baseline for later comparison:

```sh
cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --save-baseline dmb-b-$(date +%Y%m%d) '100k'
```

## 3. Thread count matrix

The sweep covers seven thread counts that form a power-of-two progression
from single-threaded baseline to heavy over-subscription:

| Threads | Purpose |
|---------|---------|
| 1 | Uncontended baseline. Establishes per-op cost with zero lock contention. |
| 2 | Minimal concurrency. Validates that the map does not regress under two concurrent accessors. |
| 4 | Light contention. Typical CI runner core count. |
| 8 | Moderate contention. Common laptop/workstation core count. |
| 16 | DashMap default shard count boundary. `DashMap::new()` creates `available_parallelism() * 4` shards rounded to power-of-two. On a 4-core host this is 16 shards - the point where one thread per shard is possible. |
| 32 | Over-subscription on most hosts. Exposes whether DashMap scales past physical cores via hyper-threading headroom or degrades under lock-convoy effects. |
| 64 | Heavy over-subscription. Stress-tests shard contention, cache-line bouncing, and scheduler overhead. |

Each cell pins the ambient rayon pool to the target worker count via
`ThreadPoolBuilder::new().num_threads(N)` and dispatches work through
`pool.scope()` or `pool.install()`. This matches the pattern established in
BR-3j.f (`br_3j_f_dashmap_cores_vs_throughput.rs`) and DDP-B4
(`delete_plan_map_contention.rs`).

Counts above the host's `available_parallelism()` still produce meaningful
data: they expose over-subscription behaviour. The sweep deduplicates
against `available_parallelism()` so hosts with exactly 32 or 64 cores do
not produce duplicate cells.

## 4. Workload sizing

### 4.1 Entry count

100,000 entries in the DashMap, matching the existing DDP-B4 bench's
`TOTAL_OPS` constant and the production scale where the delete pipeline's
parallelism starts to pay off.

### 4.2 DeletePlanEntry payloads (Target A)

Each `DeletePlan` is constructed with realistic content:

- **Directory key:** `PathBuf::from(format!("dir/{group}/{n}"))` - deterministic,
  reproducible across machines.
- **Entries per plan:** 1 `DeleteEntry` with a synthetic filename
  (`OsString::from(format!("entry-{n}"))`) and `DeleteEntryKind::File`. This
  matches the DDP-B4 bench pattern. The entry count per plan is intentionally
  low to isolate map-operation cost from plan-construction cost.
- **Pre-built plans:** All `DeletePlan` values are constructed outside the timed
  section via `iter_batched` with `BatchSize::LargeInput`. The timed loop
  captures only map operations (insert, take), not allocation.

### 4.3 SlotEntry payloads (Target B)

Each `SlotEntry` for the `ParallelDeltaApplier` files map:

- **Key:** `FileNdx::new(n as u32)` - integer key, zero allocation.
- **Value:** A `SlotEntry` wrapping a `CountingSink` writer (same as BR-3j.f),
  keeping per-file write cost at zero so the bench isolates map operations
  from I/O.
- **Three phases per iteration:** register (insert), lookup (random access),
  finish (remove + Arc unwrap).

### 4.4 RNG seed

Fixed seed root `0xDEAD_BEEF_CAFE_D00D` (as specified in DMB.a), distinct
from BR-3j.f's `0xB33D_BEEF_5EE0_C0DE`. Combined with `(workload_tag,
group, n)` per key so the corpus is reproducible across runs and machines.

## 5. Metrics

### 5.1 Throughput (ops/sec per thread count)

Criterion reports throughput via `Throughput::Elements(total_ops)` where
`total_ops` is the total number of map operations per iteration:

- **Target A:** `total_ops = inserts + takes = 100K + 100K = 200K` for the
  mixed insert/take workload, or `100K` for insert-only.
- **Target B:** `total_ops = registers + lookups + finishes = 100K + 100K +
  100K = 300K`.

Criterion emits median, lower bound, and upper bound per cell. The primary
metric is **median ops/sec** plotted against thread count.

Expected shape:

```
ops/sec
  ^
  |          .-----.-----*
  |        ./             \  (sub-linear or flat beyond inflection)
  |      ./
  |    ./
  |  ./
  | /
  |/
  +--+--+--+--+--+--+--+--> threads
     1  2  4  8  16 32 64
```

### 5.2 Scaling efficiency

Computed post-hoc from the throughput numbers:

```
efficiency(N) = ops_at_N_threads / (N * ops_at_1_thread)
```

| Efficiency | Interpretation |
|------------|----------------|
| 1.0 | Perfect linear scaling. |
| 0.5-0.99 | Sub-linear but healthy. Typical for shared data structures. |
| < 0.5 | Significant contention. Investigate shard saturation. |
| > 1.0 | Super-linear (cache effects). Possible but rare. |

### 5.3 Latency percentiles (p50, p99)

Captured in a separate criterion group to avoid contaminating throughput
numbers with instrumentation overhead:

- Each `insert` / `take` / `register` / `lookup` / `finish` call is
  bracketed by `std::time::Instant::now()` and `elapsed()`.
- Per-thread latency samples are collected into a pre-allocated
  `Vec<Duration>` (capacity = per-thread op count), merged after the
  iteration, and sorted offline.
- **p50** and **p99** are reported as named criterion benchmarks:
  - `dmb_b_delete_plan_map/latency_p50/100k/{threads}`
  - `dmb_b_delete_plan_map/latency_p99/100k/{threads}`
  - `dmb_b_applier_files_map/latency_p50/100k/{threads}`
  - `dmb_b_applier_files_map/latency_p99/100k/{threads}`

Expected behaviour: p50 stays roughly flat as threads increase (DashMap
shards absorb concurrency). p99 rises at the inflection point where shard
contention begins, with the rise rate indicating how sharply contention
degrades tail latency.

The instrumentation overhead is approximately 25 ns per op on x86
(`Instant::now()` + `elapsed()`). At 100K ops this adds ~2.5 ms per
iteration - negligible relative to the map operation cost at scale.

## 6. Expected outcome

### 6.1 Scaling prediction

DashMap 6.1 creates `available_parallelism() * 4` shards, rounded up to the
next power of two. On a typical 4-core CI runner this is 16 shards; on a
16-core bare-metal host, 128 shards.

The expected scaling behaviour:

| Thread count | Expected scaling | Rationale |
|-------------|------------------|-----------|
| 1-4 | Near-linear | Threads < shards. Each thread likely lands on its own shard. Negligible contention. |
| 4-16 | Near-linear to slightly sub-linear | On a 4-core host, threads equal or exceed physical cores at 4. Shard count (16) still exceeds thread count, so map-level contention is low. CPU scheduling overhead begins. |
| 16-32 | Sub-linear | Threads approach or exceed shard count on small hosts. Multiple threads compete for the same shard's `RwLock`. OS context-switch overhead grows. |
| 32-64 | Flat or negative | Heavy over-subscription. Shard contention and cache-line bouncing dominate. Throughput may decrease if lock-convoy effects appear. |

### 6.2 Inflection point

The inflection point is the thread count where scaling efficiency drops
below 0.5. Based on the DashMap shard count formula, this is expected at
approximately `shard_count / 2` threads - around 8 threads on a 4-core
CI runner (16 shards) or around 64 threads on a 16-core bare-metal host
(128 shards).

Identifying this inflection point is the primary deliverable of DMB.b. It
determines:

1. Whether the current DashMap default shard count is adequate for the
   delete workload's concurrency level.
2. Whether shard tuning (DMB.a section 9) is worth pursuing.
3. The thread-count ceiling above which adding more delete workers yields
   diminishing returns.

## 7. How results feed into DMB.c and DMB.d

### 7.1 DMB.c - DashMap vs Mutex at 100K

DMB.c adds the `mutex_hashmap` backing store to the same 100K sweep that
DMB.b captures. The DMB.b numbers serve as the DashMap side of the
comparison. DMB.c runs both stores at all seven thread counts and produces
a side-by-side throughput curve.

The DMB.b baseline must be captured first so DMB.c can use criterion's
`--baseline` comparison:

```sh
# DMB.b: capture DashMap baseline
cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --save-baseline dmb-b '100k.*dashmap'

# DMB.c: compare Mutex against DashMap baseline
cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --baseline dmb-b '100k.*mutex_hashmap'
```

### 7.2 DMB.d - DashMap vs Mutex at 1M

DMB.d repeats the DMB.c comparison at 1M entries to expose
memory-allocator pressure, shard imbalance, and cache-capacity effects that
are invisible at 100K. The DMB.b methodology (thread sweep, metrics,
criterion configuration) carries over unchanged; only the scale tier
filter changes from `'100k'` to `'1M'`.

### 7.3 Decision flow

```
DMB.b (DashMap 100K sweep)
  |
  +-> inflection point identified
  |
  +-> DMB.c (add Mutex baseline at 100K)
  |     |
  |     +-> crossover point: thread count where DashMap > Mutex
  |
  +-> DMB.d (both stores at 1M)
        |
        +-> memory overhead comparison
        +-> shard tuning decision (DMB.a section 9)
```

## 8. Measurement methodology

### 8.1 Criterion configuration

```rust
group.throughput(Throughput::Elements(total_ops as u64));
group.sample_size(20);
group.measurement_time(Duration::from_secs(8));
group.warm_up_time(Duration::from_secs(3));
```

Each sample rebuilds the map from scratch so population contention is
measured, not amortised by a pre-warmed map. `iter_batched` with
`BatchSize::LargeInput` keeps setup cost outside the timed window.

### 8.2 Reproducibility

- Deterministic keys: `format!("dir/{group}/{n}")` for Target A,
  `FileNdx::new(n as u32)` for Target B.
- Fixed RNG seed root: `0xDEAD_BEEF_CAFE_D00D`.
- Pre-built `DeletePlan` / `SlotEntry` values cloned into each sample.
- Rayon pool pinned to the target thread count per cell.

### 8.3 Statistical validation

Each cell must achieve a coefficient of variation (CoV) below 5% across
criterion's samples. Cells with CoV above 5% are flagged in the results
and re-run with `--sample-size 50 --measurement-time 15` to determine
whether the variance is intrinsic (contention jitter) or environmental
(noisy neighbour on CI runner).

The CoV threshold distinguishes:

| CoV | Interpretation |
|-----|----------------|
| < 5% | Stable. Baseline is reliable for regression detection. |
| 5-15% | Marginal. Acceptable for offline captures on bare-metal; too noisy for CI gating. |
| > 15% | Unstable. Root-cause the variance before drawing conclusions. |

### 8.4 Offline number capture

Target hardware: bare-metal Linux host with 16+ physical cores, following
the BR-3j.f procedure (section 3 of
`docs/design/br-3j-f-dashmap-rebench-2026-05-21.md`). CI runners are
insufficient for absolute throughput numbers due to vCPU sharing and noisy
neighbours, but adequate for regression detection (relative delta between
runs on the same runner).

Offline capture commands:

```sh
cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --save-baseline dmb-b-$(hostname)-$(date +%Y%m%d) '100k'
```

## 9. CI integration

### 9.1 Workflow

Non-required, advisory-only workflow at
`.github/workflows/bench-dashmap-delete.yml` as defined in DMB.a section
10. DMB.b does not create a separate workflow - it uses the same workflow
with the `'100k'` filter that DMB.a specifies for CI runs.

### 9.2 Nightly schedule

```yaml
schedule:
  - cron: '17 6 * * *'
```

Offset from drain-throughput (05:47 UTC), daemon-coldstart (03:17 UTC), and
daemon-concurrency (04:37 UTC) to avoid runner contention.

### 9.3 CI bench filter

CI runs only the 100K tier to stay within the 5-minute bench soft cap:

```sh
timeout "${BENCH_TIMEOUT_SECONDS}" \
    cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- --output-format bencher --measurement-time 10 '100k'
```

### 9.4 Thread count on CI

CI runners (`ubuntu-latest`) typically have 2-4 vCPUs. The sweep still
runs all seven thread counts (1-64) on CI. Cells with threads > vCPU count
measure over-subscription behaviour, which is meaningful for regression
detection even if the absolute numbers differ from bare-metal.

The `available_parallelism()` deduplication ensures that if the runner
reports 4 cores, the `4` cell does not duplicate.

### 9.5 Artifact and summary

- Criterion bencher-format output uploaded as `bench-dashmap-delete`
  artifact with 30-day retention.
- First 200 lines of bencher output rendered in the GitHub Actions step
  summary for inline review.

## 10. Success criteria

### 10.1 Baseline captured with < 5% CoV

Every cell in the 100K tier must achieve a coefficient of variation below
5% across criterion's sample set. This ensures the baseline is stable
enough to detect regressions in future runs.

Validation:

```sh
# Criterion reports CoV as "thrpt" noise percentage in bencher output.
# Cells with noise > 5% are flagged for re-run.
cargo bench -p engine --bench dmb_a_dashmap_delete_bench \
    -- '100k' 2>&1 | grep -E 'thrpt.*[0-9]' | awk '{print $NF}' | \
    while read pct; do
      val=$(echo "$pct" | tr -d '%+-')
      if [ "$(echo "$val > 5" | bc)" -eq 1 ]; then
        echo "WARN: cell noise $pct exceeds 5% threshold"
      fi
    done
```

### 10.2 Scaling inflection point identified

The results must clearly identify the thread count where scaling efficiency
drops below 0.5 (or confirm that it remains above 0.5 across all tested
thread counts). The inflection point is reported as:

- Thread count at which `efficiency(N) < 0.5`.
- Absolute throughput (ops/sec) at 1 thread and at the inflection point.
- Whether the inflection aligns with the predicted shard-count boundary.

### 10.3 No negative scaling below 16 threads

DashMap must not show throughput degradation (negative scaling) at thread
counts up to 16. Negative scaling below 16 threads would indicate a
fundamental problem with the shard layout for this workload and would
trigger the DMB.a section 11.3 evaluation of lock-free alternatives.

### 10.4 Latency percentiles captured

p50 and p99 per-operation latency must be captured at all seven thread
counts. The p99/p50 ratio at each thread count characterises tail-latency
amplification under contention:

| p99/p50 ratio | Interpretation |
|---------------|----------------|
| < 2x | Low contention. Healthy. |
| 2-5x | Moderate contention. Acceptable for the delete workload. |
| > 5x | High contention. Investigate shard saturation or false sharing. |

## 11. Results template

Numbers are captured offline and appended here once measured.

### 11.1 Target A - DeletePlanMap (100K entries)

| Threads | Ops/sec (median) | Efficiency | p50 (ns) | p99 (ns) | p99/p50 | CoV |
|---------|-----------------|------------|----------|----------|---------|-----|
| 1 | | | | | | |
| 2 | | | | | | |
| 4 | | | | | | |
| 8 | | | | | | |
| 16 | | | | | | |
| 32 | | | | | | |
| 64 | | | | | | |

### 11.2 Target B - ParallelDeltaApplier files map (100K entries)

| Threads | Ops/sec (median) | Efficiency | p50 (ns) | p99 (ns) | p99/p50 | CoV |
|---------|-----------------|------------|----------|----------|---------|-----|
| 1 | | | | | | |
| 2 | | | | | | |
| 4 | | | | | | |
| 8 | | | | | | |
| 16 | | | | | | |
| 32 | | | | | | |
| 64 | | | | | | |

### 11.3 Inflection point summary

| Target | Inflection thread count | Efficiency at inflection | Shard count (host) |
|--------|------------------------|-------------------------|-------------------|
| A (DeletePlanMap) | | | |
| B (Applier files) | | | |

## 12. Cross-references

- `docs/design/dmb-a-dashmap-delete-bench-harness.md` - DMB.a harness spec.
  Defines the bench file layout, backing-store trait, scale tiers, and CI
  workflow shape that DMB.b depends on.
- `crates/engine/benches/dmb_a_dashmap_delete_bench.rs` - The bench file
  that DMB.b invokes (created by DMB.a).
- `crates/engine/benches/delete_plan_map_contention.rs` - DDP-B4
  micro-bench. Predecessor to Target A. Sweeps 4/8/16 threads at 100K ops.
  DMB.b extends the sweep to 1/2/32/64 threads.
- `crates/engine/benches/br_3j_f_dashmap_cores_vs_throughput.rs` - BR-3j.f
  re-bench. Predecessor to Target B. Sweeps 1/2/4/8/parallelism threads.
  DMB.b extends to 32/64.
- `crates/engine/src/delete/plan_map.rs` - DeletePlanMap production code.
  Current `Mutex<HashMap>` backing store.
- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` -
  ParallelDeltaApplier production code. `files: DashMap<FileNdx, SlotEntry>`.
- `docs/design/br-3j-f-dashmap-rebench-2026-05-21.md` - BR-3j.f
  methodology and offline number-capture procedure.
- `docs/audits/br-3j-a-dashmap-vs-sharded-2026-05-20.md` - DashMap
  selection audit. Contention model and shard-guard window assumptions.
- `.github/workflows/bench-dashmap-delete.yml` - CI workflow (DMB.a
  section 10).
- `.github/workflows/bench-drain-throughput.yml` - Template CI bench
  workflow (DPC-8).
