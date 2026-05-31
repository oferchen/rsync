# PIP-9.g.b - Parallel receive-delta bench outcome template

Status: Design
Date: 2026-06-01
Tracker: PIP-9.g.b
Predecessors:
- PIP-9.g.a (merged, PR #5267) - bench harness design
- PIP-9.h.c (design) - bench-driven tuning defaults
Scope: Defines the structure, statistical reporting format, and decision
criteria for the bench results document produced after PIP-9.g.a executes
on controlled hardware.

---

## 1. Purpose

This document is a template - it specifies what the actual bench outcome
report will contain, how numbers are formatted, and how results map to
tuning decisions. The actual results document will be created after bench
execution on dedicated Linux hardware and will follow this structure
exactly.

The outcome report serves three audiences:

1. **Tuning (PIP-9.h.c):** Provides the raw data for selecting defaults.
2. **Validation:** Confirms or refutes PIP-9's default-on decision.
3. **Regression baseline:** Establishes reference numbers for future CI
   comparison.

---

## 2. Document metadata section

Every outcome report begins with a metadata block capturing
reproducibility information.

```markdown
## Metadata

| Field              | Value                          |
|--------------------|--------------------------------|
| Date               | YYYY-MM-DD HH:MM UTC          |
| Hardware           | (see section 2.1)              |
| OS                 | Linux <kernel version>         |
| Rust toolchain     | rustc <version> (stable)       |
| oc-rsync commit    | <short SHA>                    |
| Criterion version  | <version>                      |
| Rayon version      | <version>                      |
| CPU governor       | performance                    |
| Turbo boost        | disabled                       |
| Isolation          | taskset -c 0-N                 |
| Ambient load       | idle (no other user processes) |
| Reproducer command | (see section 2.2)              |
```

### 2.1 Hardware specification

```markdown
| Component  | Specification                          |
|------------|----------------------------------------|
| CPU        | Model, cores, threads, base/boost MHz  |
| L1d/L1i    | Size per core                          |
| L2         | Size per core                          |
| L3/LLC     | Total size                             |
| RAM        | Total, speed, channels                 |
| Storage    | Model, interface (NVMe/SATA), for disk cells |
| NUMA nodes | Count, topology                        |
```

### 2.2 Reproducer command

The exact commands to reproduce the run, including environment variables,
CPU pinning, and feature flags:

```sh
# Sequential baseline
sudo cpupower frequency-set -g performance
echo 0 | sudo tee /sys/devices/system/cpu/cpufreq/boost

taskset -c 0-7 cargo bench --bench pip9g_parallel_vs_sequential \
  --no-default-features --features 'zstd lz4 xattr iconv' \
  -- --save-baseline sequential

# Parallel (default features)
taskset -c 0-7 cargo bench --bench pip9g_parallel_vs_sequential \
  --features parallel-receive-delta \
  -- --baseline sequential
```

---

## 3. Results summary table

The primary results table presents throughput (MB/s) for each workload x
worker-count cell. This is the first thing readers look at.

### 3.1 Format

```markdown
## Results summary

### Throughput (MB/s) - median of 100 iterations

| Workload          | Sequential | P(1) | P(2) | P(4) | P(8) | P(16) |
|-------------------|-----------|-------|-------|-------|-------|--------|
| Single large file | ____      | ____  | ____  | ____  | ____  | ____   |
| Many small files  | ____      | ____  | ____  | ____  | ____  | ____   |
| Mixed directory   | ____      | ____  | ____  | ____  | ____  | ____   |
| Delta-heavy       | ____      | ____  | ____  | ____  | ____  | ____   |

### Speedup vs sequential baseline

| Workload          | P(1)  | P(2)  | P(4)  | P(8)  | P(16) |
|-------------------|-------|-------|-------|-------|--------|
| Single large file | __.__ | __.__ | __.__ | __.__ | __.__  |
| Many small files  | __.__ | __.__ | __.__ | __.__ | __.__  |
| Mixed directory   | __.__ | __.__ | __.__ | __.__ | __.__  |
| Delta-heavy       | __.__ | __.__ | __.__ | __.__ | __.__  |
```

Values use two decimal places for speedup ratios (e.g., `3.14x`) and
integer MB/s for throughput. P(N) denotes parallel path with N workers.

### 3.2 Overhead row

A dedicated row captures the cost of parallel infrastructure when
parallelism is unavailable:

```markdown
### Parallel overhead (P(1) vs Sequential)

| Workload          | Overhead % | Per-chunk cost (ns) |
|-------------------|-----------|---------------------|
| Single large file | __._      | ___                 |
| Many small files  | __._      | ___                 |
| Mixed directory   | __._      | ___                 |
| Delta-heavy       | __._      | ___                 |
```

---

## 4. Scaling curve visualization

### 4.1 Specification

Each workload gets a line chart with:

- **X-axis:** Worker count (1, 2, 4, 8, 16) - log2 scale
- **Y-axis:** Throughput (MB/s) - linear scale
- **Lines:** One solid line for parallel path, one dashed horizontal for
  sequential baseline
- **Error bars:** IQR (25th-75th percentile) at each point
- **Ideal line:** Dashed diagonal showing linear scaling from sequential
  baseline (theoretical maximum)

### 4.2 Composite chart

A single overlay chart with all four workloads on one plot (normalized to
sequential = 1.0x) for quick visual comparison of scaling shapes:

- **X-axis:** Worker count (log2 scale)
- **Y-axis:** Speedup vs sequential (linear, starting at 1.0)
- **Lines:** One per workload, distinct colors/markers
- **Reference:** Dashed y=x line (ideal linear scaling)

### 4.3 Rendering

Charts are generated as SVG via the `plotters` crate criterion custom
output, stored at:

```
target/pip-9g-results/charts/
  scaling_single_large_file.svg
  scaling_many_small_files.svg
  scaling_mixed_directory.svg
  scaling_delta_heavy.svg
  scaling_composite_normalized.svg
```

The outcome document embeds these as relative image links.

---

## 5. Statistical reporting format

### 5.1 Per-cell statistics

Each (workload, worker_count) cell reports:

```markdown
| Statistic                 | Value   |
|---------------------------|---------|
| Iterations                | 100     |
| Median                    | ____ ns |
| Mean                      | ____ ns |
| Std dev                   | ____ ns |
| IQR (p25 - p75)          | ____ - ____ ns |
| 95% CI (lower - upper)   | ____ - ____ ns |
| Min                       | ____ ns |
| Max                       | ____ ns |
| Outliers (mild / severe)  | _ / _   |
| CV (coefficient of var)   | _.__%   |
```

### 5.2 Outlier classification

Following criterion's convention:

- **Mild outlier:** Beyond 1.5x IQR from Q1/Q3
- **Severe outlier:** Beyond 3.0x IQR from Q1/Q3

Cells with > 5% severe outliers are flagged for investigation (likely
thermal throttling or scheduler interference).

### 5.3 Confidence interval methodology

- 95% bootstrap CI with 10,000 resamples (criterion default)
- Report as `[lower, upper]` in both absolute (ns) and relative (%)
  terms
- Two results are considered statistically different only when their 95%
  CIs do not overlap

### 5.4 Throughput derivation

```
throughput_MB_s = total_bytes / (median_ns / 1_000_000_000) / (1024 * 1024)
```

Reported with integer precision (measurement noise exceeds sub-MB/s
resolution).

---

## 6. Regression detection criteria

### 6.1 Sequential vs P(1) - overhead threshold

| Severity | Condition                        | Action                   |
|----------|----------------------------------|--------------------------|
| OK       | Overhead < 5%                   | Acceptable; no action    |
| Warning  | 5% <= overhead < 10%            | Investigate; may raise dispatch threshold |
| Failure  | Overhead >= 10%                 | Block default-on; fix scheduling overhead |

### 6.2 Scaling regression

A scaling regression is detected when P(N) throughput is lower than P(N/2)
for any N > 2 (negative scaling). Conditions:

| Severity | Condition                          | Action                       |
|----------|------------------------------------|------------------------------|
| OK       | P(N) >= P(N/2) for all N          | Expected behavior            |
| Warning  | P(N) < P(N/2) by < 5%            | Investigate contention source|
| Failure  | P(N) < P(N/2) by >= 5%           | Cap worker count at N/2      |

### 6.3 Cross-run stability

When comparing against a previous baseline (e.g., from a code change):

| Severity | Condition                                  | Action               |
|----------|--------------------------------------------|----------------------|
| OK       | Within +/-5% of previous median            | No regression        |
| Warning  | 5-10% slower than previous median          | Investigate          |
| Failure  | > 10% slower than previous median          | Bisect the regression|

### 6.4 Thermal and noise rejection

Results are invalid if any of:

- CV > 15% on any cell (too noisy for conclusions)
- Mean deviates from median by > 20% (heavy tail, likely interference)
- Severe outlier count > 10% of iterations

Remedy: re-run with longer warmup, stricter CPU isolation, or after
thermal equilibrium.

---

## 7. Expected speedup ranges per workload type

These ranges encode the predictions from PIP-9.g.a section 8.1. Bench
results falling outside these ranges trigger investigation.

### 7.1 Prediction table

| Workload          | Workers | Expected range | Below-range signal  | Above-range signal   |
|-------------------|---------|----------------|---------------------|----------------------|
| Single large file | 4       | 1.00-1.15x    | Bug (slower than 1x)| Unexpected unlock    |
| Many small files  | 4       | 2.5-3.5x      | Contention issue    | Verify cheaper than modeled |
| Mixed directory   | 4       | 2.0-3.0x      | Large files dominate| Small files dominate |
| Delta-heavy       | 4       | 2.5-3.5x      | Write serialization | Verify very parallel |
| Single large file | 8       | 1.00-1.15x    | Bug                 | Lock-free write path?|
| Many small files  | 8       | 3.5-5.0x      | DashMap sharding    | Unexpected            |
| Mixed directory   | 8       | 3.0-4.5x      | Mutex ceiling       | Unexpected            |
| Delta-heavy       | 8       | 3.5-5.0x      | Bandwidth limit     | Very light writes    |

### 7.2 Interpretation guide

- **Below range by > 20%:** Implementation bug or environmental issue.
  Do not use for tuning decisions until root-caused.
- **Below range by 5-20%:** Hardware or workload differs from model
  assumptions. Acceptable for tuning if consistent across re-runs.
- **Within range:** Model predictions confirmed. Proceed with tuning.
- **Above range:** Bonus. Investigate why (may inform further
  optimizations) but proceed with tuning using actual numbers.

---

## 8. Decision mapping to PIP-9.h.c tuning defaults

The bench outcome feeds directly into PIP-9.h.c parameter selection.
This section specifies the mapping.

### 8.1 Worker count default

The default worker count is chosen as the lowest N where:

1. P(N) achieves >= 80% of P(max) throughput on the mixed workload
2. P(N) does not regress vs sequential on single-large-file

Rationale: Mixed is the production-representative workload. The 80% knee
avoids diminishing returns from over-subscribing threads.

```
default_workers = min { N : throughput_mixed(N) >= 0.8 * max(throughput_mixed) }
```

### 8.2 Dispatch threshold

The dispatch threshold (minimum transfer size before engaging parallel
path) is set to the crossover point where parallel overhead equals
parallel benefit:

```
threshold_bytes = max file size where P(default_workers) < sequential * 1.05
```

If no such crossover exists (parallel always wins), threshold = 0.
If parallel never wins, threshold = infinity (disable parallel path).

### 8.3 Batch size inference

Not directly measured by PIP-9.g.a (covered by PIP-9.h.c grid search),
but the verify/write time split informs batch size selection:

- If `verify_ratio > 70%`: Larger batches amortize dispatch overhead.
  Suggest batch_size >= 16.
- If `verify_ratio < 30%`: Write-bound; batching does not help. Suggest
  batch_size = 1 (current default).
- If `30% <= verify_ratio <= 70%`: Moderate batching. Suggest
  batch_size = 4-8.

### 8.4 Saturation cap

If the scaling curve plateaus at N workers (defined as < 5% gain from
N to 2N), the worker pool is capped at N regardless of available cores:

```
saturation_cap = min { N : throughput(2N) / throughput(N) < 1.05 }
worker_cap = min(saturation_cap, num_cpus::get())
```

---

## 9. Verify/write time decomposition

A dedicated section in the outcome report breaks down where time is
spent per workload.

### 9.1 Format

```markdown
## Time decomposition (P(4), median)

| Workload          | Verify % | Write % | Overhead % | Notes            |
|-------------------|----------|---------|------------|------------------|
| Single large file | __       | __      | __         | Write-bound      |
| Many small files  | __       | __      | __         | Verify-bound     |
| Mixed directory   | __       | __      | __         | Balanced         |
| Delta-heavy       | __       | __      | __         | Verify-dominant  |
```

Where:
- `Verify %` = `(T1 - T0) / (T2 - T0) * 100`
- `Write %` = `(T2 - T1) / (T2 - T0) * 100`
- `Overhead %` = `100 - Verify% - Write%` (scheduling, DashMap, reorder)

### 9.2 Per-worker distribution

For the mixed workload at P(4) and P(8), report per-worker chunk counts
to assess load balance:

```markdown
### Worker load balance (Mixed, P(4))

| Worker | Chunks verified | % of total |
|--------|-----------------|------------|
| 0      | ____            | __         |
| 1      | ____            | __         |
| 2      | ____            | __         |
| 3      | ____            | __         |
```

A Gini coefficient > 0.3 indicates poor work distribution (possibly due
to file-size skew in the chunk assignment).

---

## 10. Sidecar metrics summary

The per-run JSON sidecar files (PIP-9.g.a section 4.1) are aggregated
into a summary table:

```markdown
## Sidecar metrics (median across iterations)

| Workload          | Workers | CPU util % | DashMap contention | Verify p99 (ns) |
|-------------------|---------|-----------|-------------------|------------------|
| Single large file | 4       | __        | __                | ____             |
| Many small files  | 4       | __        | __                | ____             |
| Mixed directory   | 4       | __        | __                | ____             |
| Delta-heavy       | 4       | __        | __                | ____             |
```

High DashMap contention (> 100 events per iteration) at a given worker
count suggests the parallel path is over-subscribed relative to the file
count.

---

## 11. Appendix: sample outcome (illustrative)

This section shows what a filled-in report looks like. Values are
hypothetical and exist solely to demonstrate formatting.

```markdown
## Results summary

### Throughput (MB/s) - median of 100 iterations

| Workload          | Sequential | P(1) | P(2) | P(4)  | P(8)  | P(16) |
|-------------------|-----------|-------|-------|-------|-------|--------|
| Single large file | 2840      | 2780  | 2890  | 2910  | 2920  | 2900   |
| Many small files  | 410       | 395   | 720   | 1280  | 1850  | 2050   |
| Mixed directory   | 680       | 660   | 1100  | 1840  | 2380  | 2580   |
| Delta-heavy       | 520       | 505   | 890   | 1620  | 2210  | 2450   |

### Speedup vs sequential baseline

| Workload          | P(1) | P(2) | P(4) | P(8) | P(16) |
|-------------------|------|------|------|------|--------|
| Single large file | 0.98 | 1.02 | 1.02 | 1.03 | 1.02   |
| Many small files  | 0.96 | 1.76 | 3.12 | 4.51 | 5.00   |
| Mixed directory   | 0.97 | 1.62 | 2.71 | 3.50 | 3.79   |
| Delta-heavy       | 0.97 | 1.71 | 3.12 | 4.25 | 4.71   |
```

In this hypothetical:
- **Worker count default:** 4 (achieves 71% of max on mixed; 6 would
  reach 80% - actual selection uses real numbers)
- **Dispatch threshold:** 0 (parallel never regresses vs sequential on
  any workload when workers >= 2)
- **Saturation cap:** 8 (P(16)/P(8) = 1.08 on mixed, just above 5%)

---

## 12. References

- `docs/design/parallel-receive-delta-bench.md` - PIP-9.g.a bench
  harness design
- `docs/design/parallel-receive-delta-bench-tuning.md` - PIP-9.h.c
  bench-driven tuning defaults
- `docs/design/parallel-receive-delta-tuning.md` - PIP-9.h.a tuning
  knobs specification
- `docs/design/pip-9hb-worker-pool-knobs-impl.md` - PIP-9.h.b
  WorkerPoolConfig implementation
- `crates/engine/benches/pip9g_parallel_vs_sequential.rs` - bench
  harness source
