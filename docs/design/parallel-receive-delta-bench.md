# PIP-9.g.a - Parallel vs sequential receive-delta benchmark design

Status: Design
Date: 2026-06-01
Tracker: PIP-9.g.a
Predecessors:
- PIP-9 (merged) wired `ParallelDeltaApplier` as default-on via rayon workers
- PIP-6 (scaffold) end-to-end daemon-loopback bench comparing parallel vs sequential builds
- BR-3i.f (completed) apply-loop-level bench in isolation
Scope: microbench measuring per-chunk verify+write throughput through both
parallel and sequential receive-delta paths, with worker-count variation to
characterize the scaling curve.

## 1. Motivation

PIP-6 measures the end-to-end wall-clock impact of parallel receive-delta
on production-shaped daemon transfers. It answers "does the operator feel
a speedup?" but conflates protocol framing, file-list exchange, signature
generation, and disk fsync into the measurement. When PIP-6 shows a
smaller win than BR-3i.f predicts, the question becomes: where does the
apply-loop's parallelism hit diminishing returns?

PIP-9.g.a fills this gap by exercising the same delta-apply code path
(ParallelDeltaApplier) through both parallel and sequential modes under
controlled conditions - same chunks, same checksum strategy, same
destination writer - with the only variable being worker count. The bench
isolates the verify-vs-write time split, quantifies per-core scaling, and
identifies the crossover point where additional rayon workers stop helping.

This data feeds the tuning of `ParallelDeltaApplier`'s thread pool size
and the dispatch heuristic thresholds in
`crates/transfer/src/receiver/mod.rs`.

## 2. Bench architecture

The bench drives `ParallelDeltaApplier` directly (no protocol framing, no
daemon process, no network I/O) with pre-generated chunk vectors that
simulate four workload profiles. Each workload is exercised through:

1. **Parallel path** - `apply_batch_parallel` with configurable rayon
   `ThreadPoolBuilder::num_threads` (1, 2, 4, 8, 16).
2. **Sequential path** - the fallback code path used when the
   `parallel-receive-delta` feature is disabled: chunks processed
   in-order with verify and write on the same thread.

Both paths consume identical `DeltaChunk` vectors and write to the same
in-memory `VecSink` (or tmpfs-backed file for disk-realistic cells).
The bench uses criterion's `iter_custom` to capture wall-clock per batch
and derives throughput from total bytes processed.

### 2.1 Chunk generation

Pre-generated chunk vectors avoid measuring allocation during the timed
region. Each chunk carries:

- `chunk_sequence` - monotonic per file
- `ndx` - file index
- `data` - `Vec<u8>` payload (literal bytes or reconstructed block)
- `checksum` - pre-computed strong checksum matching the strategy

The checksum is computed eagerly so that `verify_chunk` performs a real
digest comparison (not a short-circuit). This ensures the bench measures
actual CPU work in the verify step.

### 2.2 Timing decomposition

Each iteration captures three timestamps:

1. **T0** - batch submission start
2. **T1** - all verify completions (rayon barrier return)
3. **T2** - all writes flushed

From these: `verify_time = T1 - T0`, `write_time = T2 - T1`,
`total = T2 - T0`. The ratio `verify_time / total` quantifies
how much of the pipeline is parallelizable vs serialized by the
per-file Mutex.

## 3. Workload profiles

Four profiles exercise distinct bottleneck regimes.

### 3.1 Single large file (1 GiB)

| Parameter     | Value              |
|---------------|--------------------|
| Files         | 1                  |
| Chunks/file   | 16,384             |
| Chunk size    | 64 KiB            |
| Total size    | 1 GiB              |
| Delta content | 50% literal, 50% copy tokens |

**Purpose:** Worst case for cross-file parallelism. Only one file slot
exists, so the per-file Mutex serializes all writes. Verify can still
run in parallel across chunks, but the write queue is single-lane.
Expected outcome: minimal speedup from additional workers beyond 2
(one verifying while one writes).

### 3.2 Many small files (100K x 4 KiB)

