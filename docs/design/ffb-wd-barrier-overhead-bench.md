# flush_workers barrier overhead benchmark (FFB-W.d)

Status: Design (task FFB-W.d; depends on FFB-1..4 flush_workers/drain_inflight
barrier scaffold in `ParallelDeltaApplier`)
Audience: engine maintainers evaluating barrier cost vs transfer throughput.
Scope: a criterion bench harness isolating Condvar signal/wait overhead at
varied worker and file counts, with a sequential baseline for comparison.

Out of scope: I/O throughput, delta-apply correctness, wire-byte parity. This
bench measures only the synchronisation primitive cost.

## 1. Motivation

FFB-1..4 scaffolded the `flush_workers`/`drain_inflight` barrier API for
`ParallelDeltaApplier`. Each per-file boundary executes a Condvar wait cycle:
the caller parks until every outstanding `SlotHandle` drops its
`DecrementGuard`, which decrements an in-flight counter and fires
`notify_all`. The `finish_file` path then spins on `Arc::try_unwrap` to
reclaim the writer.

For transfers with many small files, the barrier fires once per file. If
barrier latency is comparable to per-file transfer time, the synchronisation
overhead dominates and the parallel pipeline collapses to sequential
throughput. FFB-W.d quantifies this cost so the team can decide whether:

- The current per-file barrier is acceptable at production worker counts.
- A batch-drain variant (flush every N files) is needed to amortise cost.
- The `Arc::try_unwrap` spin-then-yield window (lines 84-103 of `drain.rs`)
  contributes measurable overhead beyond the Condvar path itself.

## 2. Bench target

Pure barrier overhead - Condvar signal/wait cycle plus Arc bookkeeping -
isolated from all I/O. No disk reads, no delta apply, no file creation. The
measured path is:

1. `BarrierState::increment_inflight` (N times, one per simulated worker)
2. Worker threads call `BarrierState::decrement_inflight` (fires `notify_all`)
3. Caller thread blocks on `BarrierState::wait_until_idle`
4. Caller observes idle, proceeds to next file

This matches the hot path in `flush_workers` (drain.rs:146-158) without the
DashMap lookup or `finish_file` Arc unwrap overhead. A separate sub-bench
includes the full `finish_file` path (DashMap remove + Arc spin + unwrap) to
measure the additional cost of the reclaim step.

## 3. Synthetic workload

### 3.1 No-op chunk model

Each simulated file dispatches W chunks to W worker threads. Each worker
performs no payload work - just holds a `DecrementGuard` (or calls
`decrement_inflight` directly), then drops it. The caller waits for all W
workers to complete via the barrier, then moves to the next file.

```rust
fn barrier_cycle(barrier: &Arc<BarrierState>, worker_count: usize) {
    // Simulate dispatch: bump inflight W times
    for _ in 0..worker_count {
        barrier.increment_inflight();
    }
    // Simulate workers completing: decrement from W threads
    let handles: Vec<_> = (0..worker_count)
        .map(|_| {
            let b = Arc::clone(barrier);
            std::thread::spawn(move || {
                b.decrement_inflight();
            })
        })
        .collect();
    // Caller waits for idle
    barrier
        .wait_until_idle(FileNdx::new(0), "bench")
        .expect("barrier wait failed");
    for h in handles {
        h.join().expect("worker panicked");
    }
}
```

### 3.2 Rayon variant

Production code uses rayon, not raw threads. A second sub-bench replaces
`std::thread::spawn` with `rayon::scope` to measure the barrier under rayon's
work-stealing scheduler, which may exhibit different wake latency:

```rust
fn barrier_cycle_rayon(barrier: &Arc<BarrierState>, pool: &rayon::ThreadPool) {
    let worker_count = pool.current_num_threads();
    for _ in 0..worker_count {
        barrier.increment_inflight();
    }
    pool.scope(|s| {
        for _ in 0..worker_count {
            let b = Arc::clone(barrier);
            s.spawn(move |_| {
                b.decrement_inflight();
            });
        }
    });
    barrier
        .wait_until_idle(FileNdx::new(0), "bench")
        .expect("barrier wait failed");
}
```

