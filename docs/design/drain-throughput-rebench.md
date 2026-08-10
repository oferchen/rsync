# DPC-6: Drain Throughput Rebench Under Per-Worker Drain

Status: design. Defines the A/B bench protocol that compares
`drain_parallel` throughput between the default `Arc<Mutex<Vec>>`
path and the `per-worker-drain-channels` feature path (DPC-5) at
1, 4, 16, 64, and 256 worker scales.

## 1. Objective

Quantify the throughput delta between the two drain implementations
across the full worker-count sweep. Results feed the DPC-7 flip
decision: promote `per-worker-drain-channels` to default if the
improvement meets the criteria in section 9.

## 2. Relationship to DPC-2 Baseline

DPC-2 (`docs/design/dpc-2-drain-throughput-bench.md`) defined the
bench harness (`drain_parallel_throughput.rs`) and captured the
mutex baseline at T in {1, 4, 16, 64}. DPC-6 extends the sweep to
T = 256 and runs the same harness in two configurations:

| Run | Feature flag | Drain body |
|---|---|---|
| A (baseline) | default build | `Arc<Mutex<Vec>>` sharded collector |
| B (candidate) | `--features per-worker-drain-channels` | Per-worker `SegQueue` lanes |

Both runs use the same bench binary source, same parameter matrix,
same host, same rayon `ThreadPoolBuilder` configuration. Only the
internal `drain_parallel` dispatch differs (compile-time `cfg` gate
in `drain.rs`).

## 3. Worker Count Sweep

| T | Rationale |
|---|---|
| 1 | Serial baseline. No contention in either path. Measures per-item fixed cost overhead of the per-worker channel setup vs mutex allocation. |
| 4 | Typical workstation. Low contention regime. Neither path should dominate - validates no regression from per-worker bookkeeping. |
| 16 | High-core server. DPC-3 identified this as the transition point where steal-induced mutex contention becomes measurable. |
| 64 | Extreme concurrency. Exceeds typical rayon pool size. Stress-tests the hashed `ThreadId` fallback in the mutex path and the lane-count scaling in the per-worker path. |
| 256 | Hyper-scale. Forces the per-worker path to maintain 256 SegQueue lanes and merge them at barrier. Tests that the merge cost does not become the new bottleneck. |

T = 256 is new relative to DPC-2's sweep. The DPC-2 bench capped at
T = 64; DPC-6 adds T = 256 to confirm the per-worker path continues
to scale past the mutex contention cliff.

## 4. Workload Profiles

Three payload sizes exercise different pressure regimes on the drain
collector:

| Profile | Item size | Items per batch | Pressure characteristic |
|---|---|---|---|
| Small | 1 KB | 10,000 | High item count, low per-item cost. Maximizes lock acquire frequency in the mutex path. |
| Large | 1 MB | 100 | Low item count, high per-item allocation. Measures whether bulk allocation dominates drain overhead. |
| Mixed | 50% x 1 KB + 50% x 1 MB | 5,000 | Realistic production distribution. Interleaved small/large items prevent steady-state optimizations. |

### 4.1 Small Profile

Synthetic `DeltaResult` with a 1 KB payload buffer attached. The
buffer is pre-allocated outside the timed section; the drain closure
moves it into the result. This profile maximizes the ratio of drain
operations to computation time, surfacing contention effects directly.

```rust
fn make_small_work(ndx: u32) -> DeltaWork {
    DeltaWork::whole_file(ndx, PathBuf::from("/bench/drain"), 1024)
}
```

### 4.2 Large Profile

Synthetic `DeltaResult` with a 1 MB payload buffer. Fewer items
(N = 100) but each push/collect moves a 1 MB allocation through the
drain. Tests whether the per-worker path's single-CAS push amortizes
better than the mutex path when the protected critical section
includes a large `Vec::push` with potential reallocation.

```rust
fn make_large_work(ndx: u32) -> DeltaWork {
    DeltaWork::whole_file(ndx, PathBuf::from("/bench/drain"), 1_048_576)
}
```

### 4.3 Mixed Profile

Alternating small and large items in a pre-shuffled sequence.
Prevents the allocator from settling into a steady allocation pattern
and simulates realistic transfer workloads where small metadata files
interleave with large data files.

## 5. Metrics

### 5.1 Primary: Drain Throughput (items/sec)

Criterion `Throughput::Elements(N)` measures end-to-end items drained
per second. This is the headline metric for the A/B comparison.

### 5.2 Secondary: Per-Drain Latency (ns/iter)

Wall-clock time for a single `drain_parallel` invocation processing
the full batch. Captures both the parallel execution phase and the
sequential flatten/merge phase.

### 5.3 Tertiary: Mutex Contention Events (A-path only)

For the baseline (A) path, contention is inferred from scaling
efficiency rather than direct instrumentation:

```
scaling_efficiency(T) = throughput(T) / (T * throughput(1))
```