| Parameter     | Value              |
|---------------|--------------------|
| Files         | 100,000            |
| Chunks/file   | 1                  |
| Chunk size    | 4 KiB             |
| Total size    | 400 MiB            |
| Delta content | 100% literal (whole-file) |

**Purpose:** Maximum cross-file parallelism potential. Each file has
exactly one chunk, so the per-file Mutex never contends across files.
The overhead is DashMap lookup + slot registration per file. Expected
outcome: near-linear scaling up to core count, then plateau.

### 3.3 Mixed directory (1,000 files, variable sizes)

| Parameter     | Value              |
|---------------|--------------------|
| Files         | 1,000              |
| Chunks/file   | 1 - 256 (geometric distribution) |
| Chunk size    | 4 KiB - 64 KiB    |
| Total size    | ~600 MiB           |
| Delta content | 50% literal, 50% copy tokens |

**Purpose:** Production-representative shape. A mix of small config
files (1 chunk) and large media/binary files (256 chunks) exercises
the scheduler's ability to overlap verify work across heterogeneous
file sizes. Deterministic PRNG seed for reproducibility.

### 3.4 Delta-heavy (90% block matches)

| Parameter     | Value              |
|---------------|--------------------|
| Files         | 500                |
| Chunks/file   | 128                |
| Chunk size    | 64 KiB            |
| Total size    | 4 GiB              |
| Delta content | 90% copy tokens (basis offset + length), 10% literal |

**Purpose:** Exercises the verify step under realistic delta transfer
conditions. Copy tokens require checksum verification of the
reconstructed block (read from basis + apply literal patches). This
profile stresses the CPU-bound verify path where parallelism has the
highest potential payoff, since write volume is low (only 10% literal
bytes hit the destination).

## 4. Metrics

| Metric                    | Collection method                                    | Unit     |
|---------------------------|------------------------------------------------------|----------|
| Wall-clock per batch      | criterion `iter_custom`                              | ns       |
| Throughput                | `Throughput::Bytes(total_bytes)`                     | MB/s     |
| Verify time               | `Instant::now()` bracketing rayon barrier            | ns       |
| Write time                | `Instant::now()` from barrier return to flush        | ns       |
| Verify/total ratio        | derived                                              | %        |
| CPU utilization           | `getrusage` utime+stime / (wall * num_workers)      | %        |
| Per-worker verify count   | atomic counter per rayon worker (thread-local bump)  | count    |
| DashMap contention events | `DashMap::try_entry` miss counter                    | count    |

### 4.1 Sidecar output

Metrics beyond wall-clock and throughput are written to
`target/pip-9g-sidecar/{workload}/{workers}/metrics.json` per run.
Format:

```json
{
  "workload": "many_small_files",
  "workers": 4,
  "verify_ns_p50": 12340,
  "verify_ns_p99": 45600,
  "write_ns_p50": 8900,
  "write_ns_p99": 23400,
  "verify_ratio_pct": 58.2,
  "cpu_utilization_pct": 72.1,
  "dashmap_contention_events": 14
}
```

## 5. Worker count variation

The bench sweeps rayon thread pool size across `{1, 2, 4, 8, 16}` to
characterize the scaling curve. Each worker count runs as a separate
criterion benchmark group so that criterion's comparison output shows
the delta between adjacent thread counts.

### 5.1 Thread pool construction

```rust
let pool = rayon::ThreadPoolBuilder::new()
    .num_threads(worker_count)
    .thread_name(|i| format!("pip9g-worker-{i}"))
    .build()
    .unwrap();

pool.install(|| {
    // run apply_batch_parallel inside this pool
});
```

A fresh pool per worker-count group avoids warm-pool bias between
configurations. The pool is constructed outside the timed region.

### 5.2 Expected scaling curve