### 3.3 Full finish_file variant

A third sub-bench exercises the complete `finish_file` path including
DashMap lookup, barrier wait, DashMap remove, barrier Arc drop, spin-yield
loop, and `Arc::try_unwrap`. Uses a `ParallelDeltaApplier` instance with
`Vec<u8>` writers:

```rust
fn finish_file_cycle(applier: &ParallelDeltaApplier, ndx: FileNdx) {
    // register_file + dispatch workers + finish_file
    // (setup creates the applier and registers the file outside timed section)
    let _writer = applier.finish_file(ndx).expect("finish_file failed");
}
```

## 4. Parameter sweeps

### 4.1 Worker count sweep

| Workers | Rationale |
|---------|-----------|
| 4 | Minimum parallel configuration; typical CI runner core count |
| 8 | Common developer workstation |
| 16 | Server-class; stress-tests Condvar wake fan-out |
| 32 | High-end; exposes contention scaling on the inflight Mutex |

Each worker count uses a dedicated `rayon::ThreadPool` for the rayon variant
and a pre-spawned thread set for the raw-thread variant.

### 4.2 File count sweep

| Files | Rationale |
|-------|-----------|
| 100 | Baseline; barrier overhead should be negligible |
| 1,000 | Moderate; common delta transfer size |
| 10,000 | Stress; many-small-file transfers where overhead matters most |

Each file executes one barrier cycle (W increments, W decrements, 1 wait).
Total barrier invocations = file_count. Criterion measures wall-clock for the
entire file sweep, then reports per-file cost as `total / file_count`.

### 4.3 Combined matrix

The full benchmark matrix is 4 worker counts x 3 file counts x 3 variants
(raw-thread, rayon, finish_file) = 36 parameter points. Each is a separate
criterion `BenchmarkId`.

## 5. Comparison baseline: sequential path

The sequential baseline replaces the barrier with a direct function call on
the same thread - no Condvar, no Mutex, no Arc bookkeeping. Each "file"
executes a no-op closure W times sequentially:

```rust
fn sequential_cycle(worker_count: usize) {
    for _ in 0..worker_count {
        black_box(());  // simulate per-chunk work slot
    }
}
```

This measures the irreducible per-file overhead (loop + function call) so the
barrier cost can be expressed as a multiplier over the sequential baseline.
The sequential path appears as a fourth variant in the benchmark group.

## 6. Metrics

### 6.1 Primary metrics

| Metric | Source | Unit |
|--------|--------|------|
| Wall-clock per file | criterion total / file_count | ns/file |
| Barrier latency | Instant::now() around wait_until_idle | ns/call |
| Barrier overhead ratio | (parallel - sequential) / sequential | percentage |

### 6.2 Barrier latency instrumentation

Inside the timed section, a lightweight `Instant::now()` pair brackets the
`wait_until_idle` call to capture raw barrier latency (time between the
caller entering the wait and the caller waking). This captures the
last-worker-done-to-caller-wakes interval, which is the fundamental cost of
the Condvar path.

```rust
let t0 = Instant::now();
barrier.wait_until_idle(ndx, "bench").unwrap();
let barrier_ns = t0.elapsed().as_nanos();
barrier_latency_sum += barrier_ns;
```

The accumulated `barrier_latency_sum` is emitted as a custom measurement
via criterion's `black_box` (to prevent elision) and reported in the
summary script alongside the wall-clock numbers.

### 6.3 Overhead as percentage of transfer time

The bench does not perform I/O, so "transfer time" is estimated from
production baselines. The summary script computes:

```
overhead_pct = (barrier_ns_per_file / estimated_transfer_ns_per_file) * 100
```

Where `estimated_transfer_ns_per_file` is parameterised at 100 us (small
file with delta apply + temp-file rename on SSD) and 1 ms (medium file with
checksumming). Both values are documented in the summary output so reviewers
can substitute their own measurements.

## 7. Pass/fail criteria