A scaling efficiency below 0.5 at T >= 16 confirms mutex contention
as the binding constraint. Values above 0.75 would weaken the case
for per-worker channels at that worker count.

### 5.4 Supplementary: Merge Phase Cost (B-path only)

The per-worker path has a sequential merge phase after the
`rayon::scope` barrier where the calling thread drains all T
SegQueue lanes into a flat `Vec<R>`. At T = 256 with N = 10,000
items, the merge iterates 256 queues. Measured as:

```
merge_fraction = (wall_clock_total - wall_clock_parallel) / wall_clock_total
```

If `merge_fraction` exceeds 0.3 at T = 256, the merge phase is
becoming a new bottleneck and DPC-7 should investigate parallel
merge or batched lane drainage.

## 6. Bench Protocol

### 6.1 Environment

- Host: Mac Studio M2 Ultra (24 physical cores, 192 GB RAM).
- Background load: minimal (no compilation, no containers).
- Rust toolchain: stable 1.88.0 (pinned in `rust-toolchain.toml`).
- Criterion sample size: 50 iterations per cell.
- Warm-up: 5 seconds per cell.

### 6.2 Run Sequence

```bash
# Run A: mutex baseline
cargo bench -p engine --bench drain_parallel_throughput \
    -- --save-baseline mutex-baseline

# Run B: per-worker drain
cargo bench -p engine --bench drain_parallel_throughput \
    --features per-worker-drain-channels \
    -- --save-baseline per-worker

# Compare
critcmp mutex-baseline per-worker
```

### 6.3 Stability Validation

Each configuration runs 3 consecutive times. A cell is stable if its
coefficient of variation across 3 runs is below 5%. Unstable cells
are rerun after a 60-second cooldown. If instability persists, the
cell is flagged and excluded from the flip decision.

## 7. Parameter Matrix

Full matrix: 5 worker counts x 3 workload profiles = 15 cells per
configuration. Total: 30 cells (15 baseline + 15 per-worker).

| T | Small (10K items) | Large (100 items) | Mixed (5K items) |
|---|---|---|---|
| 1 | A vs B | A vs B | A vs B |
| 4 | A vs B | A vs B | A vs B |
| 16 | A vs B | A vs B | A vs B |
| 64 | A vs B | A vs B | A vs B |
| 256 | A vs B | A vs B | A vs B |

## 8. Expected Outcomes

Based on DPC-1 contention analysis and DPC-2 baseline projections:

| T | Expected delta (B vs A) | Rationale |
|---|---|---|
| 1 | -5% to +5% (neutral) | No contention in either path. Per-worker setup cost roughly offsets single-CAS vs uncontended mutex. |
| 4 | 0% to +15% | Minimal steal-induced contention. Per-worker avoids the occasional cross-shard lock but benefit is small. |
| 16 | +20% to +60% | Steal rate rises; mutex contention becomes measurable. Per-worker eliminates the lock-acquire hot sequence entirely. |
| 64 | +80% to +200% | Contention cliff. Mutex path scaling efficiency drops below 0.3. Per-worker maintains near-linear scaling. |
| 256 | +150% to +400% | Mutex path throughput plateaus or regresses. Per-worker merge cost grows linearly in T but remains cheaper than per-item locking. |

For workload profiles:
- **Small items** show the largest delta because the high item count
  maximizes lock-acquire frequency in the mutex path.
- **Large items** show smaller deltas because per-item computation
  dominates and the drain overhead is a smaller fraction of total time.
- **Mixed items** falls between, tracking the small-item profile
  at approximately 60-70% of its delta magnitude.

## 9. Decision Criteria for DPC-7

DPC-7 flips `per-worker-drain-channels` to default ON if ALL of the
following hold:

### 9.1 Improvement Threshold

| Condition | Required |
|---|---|
| Throughput at T = 64, Small profile | >= 1.5x improvement (B >= 1.5 * A) |
| Throughput at T = 64, Mixed profile | >= 1.3x improvement |
| Throughput at T = 256, Small profile | >= 2.0x improvement |

### 9.2 No-Regression Guard

| Condition | Required |
|---|---|
| Throughput at T = 1, all profiles | No regression > 5% (B >= 0.95 * A) |
| Throughput at T = 4, all profiles | No regression > 5% (B >= 0.95 * A) |
| Throughput at T = 16, all profiles | No regression > 3% (B >= 0.97 * A) |

### 9.3 Stability Requirement

All 15 per-worker cells must achieve < 5% coefficient of variation
across 3 consecutive runs. Unstable cells cannot contribute to the
flip decision.

### 9.4 Merge Phase Bound

The merge phase fraction (section 5.4) must remain below 0.2 at
T = 256 for all profiles. If merge exceeds 0.2, DPC-7 investigates
parallel merge before flipping.

### 9.5 Decision Outcomes