| Workers | Single large file | Many small files | Mixed | Delta-heavy |
|---------|-------------------|------------------|-------|-------------|
| 1       | baseline          | baseline         | baseline | baseline |
| 2       | ~1.1x (write-bound) | ~1.8x          | ~1.6x | ~1.7x      |
| 4       | ~1.1x            | ~3.2x            | ~2.8x | ~3.0x      |
| 8       | ~1.1x            | ~4.5x            | ~3.8x | ~4.2x      |
| 16      | ~1.1x            | ~5.0x (plateau)  | ~4.0x | ~4.5x      |

These are predictions based on the architecture: single-file is
Mutex-bound, multi-file scales until DashMap sharding and memory
bandwidth saturate. Actual numbers will differ; the purpose is to set
the expectation shape (sublinear, plateauing) rather than specific
targets.

## 6. Comparison methodology

### 6.1 Parallel vs sequential

The sequential baseline uses `worker_count = 1` with the parallel
code path disabled - chunks processed through a simple loop:

```rust
for chunk in batch {
    let digest = strategy.verify(&chunk.data);
    assert_eq!(digest, chunk.expected_checksum);
    writer.write_all(&chunk.data)?;
}
```

This mimics the code path active when `parallel-receive-delta` is
compiled out. The comparison is:

- Sequential (loop above) vs Parallel(N=1): measures scheduling
  overhead of rayon + DashMap + reorder buffer when parallelism is
  not actually exploited.
- Sequential vs Parallel(N=4): measures the real-world win on a
  typical 4-core developer machine.
- Sequential vs Parallel(N=8): measures the win on a typical CI
  runner or server.

### 6.2 Feature-flag toggle

Two bench binaries are built:

```sh
# Parallel build (default features)
cargo bench --bench pip9g_parallel_vs_sequential --features parallel-receive-delta

# Sequential build (feature disabled)
cargo bench --bench pip9g_parallel_vs_sequential --no-default-features \
  --features 'zstd lz4 xattr iconv'
```

Criterion's `--baseline` and `--save-baseline` options allow
cross-build comparison:

```sh
# Run sequential first, save baseline
cargo bench ... --no-default-features ... -- --save-baseline sequential

# Run parallel, compare against sequential baseline
cargo bench ... --features parallel-receive-delta -- --baseline sequential
```

## 7. Environment control

### 7.1 CPU pinning

On Linux, use `taskset` to pin the bench process and rayon workers to
a fixed set of cores, avoiding scheduler migration noise:

```sh
taskset -c 0-7 cargo bench --bench pip9g_parallel_vs_sequential
```

On macOS, use `cpuset` or accept that the scheduler provides
reasonable affinity on Apple Silicon (unified memory eliminates NUMA
effects).

### 7.2 Cache warming

Each criterion group runs 10 warmup iterations before measurement.
The chunk data vectors are pre-allocated and touched (read once) before
timing begins, ensuring they are in L3/LLC. The destination sinks are
pre-allocated to final size (no realloc during measurement).

### 7.3 Storage

The primary bench mode uses in-memory `VecSink` destinations to isolate
CPU scaling from disk I/O. A secondary "disk-realistic" mode writes to
tmpfs (Linux) or `/tmp` on ramdisk (macOS) to include kernel buffer
management overhead without introducing rotational/SSD latency variance.

For the full-stack disk cell (optional, not criterion-timed):
- NVMe: confirms that parallel verify overlaps with NVMe write
  latency (~10us per 4K write), showing CPU-parallel wins are not
  masked by fast storage.
- SSD: confirms the same property holds on typical developer machines.
- HDD: expected to show no parallel benefit (write latency dominates;
  the per-file Mutex queue never builds up).

### 7.4 Ambient load

The bench should run on an otherwise-idle machine. The criterion
`--measurement-time` is set to 10 seconds per group (sufficient for
stable p50 on modern hardware). A `--warm-up-time` of 3 seconds
per group ensures JIT effects (LLVM, branch predictor training) are
amortized before measurement begins.

## 8. Expected outcomes

### 8.1 Parallel wins