| Metric | Target | Rationale |
|--------|--------|-----------|
| Barrier overhead < 1% of transfer time | At 8+ workers, 10K files, using 100 us/file estimate | The barrier must not dominate small-file transfers |
| Barrier latency < 10 us per file | At 8 workers, raw-thread variant | Condvar wake + mutex acquire should be sub-10 us on modern kernels |
| Linear scaling with file count | Barrier cost per file constant (+/- 20%) across 100/1K/10K | Confirms O(1) per-file barrier cost, not O(files) |
| Rayon variant within 2x of raw-thread | Same worker count, same file count | Rayon's work-stealing should not double the wake path |
| finish_file within 5x of raw barrier | Same worker count, same file count | DashMap + Arc spin overhead is bounded |

If the 1% target fails at 8 workers on 10K files, the batch-drain alternative
(section 9) becomes the recommended path.

## 8. Implementation structure

### 8.1 File location

`crates/engine/benches/barrier_overhead.rs`

### 8.2 Cargo.toml addition

```toml
[[bench]]
name = "barrier_overhead"
harness = false
```

No feature gate - the barrier types are always compiled (they are part of the
`concurrent_delta` module). The bench imports `BarrierState` and
`ParallelDeltaApplier` directly.

### 8.3 Module layout

```rust
// crates/engine/benches/barrier_overhead.rs

use std::sync::Arc;
use std::time::Instant;

use criterion::{
    BenchmarkId, Criterion, Throughput,
    criterion_group, criterion_main,
};
use std::hint::black_box;

const WORKER_COUNTS: [usize; 4] = [4, 8, 16, 32];
const FILE_COUNTS: [usize; 3] = [100, 1_000, 10_000];

fn bench_barrier_raw_thread(c: &mut Criterion) { /* ... */ }
fn bench_barrier_rayon(c: &mut Criterion) { /* ... */ }
fn bench_finish_file(c: &mut Criterion) { /* ... */ }
fn bench_sequential_baseline(c: &mut Criterion) { /* ... */ }

criterion_group!(
    name = barrier_overhead;
    config = Criterion::default()
        .sample_size(50)
        .measurement_time(std::time::Duration::from_secs(10))
        .warm_up_time(std::time::Duration::from_secs(3))
        .noise_threshold(0.05);
    targets =
        bench_barrier_raw_thread,
        bench_barrier_rayon,
        bench_finish_file,
        bench_sequential_baseline,
);
criterion_main!(barrier_overhead);
```

### 8.4 Criterion configuration rationale

- `sample_size = 50`: barrier cycles are cheap (microseconds), so 50 samples
  yields tight confidence intervals without excessive runtime.
- `measurement_time = 10s`: sufficient for criterion to auto-calibrate
  iteration count at sub-microsecond per-file cost.
- `noise_threshold = 0.05`: Condvar latency varies with OS scheduler load;
  5% noise floor prevents false regression signals.

### 8.5 Iteration structure

Each benchmark uses `iter_batched` with `BatchSize::SmallInput`:

1. **Setup** (untimed): construct `Arc<BarrierState>` (or `ParallelDeltaApplier`
   for the finish_file variant), pre-spawn worker threads or build rayon pool.
2. **Measured**: loop over `file_count` files, each executing one barrier cycle.
3. **Teardown** (untimed): join threads, drop applier.

The rayon pool is built once per parameter point (outside the benchmark
closure) and reused across iterations via `pool.install(|| ...)`.

## 9. Batch-drain alternative

If the per-file barrier exceeds the 1% overhead target, a batch-drain
variant amortises the Condvar cost across N files.

### 9.1 Design

Instead of flushing after every file, the caller accumulates completed file
indices and flushes in batches of N:

```rust
fn batch_flush(
    applier: &ParallelDeltaApplier,
    completed: &mut Vec<FileNdx>,
    batch_size: usize,
) {
    if completed.len() >= batch_size {
        for &ndx in completed.iter() {
            applier.flush_workers(ndx).unwrap();
        }
        completed.clear();
    }
}
```

### 9.2 Batch size sweep

| Batch size | Expected overhead reduction |
|------------|----------------------------|
| 1 (baseline) | 0% - same as per-file flush |
| 10 | ~10x fewer Condvar waits |
| 100 | ~100x fewer Condvar waits |
| 1000 | ~1000x fewer Condvar waits; risks memory pressure from held writers |

