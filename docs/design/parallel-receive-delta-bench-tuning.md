# PIP-9.h.c - Bench-driven default tuning for parallel-receive-delta

Status: Design
Date: 2026-06-01
Tracker: PIP-9.h.c (#3018)
Predecessors:
- PIP-9.h.a (completed) - tuning spec enumerating knobs and sweep ranges
  (`docs/design/parallel-receive-delta-tuning.md`)
- PIP-9.h.b (completed) - implementation of WorkerPoolConfig knobs
  (`docs/design/pip-9hb-worker-pool-knobs-impl.md`)
- PIP-9.g.a (merged, PR #5267) - parallel vs sequential bench harness design
  (`docs/design/parallel-receive-delta-bench.md`)
Scope: Grid-search bench across worker count, batch size, and verify
threshold parameters on simulated hardware tiers. Produces evidence-backed
defaults for `WorkerPoolConfig` that ship as compile-time constants.

## 1. Objective

Pick default values for the three primary parallel-receive-delta knobs -
worker count, batch size, and threshold bytes - using bench evidence across
representative hardware and workloads. The defaults must satisfy:

1. No regression vs sequential on any workload (floor: 0.95x).
2. At least 2x throughput on the 90th percentile workload across the
   hardware matrix.
3. Graceful degradation on constrained hardware (4-core).
4. No resource waste on capable hardware (16-core).

## 2. Parameters under test

From PIP-9.h.a section 3 and PIP-9.h.b implementation:

| # | Parameter | Env var | Sweep values | Current default |
|---|-----------|---------|--------------|-----------------|
| 1 | Worker count | `OC_RSYNC_PARALLEL_RECEIVE_WORKERS` | 1, 2, 4, 6, 8, 12, 16 | `rayon::current_num_threads()` |
| 2 | Batch size | `OC_RSYNC_PARALLEL_RECEIVE_BATCH_SIZE` | 1, 4, 8, 16, 32, 64, 128 | 1 (apply_one_chunk shape) |
| 3 | Threshold bytes | `OC_RSYNC_PARALLEL_RECEIVE_THRESHOLD_BYTES` | 0, 1M, 4M, 16M, 64M, 256M | 0 (disabled) |

Queue depth is excluded from the grid search and kept at its existing
adaptive heuristic (`workers * capacity_multiplier`). PIP-9.h.a section 5
specifies that if queue depth co-varies completely with worker count, it
should remain a derivation rather than an independent parameter.

### 2.1 Parameter interactions

The sweep enforces the constraint matrix from PIP-9.h.a section 6:

- `batch_size <= queue_depth` (derived as `workers * 2` minimum)
- `batch_size >= workers` for non-degenerate configurations
- Invalid combinations are skipped rather than clamped

Total grid points per hardware tier: 7 workers x 7 batch sizes x 6
thresholds = 294. After constraint filtering (batch >= workers,
batch <= workers * 2): approximately 120 valid combinations per tier.

## 3. Hardware matrix

Physical hardware differences are simulated via rayon `ThreadPoolBuilder`
with explicit `num_threads` caps. This follows the methodology established
in PIP-9.g.a section 5.1.

| Tier | Simulated cores | Rayon pool cap | Representative hardware |
|------|-----------------|----------------|-------------------------|
| Low | 4 | 4 | Developer laptop (M1 efficiency cores, older i5) |
| Mid | 8 | 8 | Developer workstation (M3 Pro, Ryzen 5) |
| High | 16 | 16 | CI runner, build server (Xeon, EPYC, M3 Ultra) |

### 3.1 Pool construction

```rust
fn make_pool(cap: usize) -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(cap)
        .thread_name(|i| format!("pip9hc-{i}"))
        .build()
        .expect("rayon pool creation")
}
```

The pool is constructed once per hardware tier and reused across all
parameter combinations within that tier. Worker count values exceeding
the tier cap are skipped (e.g., workers=16 is invalid on the 4-core
tier).

### 3.2 Valid worker counts per tier

| Tier | Valid worker counts |
|------|--------------------|
| 4-core | 1, 2, 4 |
| 8-core | 1, 2, 4, 6, 8 |
| 16-core | 1, 2, 4, 6, 8, 12, 16 |

## 4. Workload matrix

Reuses the four profiles defined in PIP-9.g.a section 3. Each profile
exercises a distinct bottleneck regime relevant to the tuning parameters.

### 4.1 Profile definitions

| Profile | Files | Chunks/file | Chunk size | Total | Bottleneck |
|---------|-------|-------------|------------|-------|------------|
| Single large | 1 | 16,384 | 64 KiB | 1 GiB | Per-file Mutex (write-bound) |
| Many small | 100,000 | 1 | 4 KiB | 400 MiB | Registration + dispatch overhead |
| Mixed | 1,000 | 1-256 | 4-64 KiB | ~600 MiB | Heterogeneous scheduling |
| Delta-heavy | 500 | 128 | 64 KiB | 4 GiB | CPU-bound verify |

### 4.2 Profile relevance to parameters

| Profile | Worker count sensitivity | Batch size sensitivity | Threshold bytes sensitivity |
|---------|--------------------------|------------------------|-----------------------------|
| Single large | Low (Mutex-bound) | High (amortizes dispatch) | Low (always above threshold) |
| Many small | Medium (cross-file fan-out) | Low (1 chunk/file) | High (may fall below threshold) |
| Mixed | High (scheduling diversity) | Medium (variable chunk counts) | Medium (depends on aggregate) |
| Delta-heavy | High (CPU-bound verify) | High (batch amortization) | Low (always above threshold) |

## 5. Measurement methodology

### 5.1 Primary metric

**Throughput** (MB/s) - total bytes processed divided by wall-clock time.
Measured via criterion `iter_custom` with `Throughput::Bytes`. This is the
single metric used for default selection.

### 5.2 Secondary metrics

Captured in sidecar JSON for analysis but not used directly in default
selection:

- **Verify time ratio** - fraction of wall-clock spent in parallel verify
- **Write time ratio** - fraction spent in serialized per-file write
- **Queue backpressure events** - count of producer-blocked sends
- **Per-worker utilization** - atomic bump per worker per chunk processed
- **Peak RSS delta** - `getrusage` maxrss before and after bench

### 5.3 Measurement protocol

Each (hardware-tier, workload, parameter-combination) cell runs:

1. **Warmup:** 5 iterations (discarded)
2. **Measurement:** 20 iterations (criterion default sufficient for
   stable p50 at this granularity)
3. **Measurement time:** 10 seconds per cell minimum
4. **Output:** p50, p95, p99 wall-clock; throughput MB/s; coefficient of
   variation (CV). Cells with CV > 10% are flagged for re-run.

### 5.4 Environment

- CPU pinning via `taskset -c 0-{cap-1}` on Linux
- In-memory `VecSink` destinations (isolate from disk variance)
- Pre-allocated chunk vectors touched before timing (L3-warm)
- Machine otherwise idle during bench execution
- Repeat full grid 3 times; report median of medians

## 6. Analysis framework

### 6.1 Grid search output

The grid search produces a results matrix:

```
results[tier][workload][workers][batch_size][threshold_bytes] -> throughput_mbps
```

Total cells: 3 tiers x 4 workloads x ~120 valid combinations = ~1,440
data points.

### 6.2 Knee-point detection

For each (tier, workload) slice, plot throughput against each parameter
while holding others at their best values. The knee point is where
marginal throughput gain drops below 5% per unit increase:

```
knee(param) = min(v) where:
  throughput(v+1) / throughput(v) < 1.05
```

### 6.3 Diminishing returns threshold

Worker count diminishing returns: the point where adding one more worker
yields less than 10% throughput improvement. Beyond this point, the worker
is consuming resources (RSS, scheduling overhead) without proportional
benefit.

Batch size diminishing returns: the point where doubling the batch yields
less than 5% improvement. Beyond this, latency tail grows (larger batches
delay per-file completion) without throughput gain.

### 6.4 Worst-case regression detection

For each parameter combination, compute the minimum throughput ratio
against the sequential baseline (workers=1, batch=1, threshold=0) across
all workloads:

```
worst_case_ratio(params) = min over all workloads:
  throughput(params) / throughput(sequential_baseline)
```

Any combination where `worst_case_ratio < 0.95` is eliminated from
default candidacy. The 5% regression floor prevents configurations that
win on large transfers but regress on small ones.

### 6.5 Sensitivity analysis

Rank parameters by their contribution to throughput variance using ANOVA
or equivalent:

- If one parameter explains > 80% of variance: the other two should
  remain as derived heuristics, not independent defaults.
- If all three contribute meaningfully (> 15% each): all three deserve
  explicit defaults.

This implements PIP-9.h.a section 5 rollback criterion.

## 7. Default selection criteria

### 7.1 Primary selection rule

The default for each parameter is the value that maximizes the **90th
percentile throughput** across the full workload matrix on the **8-core
tier** (mid-tier hardware):

```
default(param) = argmax over valid values:
  P90(throughput[mid_tier][all_workloads][param][best_others])
```

The 8-core tier is the reference because it represents the median target
deployment (developer workstation, typical CI runner). Optimizing for the
median avoids over-tuning for either extreme.

### 7.2 Constraint: no regression on 4-core

The selected default must also satisfy:

```
for all workloads on 4-core tier:
  throughput(default_params) >= 0.95 * throughput(sequential_baseline)
```

This ensures the default does not regress on constrained hardware. If the
P90-optimal value regresses on 4-core, fall back to the highest value
that satisfies both the P90 criterion and the 4-core floor.

### 7.3 Constraint: no waste on 16-core

The selected default must not consume resources disproportionate to its
benefit on 16-core:

```
for the 16-core tier:
  throughput(default_params) >= 0.80 * throughput(tier_optimal_params)
```

This prevents a conservative default from leaving large speedups on the
table for server-class hardware. If the 8-core-optimal default achieves
less than 80% of the 16-core tier's maximum, the default should be
expressed as a formula (e.g., `min(num_cpus / 2, 8)`) rather than a fixed
constant.

### 7.4 Tie-breaking

When multiple parameter values are within 3% of each other on the P90
metric:

1. Prefer the lower value (less resource consumption).
2. Prefer the value that yields lower p99 latency (tail stability).
3. Prefer the value that is a power of two (alignment with hardware).

## 8. Adaptive tuning consideration

### 8.1 Question

Should defaults change based on detected hardware at runtime? Three
options:

| Option | Description | Complexity | Benefit |
|--------|-------------|------------|---------|
| A | Fixed constants | Zero | Simple, predictable, testable |
| B | Core-count formula | Low | Scales automatically |
| C | Runtime profiling | High | Optimal per-machine |

### 8.2 Decision criteria

If the bench evidence shows that the optimal worker count on 4-core
differs from 8-core by more than 2x, a core-count formula (Option B) is
justified. Expected formulas if adaptive is warranted:

```rust
/// Worker count: half of available cores, capped at 8.
fn default_workers() -> usize {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    cpus.div_ceil(2).min(8)
}

/// Batch size: scale with worker count for dispatch amortization.
fn default_batch_size(workers: usize) -> usize {
    (workers * 4).max(8).min(64)
}

/// Threshold bytes: fixed regardless of hardware.
fn default_threshold_bytes() -> u64 {
    16 * 1024 * 1024 // 16 MiB
}
```

### 8.3 Option C rejection

Runtime profiling (Option C) is rejected outright:

- Adds first-transfer latency (profiling overhead before steady-state).
- Requires a feedback loop that couples the worker pool to transfer
  progress, creating a testing surface that interop cannot validate.
- Violates upstream rsync behavioral equivalence - upstream uses no
  adaptive parallelism.

### 8.4 Decision gate

The bench results determine which option ships:

- If optimal worker count is `4` across all three tiers: ship Option A
  (fixed constant `4`).
- If optimal worker count is `{2, 4, 8}` for `{4-core, 8-core, 16-core}`
  respectively: ship Option B with `min(cpus / 2, 8)`.
- If results are noisy with no clear pattern: ship Option A with the
  most conservative value that satisfies section 7 criteria.

## 9. Bench harness implementation

### 9.1 File location

```
crates/engine/benches/pip9hc_tuning_grid.rs
```

Follows the pattern established by `pip9g_parallel_vs_sequential.rs`.
Uses criterion with custom groups per hardware tier.

### 9.2 Structure

```rust
fn bench_tuning_grid(c: &mut Criterion) {
    let tiers = [(4, "4core"), (8, "8core"), (16, "16core")];
    let workloads = [
        WorkloadProfile::single_large(),
        WorkloadProfile::many_small(),
        WorkloadProfile::mixed(),
        WorkloadProfile::delta_heavy(),
    ];
    let worker_counts = [1, 2, 4, 6, 8, 12, 16];
    let batch_sizes = [1, 4, 8, 16, 32, 64, 128];
    let thresholds_mb = [0, 1, 4, 16, 64, 256];

    for &(tier_cap, tier_name) in &tiers {
        let pool = make_pool(tier_cap);
        let mut group = c.benchmark_group(format!("pip9hc/{tier_name}"));
        group.measurement_time(Duration::from_secs(10));
        group.warm_up_time(Duration::from_secs(3));

        for workload in &workloads {
            for &workers in &worker_counts {
                if workers > tier_cap { continue; }
                for &batch in &batch_sizes {
                    if batch < workers { continue; }
                    if batch > workers * 2 { continue; } // queue depth constraint
                    for &thresh_mb in &thresholds_mb {
                        let thresh = thresh_mb as u64 * 1024 * 1024;
                        let config = WorkerPoolConfig {
                            workers: Some(workers),
                            batch_size: Some(batch),
                            queue_depth: None, // adaptive
                            threshold_bytes: Some(thresh),
                        };
                        let id = BenchmarkId::new(
                            &workload.name,
                            format!("w{workers}_b{batch}_t{thresh_mb}M"),
                        );
                        group.throughput(Throughput::Bytes(
                            workload.total_bytes as u64,
                        ));
                        group.bench_with_input(id, &config, |b, cfg| {
                            pool.install(|| {
                                b.iter_custom(|iters| {
                                    run_tuning_cell(workload, cfg, iters)
                                });
                            });
                        });
                    }
                }
            }
        }
        group.finish();
    }
}
```

### 9.3 Sidecar output

Each cell writes metrics to:

```
target/pip-9hc-sidecar/{tier}/{workload}/w{N}_b{N}_t{N}M.json
```

Format matches PIP-9.g.a section 4.1 with additional fields:

```json
{
  "tier": "8core",
  "workload": "delta_heavy",
  "workers": 4,
  "batch_size": 16,
  "threshold_mb": 16,
  "throughput_mbps": 2847.3,
  "p50_ns": 3412000,
  "p95_ns": 3890000,
  "p99_ns": 4210000,
  "cv_pct": 4.2,
  "verify_ratio_pct": 62.1,
  "write_ratio_pct": 37.9,
  "queue_backpressure_events": 0,
  "peak_rss_delta_bytes": 8388608
}
```

### 9.4 Analysis script

```
tools/analyze_pip9hc.py
```

Reads sidecar JSON, computes:
- Per-parameter knee points
- Sensitivity ranking (ANOVA-like variance decomposition)
- Default candidates per section 7 criteria
- Regression detection against sequential baseline
- Summary table for the design decision

Output: `target/pip-9hc-sidecar/analysis.md` with recommended defaults
and supporting evidence.

## 10. Expected outcomes

### 10.1 Worker count prediction

Based on PIP-9.g.a section 5.2 scaling curves and BR-3i.f results:

- **4-core tier:** Optimal at 2 workers (verify overlaps with write;
  3rd and 4th workers contend on Mutex and L2 cache)
- **8-core tier:** Optimal at 4 workers (diminishing returns beyond 4
  on mixed workloads due to per-file write serialization)
- **16-core tier:** Optimal at 6-8 workers (delta-heavy benefits from
  more verify parallelism; small-file saturates at DashMap sharding)

Expected default formula: `min(num_cpus / 2, 8)`.

### 10.2 Batch size prediction

- Batch=1 (current): high dispatch overhead per chunk
- Batch=8-16: expected sweet spot where dispatch cost is amortized but
  latency tail remains bounded
- Batch=64+: diminishing returns; larger batches delay per-file
  completion without proportional throughput gain

Expected default: 16 (or `workers * 2`).

### 10.3 Threshold bytes prediction

- 0 (disabled): small-file transfers pay dispatch overhead for no gain
- 1-4 MiB: too aggressive; many legitimate transfers fall below
- 16 MiB: expected sweet spot; filters out `rsync .config/` style
  transfers while permitting `rsync ~/Documents/` style transfers
- 256 MiB+: too conservative; denies parallelism to common workloads

Expected default: 16 MiB.

## 11. Deliverables

1. **Bench binary** at `crates/engine/benches/pip9hc_tuning_grid.rs`
   implementing the grid search described in section 9.
2. **Analysis script** at `tools/analyze_pip9hc.py` consuming sidecar
   output and producing the default recommendation.
3. **Constants PR** updating `WorkerPoolConfig` defaults based on bench
   evidence:
   - `DEFAULT_PARALLEL_RECEIVE_WORKERS` in
     `crates/engine/src/concurrent_delta/worker_pool_config.rs`
   - `DEFAULT_PARALLEL_RECEIVE_BATCH_SIZE` in
     `crates/engine/src/concurrent_delta/worker_pool_config.rs`
   - `DEFAULT_PARALLEL_RECEIVE_THRESHOLD_BYTES` in
     `crates/transfer/src/delta_pipeline/threshold.rs`
4. **Adaptive formula** if bench evidence warrants (section 8.4 gate).

## 12. Success criteria

The bench-selected defaults are accepted when:

1. P90 throughput across all workloads on 8-core >= 2x sequential
   baseline.
2. Worst-case throughput on 4-core >= 0.95x sequential baseline.
3. 16-core throughput >= 80% of the tier-optimal configuration.
4. CV < 10% on all measured cells (reproducibility).
5. No queue backpressure events on any profile at the default
   configuration (queue depth headroom is sufficient).

## 13. Out of scope

- **Queue depth as independent parameter.** Remains a derived
  heuristic (`workers * capacity_multiplier`). Promoted only if
  sensitivity analysis shows it explains > 15% of throughput variance
  independently of worker count.
- **Per-file reorder capacity.** PIP-9.h.a section 3.5 deferred
  until evidence shows the reorder buffer is the dominant
  backpressure surface. If the bench shows high-watermark metrics
  hitting the cap, this becomes PIP-9.h.d.
- **Disk I/O interaction.** The bench uses in-memory sinks. Disk
  interaction is covered by PIP-6 end-to-end bench on real
  storage.
- **Sender-side tuning.** Sender parallelism is a separate concern.
- **Wire protocol changes.** The parallel path is wire-compatible by
  design.

## 14. References

- `docs/design/parallel-receive-delta-tuning.md` - PIP-9.h.a tuning spec
- `docs/design/pip-9hb-worker-pool-knobs-impl.md` - PIP-9.h.b impl spec
- `docs/design/parallel-receive-delta-bench.md` - PIP-9.g.a bench design
- `docs/design/pip-6-end-to-end-parallel-vs-sequential-bench-2026-05-21.md` -
  PIP-6 end-to-end bench
- `crates/engine/benches/parallel_receive_delta_perf.rs` - BR-3i.f
  apply-loop bench
- `crates/engine/src/concurrent_delta/worker_pool_config.rs` -
  WorkerPoolConfig struct
- `crates/engine/src/concurrent_delta/parallel_apply/batch.rs` -
  apply_batch_parallel implementation
- `crates/transfer/src/delta_pipeline/threshold.rs` - dispatch threshold