| Workload          | Expected speedup (4 workers) | Rationale                                    |
|-------------------|------------------------------|----------------------------------------------|
| Many small files  | 2.5-3.5x                    | Cross-file parallelism, no Mutex contention  |
| Mixed directory   | 2.0-3.0x                    | Heterogeneous files; large files limit gains |
| Delta-heavy       | 2.5-3.5x                    | CPU-bound verify dominates; highly parallel  |
| Single large file | 1.0-1.15x                   | Per-file Mutex serializes writes             |

### 8.2 Overhead characterization

- **Parallel(N=1) vs Sequential:** Expected 3-8% overhead from rayon
  dispatch, DashMap lookup, reorder buffer insert/drain. This is the
  cost of the parallel infrastructure when it cannot exploit
  parallelism. If overhead exceeds 10%, the dispatch heuristic's
  threshold needs raising.
- **Scheduling cost per chunk:** Derived from `(parallel_N1_time -
  sequential_time) / chunk_count`. Target: under 200ns per chunk.

### 8.3 Scaling saturation

The bench identifies the worker count where throughput plateaus for
each workload. Expected saturation points:

- Many small files: 6-8 workers (DashMap sharding becomes the limiter)
- Mixed: 4-6 workers (large files' Mutex contention limits scaling)
- Delta-heavy: 6-8 workers (verify is CPU-pure; scales until memory
  bandwidth saturates)
- Single large file: 2 workers (one verify, one write; more is waste)

### 8.4 Decision criteria

The bench validates PIP-9's default-on decision if:

1. **Win floor:** Parallel(N=4) achieves >= 2x throughput vs sequential
   on at least two of the four workload profiles.
2. **Regression ceiling:** Parallel(N=1) overhead vs sequential is
   under 10% on all profiles.
3. **Saturation visibility:** The scaling curve visibly plateaus,
   confirming that the default rayon pool size (num_cpus) is
   reasonable and not wasteful.

A failure on criterion 1 suggests the parallel path does not justify
its complexity for the workload shapes it handles. A failure on
criterion 2 suggests the dispatch heuristic's threshold is too
aggressive (steering small workloads into overhead they do not
recoup).

## 9. Bench file location

```
crates/engine/benches/pip9g_parallel_vs_sequential.rs
```

Follows the existing pattern of engine-crate criterion benches
(`parallel_receive_delta_perf.rs`, `buffer_pool_bench.rs`). The bench
is `#[ignore]`-gated for CI since it requires specific thread-count
pinning for reproducible results; the bench CI job runs it explicitly.

## 10. Out of scope

- **Protocol framing overhead** - covered by PIP-6 end-to-end bench.
- **Disk fsync latency** - the bench uses in-memory sinks; fsync
  overhead is a property of the storage subsystem, not the parallel
  apply path.
- **Sender-side parallelism** - PIP-9.g.a measures the receiver's
  apply step only.
- **Upstream rsync comparison** - this bench compares parallel vs
  sequential within oc-rsync; upstream comparison is a different
  question answered by `crates/core/benches/transfer_benchmark.rs`.
- **Numbers capture** - this doc ships the design; actual numbers
  land in a follow-up commit after bench execution on controlled
  hardware.

## 11. References

- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` -
  `ParallelDeltaApplier` implementation.
- `crates/engine/src/concurrent_delta/parallel_apply/batch.rs` -
  `apply_batch_parallel` rayon dispatch.
- `crates/engine/src/concurrent_delta/parallel_apply/drain.rs` -
  reorder buffer drain into per-file write.
- `crates/transfer/src/receiver/mod.rs` - dispatch heuristic
  thresholds (`PARALLEL_RECEIVE_FILE_COUNT_THRESHOLD`,
  `PARALLEL_RECEIVE_BYTES_THRESHOLD`).
- `docs/design/pip-6-end-to-end-parallel-vs-sequential-bench-2026-05-21.md` -
  PIP-6 end-to-end bench scaffold.
- `crates/engine/benches/parallel_receive_delta_perf.rs` - BR-3i.f
  apply-loop bench (in-memory, no protocol framing).
- `docs/design/parallel-receive-delta-application.md` - umbrella
  design for the parallel apply loop.