### 9.3 Bench integration

The batch-drain variant is a fifth benchmark group in the same file. It
sweeps batch_size x worker_count at a fixed file_count of 10K:

```rust
const BATCH_SIZES: [usize; 4] = [1, 10, 100, 1_000];

fn bench_batch_drain(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_drain");
    for &batch in &BATCH_SIZES {
        for &workers in &WORKER_COUNTS {
            group.throughput(Throughput::Elements(10_000));
            group.bench_with_input(
                BenchmarkId::new(
                    "batch_drain",
                    format!("batch{batch}/{workers}w"),
                ),
                &(batch, workers),
                |b, &(batch, workers)| { /* ... */ },
            );
        }
    }
    group.finish();
}
```

### 9.4 Decision criteria

Adopt batch-drain if:
- Per-file barrier overhead exceeds 1% at 8 workers on 10K files, AND
- Batch size 10 reduces overhead below 0.5%, AND
- Memory pressure from holding 10 writers concurrently is acceptable
  (estimated at 10 x buffer_pool_size, typically 10 x 256 KB = 2.5 MB).

## 10. CI integration

### 10.1 Workflow file

`.github/workflows/bench-barrier-overhead.yml`

Follows the pattern from `bench-daemon-coldstart.yml` (DIS-8.a) and
`bench-delete-throughput.yml` (DEL-4.a):

- **Triggers**: `workflow_dispatch`, nightly cron (`42 7 * * *` - offset from
  existing bench cells), and `pull_request` on paths:
  - `crates/engine/src/concurrent_delta/parallel_apply/**`
  - `crates/engine/benches/barrier_overhead.rs`
  - `.github/workflows/bench-barrier-overhead.yml`
- **Runner**: `ubuntu-latest` (2-core).
- **Timeout**: 15 minutes job-level.
- **Status**: non-required (advisory). Does not block PR merge.
- **Concurrency**: `bench-barrier-overhead-${{ github.ref }}`,
  `cancel-in-progress: true`.

### 10.2 Artifact and summary

The workflow uploads the criterion HTML report as a build artifact
(`criterion-barrier-overhead/`) and emits a step summary table:

```markdown
| Variant | Workers | Files | ns/file | Barrier ns | Overhead % (100us) |
|---------|---------|-------|---------|------------|-------------------|
| raw-thread | 8 | 10K | ... | ... | ... |
| rayon | 8 | 10K | ... | ... | ... |
| finish_file | 8 | 10K | ... | ... | ... |
| sequential | 8 | 10K | ... | ... | (baseline) |
```

Extracted via `tools/ci/extract_barrier_bench_summary.sh` from criterion's
JSON output at `target/criterion/*/new/estimates.json`.

### 10.3 Regression detection

Criterion's built-in regression detection (`noise_threshold = 0.05`,
`significance_level = 0.01`) flags regressions. The CI step uses
`continue-on-error: true` so regressions surface in the summary without
blocking the PR.

## 11. Relationship to other tasks

| Task | Relationship |
|------|-------------|
| FFB-1 | Option D barrier baked into `finish_file` - the path this bench measures |
| FFB-2 | Condvar wait protocol in `wait_until_idle` - the primitive under test |
| FFB-3 | `DecrementGuard` RAII pairing - the decrement side of the barrier |
| FFB-4 | `SlotEntry` DashMap carrier - the DashMap overhead in finish_file variant |
| DG-3.c | `DecrementGuard` retype to `Arc<BarrierState>` - current barrier shape |
| SSC-1 | `registrations_done` atomic + yield-loop fix - prerequisite for concurrent dispatch correctness |
| PIP-3 | Pipelined verify-write - would benefit from batch-drain if per-file barrier is too expensive |

## 12. Upstream reference

The upstream C rsync does not use a barrier - it processes files sequentially
in the generator/receiver pipeline. The barrier is an artefact of the
parallel delta-apply architecture unique to oc-rsync. There is no upstream
code path to benchmark against; the sequential baseline (section 5) serves
as the comparison point.