| Result | Action |
|---|---|
| All criteria met | DPC-7 flips default to ON, removes the old mutex path behind `cfg(not(...))` for rollback |
| Improvement met but regression detected at T <= 4 | Investigate per-worker setup overhead; defer flip until regression is resolved |
| Improvement below threshold at T = 64 | Per-worker design does not deliver sufficient benefit; DPC series closes without flip |
| Merge phase exceeds 0.2 at T = 256 | Investigate parallel merge (DPC-7.a) before flip decision |

## 10. Bench Implementation Changes

The existing `drain_parallel_throughput.rs` (DPC-2) needs the
following extensions for DPC-6:

### 10.1 Add T = 256

```rust
const WORKER_COUNTS: [usize; 5] = [1, 4, 16, 64, 256];
```

### 10.2 Add Workload Profiles

Replace the single 4 KiB payload with three profile functions:

```rust
mod profiles {
    pub fn small_batch(count: usize) -> Vec<DeltaWork> {
        (0..count as u32)
            .map(|i| DeltaWork::whole_file(i, DEST.clone(), 1024))
            .collect()
    }

    pub fn large_batch(count: usize) -> Vec<DeltaWork> {
        (0..count as u32)
            .map(|i| DeltaWork::whole_file(i, DEST.clone(), 1_048_576))
            .collect()
    }

    pub fn mixed_batch(count: usize) -> Vec<DeltaWork> {
        (0..count as u32)
            .map(|i| {
                let size = if i % 2 == 0 { 1024 } else { 1_048_576 };
                DeltaWork::whole_file(i, DEST.clone(), size)
            })
            .collect()
    }
}
```

### 10.3 Criterion Group Structure

```rust
fn bench_drain_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("drain_parallel_throughput");
    group.sample_size(50);
    group.warm_up_time(Duration::from_secs(5));

    let profiles: &[(&str, fn(usize) -> Vec<DeltaWork>, usize)] = &[
        ("small", profiles::small_batch, 10_000),
        ("large", profiles::large_batch, 100),
        ("mixed", profiles::mixed_batch, 5_000),
    ];

    for &threads in &WORKER_COUNTS {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .expect("failed to build rayon thread pool");

        for &(profile_name, make_batch, count) in profiles {
            group.throughput(Throughput::Elements(count as u64));
            group.bench_with_input(
                BenchmarkId::new(
                    format!("{profile_name}"),
                    format!("{threads}t"),
                ),
                &count,
                |b, &count| { /* ... bench body from DPC-2 ... */ },
            );
        }
    }

    group.finish();
}
```

## 11. Analysis Script

A post-bench analysis script computes the derived metrics from
Criterion JSON output:

```bash
#!/usr/bin/env bash
# tools/bench/analyze_drain_rebench.sh
# Computes A/B deltas and scaling efficiency from Criterion baselines.

set -euo pipefail

BASELINE_DIR="target/criterion/drain_parallel_throughput"

for profile in small large mixed; do
    echo "=== Profile: $profile ==="
    for t in 1 4 16 64 256; do
        a_ns=$(jq '.mean.point_estimate' \
            "$BASELINE_DIR/${profile}/${t}t/mutex-baseline/estimates.json")
        b_ns=$(jq '.mean.point_estimate' \
            "$BASELINE_DIR/${profile}/${t}t/per-worker/estimates.json")
        delta=$(echo "scale=2; ($a_ns - $b_ns) / $a_ns * 100" | bc)
        echo "  T=$t: A=${a_ns}ns B=${b_ns}ns delta=${delta}%"
    done
done
```

The script outputs a table suitable for pasting into the DPC-7
decision document.

## 12. Timeline

| Step | Dependency | Estimate |
|---|---|---|
| Extend bench harness (T=256, profiles) | DPC-5 merged | 1 hour |
| Run A (mutex baseline, 3 repetitions) | Bench changes merged | 30 min |
| Run B (per-worker, 3 repetitions) | Same | 30 min |
| Analyze results, write DPC-7 decision | A + B complete | 1 hour |

Total wall-clock: approximately 3 hours from DPC-5 merge to DPC-7
decision document.

## 13. Cross-References

- DPC-1 (#2846) - Contention audit identifying `Mutex<Vec>` as the
  hottest sequence in `drain_parallel`.
- DPC-2 (#2847) - Baseline bench harness design
  (`docs/design/dpc-2-drain-throughput-bench.md`).
- DPC-3 (#2848) - Per-worker drain channels design
  (`docs/design/per-worker-drain-channels.md`). Section 6: flip
  criterion.
- DPC-4 (#2849) - Rollback runbook
  (`docs/operations/drain-restructure-rollback.md`).
- DPC-5 (#2850) - Implementation spec
  (`docs/design/dpc-5-per-worker-drain-impl.md`). Feature flag:
  `per-worker-drain-channels`.
- DPC-7 - Flip-vs-hold decision. Consumes this document's results.
- `crates/engine/benches/drain_parallel_throughput.rs` - Bench binary.
- `crates/engine/src/concurrent_delta/work_queue/drain.rs` -
  Production drain body with `cfg` dispatch.
