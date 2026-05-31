# PIP-9.h: Worker-pool tuning close-out

Status: Complete
Date: 2026-06-01
Tracker: PIP-9.h (#2603)
Sub-tasks:
- PIP-9.h.a (completed) - tuning spec (`docs/design/parallel-receive-delta-tuning.md`)
- PIP-9.h.b (completed) - knob implementation (`docs/design/pip-9hb-worker-pool-knobs-impl.md`)
- PIP-9.h.c (completed) - bench-driven defaults (`docs/design/parallel-receive-delta-bench-tuning.md`)

This document records the final tuning decisions for the parallel-receive-delta
worker pool, the bench evidence that drove them, and guidance for operators who
need to override the defaults.

## 1. Final defaults

The following compile-time defaults ship in production builds. They are the
outcome of the PIP-9.h.c grid search across 4-core, 8-core, and 16-core
simulated hardware tiers against four workload profiles (single-large,
many-small, mixed, delta-heavy).

| Parameter | Default | Constant / function | Location |
|-----------|---------|---------------------|----------|
| Worker count | `min(available_parallelism / 2, 8)` | `default_workers()` formula | `WorkerPoolConfig` resolution in `ThresholdDeltaPipeline::promote_to_parallel` |
| Batch size | `workers * 4`, clamped to `[8, 64]` | `default_batch_size(workers)` | Same resolution path |
| Queue depth | Adaptive: `workers * multiplier` where multiplier is 8x (small files), 4x (medium), 2x (large) | `adaptive_queue_depth(avg_file_size)` | `crates/engine/src/concurrent_delta/work_queue/capacity.rs` |
| Threshold bytes | 16 MiB | `DEFAULT_PARALLEL_RECEIVE_THRESHOLD_BYTES` | `crates/transfer/src/delta_pipeline/threshold.rs` |
| File-count threshold | 64 files | `DEFAULT_PARALLEL_THRESHOLD` | `crates/transfer/src/delta_pipeline/mod.rs` |
| Per-file reorder capacity | 64 chunks | `DEFAULT_PER_FILE_REORDER_CAPACITY` | `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` |

### 1.1 Worker count formula

```rust
fn default_workers() -> usize {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    cpus.div_ceil(2).min(8)
}
```

Option B (core-count formula) was selected because the bench showed optimal
worker counts differing by more than 2x across hardware tiers: 2 workers
optimal on 4-core, 4 on 8-core, 6-8 on 16-core. A fixed constant would
either waste resources on high-core machines or regress on constrained ones.

The cap at 8 workers reflects the diminishing-returns threshold: beyond 8
workers, the per-file write mutex serialization dominates and additional
workers add scheduling overhead without throughput gain.

### 1.2 Batch size formula

```rust
fn default_batch_size(workers: usize) -> usize {
    (workers * 4).max(8).min(64)
}
```

Batch size 16 was the sweet spot on the 8-core reference tier (4 workers),
amortizing dispatch overhead while keeping per-file completion latency bounded.
The formula scales linearly with worker count so each worker processes 4 chunks
per dispatch. The floor of 8 avoids degenerate 1-chunk batches on 2-worker
configurations; the ceiling of 64 prevents latency tail growth.

### 1.3 Queue depth (adaptive)

Queue depth remains a derived heuristic rather than a fixed constant. The
PIP-9.h.c sensitivity analysis showed queue depth co-varies with worker count
(< 15% independent variance contribution). The three-tier file-size multiplier
- 8x for small files (< 64 KiB), 4x for medium (64 KiB - 1 MiB), 2x for
large (> 1 MiB) - prevents worker starvation on syscall-bound small-file
transfers without wasting memory on I/O-bound large-file transfers.

### 1.4 Threshold bytes

16 MiB was the smallest threshold at which parallel mode consistently pays for
itself. Below 16 MiB, the dispatch, thread-spawn, and bounded-channel
allocation costs exceed the throughput gain from verify parallelism. The gate
fires as a logical AND with the file-count threshold: parallel mode activates
only when both the file count exceeds 64 and the aggregate byte volume exceeds
16 MiB. This keeps small config-file syncs (`rsync ~/.config/`) on the fast
sequential path.

### 1.5 Per-file reorder capacity

The deferred fifth knob (PIP-9.h.a section 3.5) was not promoted. Bench
telemetry showed the reorder buffer high-watermark never exceeded 40 chunks
across any workload profile at the default batch sizes, well below the 64-chunk
cap. The default of 64 provides sufficient headroom without tuning.

## 2. Bench evidence summary

### 2.1 Throughput results (8-core reference tier, P90 across workloads)

| Configuration | Throughput (MB/s) | vs sequential |
|---------------|-------------------|---------------|
| Sequential baseline (w=1, b=1) | 1,420 | 1.00x |
| Default formula (w=4, b=16, t=16M) | 3,210 | 2.26x |
| Tier-optimal (w=4, b=16, t=16M) | 3,210 | 2.26x |

The default configuration coincides with the tier-optimal on 8-core, confirming
the formula selection.

### 2.2 4-core regression check

| Workload | Default throughput / Sequential throughput |
|----------|-------------------------------------------|
| Single large | 1.02x |
| Many small | 1.41x |
| Mixed | 1.38x |
| Delta-heavy | 1.65x |

No workload regresses below 0.95x on 4-core. The single-large workload
effectively breaks even (write-mutex-bound, as predicted), while workloads with
cross-file fan-out benefit meaningfully even at 2 workers.

### 2.3 16-core utilization

Default formula produces 4 workers on 8-core and 8 on 16-core. Throughput on
16-core at default (8 workers) reaches 92% of the tier-optimal configuration
(also 8 workers), satisfying the >= 80% criterion from PIP-9.h.c section 7.3.

### 2.4 Sensitivity ranking (ANOVA)

| Parameter | Variance explained |
|-----------|-------------------|
| Worker count | 62% |
| Batch size | 24% |
| Threshold bytes | 8% |
| Queue depth (vs worker count) | 6% |

Worker count is the dominant lever. Batch size contributes meaningfully
(> 15%), justifying its retention as an explicit knob. Queue depth and
threshold bytes remain derived/fixed respectively, per the PIP-9.h.a section 5
rollback criterion.

## 3. When to override defaults

### 3.1 Large-file workloads (media, VM images, database dumps)

The per-file write mutex serializes writes within each file. When transferring
a small number of very large files (< 10 files, each > 1 GiB), the parallel
path provides minimal benefit because cross-file parallelism has few files to
fan out across. The sequential path avoids dispatch overhead entirely.

Recommendation: set `OC_RSYNC_PARALLEL_RECEIVE_THRESHOLD_BYTES=0` (disable the
byte gate) if you know the transfer always exceeds 16 MiB, or set
`OC_RSYNC_PARALLEL_RECEIVE_WORKERS=1` to force sequential-equivalent behavior
when the overhead is unwanted.

### 3.2 Many-small-file workloads (config sync, dotfiles)

Transfers of thousands of files under 4 KiB each are dispatch-overhead-bound.
The 16 MiB byte threshold may prevent parallel activation on transfers that
would actually benefit (e.g., 10,000 files x 2 KiB = 20 MiB, barely above
threshold).

Recommendation: the defaults handle this case correctly (20 MiB > 16 MiB
triggers parallel). For transfers that are borderline, lowering the threshold
via `OC_RSYNC_PARALLEL_RECEIVE_THRESHOLD_BYTES=4194304` (4 MiB) allows earlier
parallel activation.

### 3.3 Constrained hardware (Raspberry Pi, CI micro-runners, 2-core VMs)

On 2-core machines, the formula produces 1 worker - effectively sequential with
the parallel dispatch overhead. This is intentional: the parallel path cannot
win when there is only one core available for verify.

Recommendation: no override needed. The formula gracefully degrades. If
dispatch overhead is measurable on extremely constrained hardware (< 2 cores),
disable parallel entirely by setting
`OC_RSYNC_PARALLEL_RECEIVE_WORKERS=1`.

### 3.4 High-core server workloads (32+ cores, build farms, mirror servers)

The 8-worker cap leaves cores unused on 32+ core machines. For workloads that
are strongly verify-bound (delta-heavy with many files), raising the cap may
yield additional throughput.

Recommendation: set `OC_RSYNC_PARALLEL_RECEIVE_WORKERS=12` or `=16` and
increase batch size proportionally via
`OC_RSYNC_PARALLEL_RECEIVE_BATCH_SIZE=48` or `=64`. Monitor queue backpressure
via debug logging to confirm the per-file write mutex is not the bottleneck.

### 3.5 Memory-constrained environments

Each worker holds one in-flight `DeltaChunk` (average 64 KiB). Peak parallel
memory overhead: `workers * avg_chunk_size + queue_depth * sizeof(DeltaWork) +
active_files * reorder_capacity * avg_chunk_size`. At defaults (4 workers, 16
queue depth, ~8 active files, 64-chunk reorder): approximately 32-40 MiB peak.

Recommendation: reduce workers to 2 via `OC_RSYNC_PARALLEL_RECEIVE_WORKERS=2`
and reduce queue depth via `OC_RSYNC_PARALLEL_RECEIVE_QUEUE_DEPTH=8`.

## 4. CLI flags and environment variables

All knobs are accessible through both CLI flags and environment variables. CLI
flags take precedence over env vars; env vars take precedence over the compiled
defaults.

| CLI flag | Environment variable | Type | Valid range |
|----------|---------------------|------|-------------|
| `--parallel-receive-workers` | `OC_RSYNC_PARALLEL_RECEIVE_WORKERS` | `usize` | 1 to ambient rayon pool size |
| `--parallel-receive-batch-size` | `OC_RSYNC_PARALLEL_RECEIVE_BATCH_SIZE` | `usize` | 1 to queue depth |
| `--parallel-receive-queue-depth` | `OC_RSYNC_PARALLEL_RECEIVE_QUEUE_DEPTH` | `usize` | 1 to 65,536 |
| `--parallel-receive-threshold-bytes` | `OC_RSYNC_PARALLEL_RECEIVE_THRESHOLD_BYTES` | `u64` | 0 to u64::MAX |

### 4.1 Constraint matrix (validated at config-build time)

- `workers >= 1`
- `batch_size >= 1` and `batch_size <= queue_depth`
- `queue_depth >= 1` and `queue_depth <= 65,536`
- When `queue_depth` is explicitly set, the adaptive file-size heuristic is
  bypassed entirely.
- When `workers` is explicitly set, it is clamped to the ambient rayon pool
  size (no benefit in requesting more workers than rayon owns).

### 4.2 Precedence chain

1. CLI flag (highest)
2. Environment variable
3. Programmatic `WorkerPoolConfig` field (bench harnesses)
4. Compiled default / adaptive formula (lowest)

Invalid env-var values emit a `tracing::warn!` and leave the field at its
prior value - never a panic.

## 5. Architectural decisions

### 5.1 Global rayon pool (no dedicated pool)

The parallel-receive-delta path dispatches into rayon's global thread pool.
A dedicated `rayon::ThreadPool` was considered and rejected:

- The sender-side signature generation path also uses the global rayon pool.
  A separate pool would double thread count and compete for CPU cache residency.
- Bench evidence showed no measurable pool contention between sender and
  receiver paths at the default worker counts. The sender completes signature
  generation before the receiver begins delta application in most transfer
  profiles.
- If future bench evidence shows sender/receiver pool contention,
  `--parallel-receive-dedicated-pool` can be added without changing the
  `WorkerPoolConfig` interface.

### 5.2 Queue depth as derived heuristic

Queue depth is not an independently tuned default. The sensitivity analysis
showed it explains < 15% of throughput variance independently of worker count.
It remains a `workers * file_size_multiplier` derivation, preserving the
principle of fewer knobs for operators to misconfigure.

### 5.3 Rollback surface

Per PIP-9.h.a section 5, if future evidence shows only `workers` matters,
`batch_size` and `queue_depth` CLI flags can be deprecated and folded back
into internal derivations. The `WorkerPoolConfig` struct and env-var machinery
remain for bench reproducibility regardless.

## 6. Cross-references

- PIP-9.h.a tuning spec: `docs/design/parallel-receive-delta-tuning.md`
- PIP-9.h.b implementation spec: `docs/design/pip-9hb-worker-pool-knobs-impl.md`
- PIP-9.h.c bench design: `docs/design/parallel-receive-delta-bench-tuning.md`
- Umbrella parallel-receive design: `docs/design/parallel-receive-delta-application.md`
- PIP-9.g.a bench harness: `docs/design/parallel-receive-delta-bench.md`
- Default-on decision history: `docs/design/parallel-receive-delta-default-on.md`
- Worker pool config struct: `crates/engine/src/concurrent_delta/work_queue/capacity.rs`
- Parallel applier: `crates/engine/src/concurrent_delta/parallel_apply/mod.rs`
- Threshold pipeline: `crates/transfer/src/delta_pipeline/threshold.rs`
- Dispatch threshold constant: `crates/transfer/src/delta_pipeline/mod.rs`
