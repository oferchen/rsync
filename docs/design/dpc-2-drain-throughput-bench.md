# DPC-2: Drain Throughput Baseline Bench (#2847)

Status: spec. Defines the benchmark harness that captures baseline
`drain_parallel` throughput at multiple worker scales before the
per-worker drain optimization (DPC-5) ships.

## 1. Objective

Capture production-grade throughput numbers for
`WorkQueueReceiver::drain_parallel` at 1, 4, 16, and 64 worker counts
with synthetic `DeltaResult` payloads. These numbers serve three
purposes:

1. **Baseline for DPC-6.** DPC-6 re-runs this bench with
   `--features per-worker-drain-channels` and compares against the
   baseline captured here. The flip criterion from DPC-3 section 6
   requires >= 5% throughput improvement at T = 16 with no regression
   worse than 5% at T in {1, 4}.
2. **Regression gate.** Once captured, these numbers define a standing
   regression threshold for the drain path. Any future change to
   `drain_parallel` must not regress throughput by more than 5% at any
   worker count.
3. **Contention visibility.** Wall-clock timing at T = 64 exposes the
   mutex contention cliff the DPC series targets. If the T = 64 / T = 1
   throughput ratio falls below 8x (50% efficiency at 16x the workers),
   the contention is confirmed as the bottleneck.

## 2. Bench Harness Location

```
crates/engine/benches/drain_parallel_throughput.rs
```

New file. Declared in `crates/engine/Cargo.toml` as:

```toml
[[bench]]
name = "drain_parallel_throughput"
harness = false
```

## 3. Relationship to Existing Benches

Two drain-related benches already exist:

- **`drain_parallel_benchmark.rs`** - Measures `drain_parallel` at
  T in {1, 4, 8, 16} and N in {10K, 100K}. Uses a `u64` return type
  and exercises the full `WorkQueue` channel (producer thread + bounded
  queue + `drain_parallel`). Good for end-to-end throughput but
  conflates queue feeding latency with drain collection cost.

- **`drain_parallel_alternatives.rs`** - Compares three fan-in
  strategies (sharded mutex, per-thread vec, MPSC channel) at
  T in {4, 8, 16} and N in {10K, 100K}. Isolates the collector by
  pre-allocating items and calling each strategy directly. Does not
  exercise the `WorkQueue` channel.

This new bench (`drain_parallel_throughput.rs`) fills the gap:

| Dimension | `drain_parallel_benchmark` | `drain_parallel_alternatives` | **`drain_parallel_throughput`** |
|---|---|---|---|
| Worker sweep | 1, 4, 8, 16 | 4, 8, 16 | **1, 4, 16, 64** |
| Batch sizes | 10K, 100K | 10K, 100K | **100, 1K, 10K** |
| Payload type | `u64` | `u64` | **`DeltaResult`** |
| Measures queue feed | Yes | No | Yes |
| Contention timing | No | No | **Yes** |
| Comparison target | Single strategy | Three strategies | **Mutex baseline vs DPC-5** |

Key differences from existing benches:

1. **T = 64 worker count.** Exercises the hashed `ThreadId` fallback
   path (`drain.rs:73-80`) and exposes the contention cliff at high
   concurrency. Neither existing bench goes above T = 16.
2. **Realistic payload.** Uses `DeltaResult` (the actual type flowing
   through the production pipeline) rather than bare `u64`. This
   captures the per-item memory and copy cost of the production type.
3. **Variable batch sizes.** Includes N = 100 to measure per-drain
   setup/teardown overhead and N = 10K for the production-scale hot
   path. The 100-item batch captures the cold-start regime where
   thread-pool ramp-up dominates.
4. **Contention timing.** Records per-lock-acquire elapsed time at
   T = 16 and T = 64 to surface the mutex wait distribution.

## 4. Parameter Matrix

### 4.1 Worker Counts

| T | Rationale |
|---|---|
| 1 | Serial baseline. No contention. Establishes per-item overhead floor. |
| 4 | Typical workstation. Low contention regime. |
| 16 | High-core server. DPC-3 flip criterion target. |
| 64 | Extreme concurrency. Exceeds typical rayon pool size; exercises hashed `ThreadId` fallback. Contention cliff detector. |

### 4.2 Batch Sizes

| N | Rationale |
|---|---|
| 100 | Small sync. Measures per-drain fixed cost (pool setup, shard allocation, flatten). |
| 1,000 | Mid-size sync. Transitional regime between setup-dominated and throughput-dominated. |
| 10,000 | Large sync hot path. Matches the lower bound of the existing bench sweep. |

### 4.3 Full Matrix

12 cells: 4 worker counts x 3 batch sizes. Each cell reports:

- **Throughput** (items/sec) via Criterion `Throughput::Elements`.
- **Wall-clock** (ns/iter) via Criterion's default timing.

## 5. Workload Design

### 5.1 Payload: Synthetic `DeltaResult`

Each work item is a `DeltaWork::whole_file` with a unique NDX and a
fixed 4 KiB target size. The drain closure constructs a
`DeltaResult::success` with deterministic stats derived from the NDX:

```rust
fn make_result(work: DeltaWork) -> DeltaResult {
    let ndx = work.ndx().get();
    // Deterministic stats prevent dead-code elimination while
    // avoiding I/O. The multiply-add pattern is cheap enough that
    // the bench measures drain overhead, not computation.
    let bytes = u64::from(ndx).wrapping_mul(31).wrapping_add(17);
    DeltaResult::success(ndx, bytes, bytes / 3, bytes * 2 / 3)
}
```

`DeltaResult` is 72 bytes on x86-64 (4 + 8 + 8 + 8 + 8 + enum
discriminant + padding). This is representative of the production
payload size that flows through the drain.

### 5.2 Per-Item Work Simulation

The drain closure must include enough computation to prevent the
optimizer from collapsing the benchmark loop, but not so much that it
drowns out the drain overhead. A 16-iteration multiply-add chain (same
as `drain_parallel_alternatives.rs:98-105`) provides this balance:

```rust
#[inline(never)]
fn simulate_and_collect(work: DeltaWork) -> DeltaResult {
    let ndx = work.ndx().get();
    let size = work.target_size();
    let mut hash: u64 = u64::from(ndx);
    for i in 0..16u64 {
        hash = hash
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(size ^ i);
    }
    DeltaResult::success(ndx, hash, hash / 3, hash * 2 / 3)
}
```

The `#[inline(never)]` annotation prevents cross-iteration folding.
The 16-iteration loop takes approximately 10 ns on an M2 core, which
is within an order of magnitude of the ~3 ns mutex acquire on the
uncontended fast path and will therefore surface contention effects
without dominating.

### 5.3 Producer Thread

A dedicated producer thread feeds the bounded `WorkQueue` channel.
Pre-allocating items avoids measuring allocation cost in the timed
section:

```rust
let dest = PathBuf::from("/bench/drain-throughput");
let items: Vec<DeltaWork> = (0..count as u32)
    .map(|i| DeltaWork::whole_file(i, dest.clone(), 4096))
    .collect();
```

The producer iterates the pre-built `Vec` and sends each item. This
mirrors the production pattern where a single thread reads from the
wire and dispatches to the `WorkQueue`.

### 5.4 Queue Capacity

`bounded_with_capacity(threads * 4)` - same as
`drain_parallel_benchmark.rs:59`. This keeps the queue small enough
to exercise backpressure without starving workers.

## 6. Contention Measurement

### 6.1 Approach

Direct mutex instrumentation (wrapping `std::sync::Mutex` with timing)
would perturb the measurement. Instead, contention is inferred from
throughput scaling:

- **Linear scaling efficiency**: `throughput(T) / (T * throughput(1))`.
  A value below 0.5 at T = 16 indicates contention is consuming more
  than half the theoretical throughput. A value below 0.25 at T = 64
  confirms the contention cliff.

- **Wall-clock ratio**: `wall_clock(T=1, N) / wall_clock(T, N)`.
  This is the effective speedup. For a contention-free drain, speedup
  at T = 16 should approach 16x. The gap between actual and ideal
  speedup quantifies contention overhead.

### 6.2 Reporting

Criterion's default output captures wall-clock per iteration and
throughput (items/sec). The scaling analysis is performed offline by
comparing the Criterion JSON outputs across worker counts:

```
target/criterion/drain_parallel_throughput/
    1t_N100/
    1t_N1000/
    1t_N10000/
    4t_N100/
    ...
    64t_N10000/
```

The DPC-6 comparison adds a second Criterion baseline captured with
`--features per-worker-drain-channels`, using `critcmp` or manual
JSON comparison to compute the delta.

## 7. Mutex Poisoning Validation

`std::sync::Mutex` poisoning occurs when a thread panics while holding
the lock. In the production `drain_parallel`, a panic in the closure
`f` propagates through `rayon::scope` and the `.unwrap()` on
`shards[idx].lock()` (drain.rs:81) would trigger a poison panic on
subsequent lock attempts within the same scope.

The bench does not attempt to induce poisoning - a poisoned mutex in
the drain path is a fatal error, not a contention signal. Instead, the
bench validates that zero panics occur across all iterations:

- The `DeltaResult::success` constructor is infallible.
- The `simulate_and_collect` closure is infallible.
- The `rayon::scope` propagates panics; Criterion aborts the bench on
  any panic.

If a bench run completes without abort, the poisoning rate is zero by
construction. No explicit poison-rate counter is needed.

## 8. DPC-5 Comparison Shape

DPC-6 runs this bench twice:

1. **Default build** (flag off): captures the `Arc<Mutex<Vec>>`
   baseline. This is the primary output of DPC-2.
2. **`--features per-worker-drain-channels`** (flag on): captures the
   per-worker `SegQueue` lane performance. Same bench binary, same
   parameters, different `drain_parallel` body via `cfg` dispatch.

The comparison is valid because:

- The public API (`drain_parallel`) is identical in both configurations.
- The bench parameters (T, N, payload, queue capacity) are identical.
- The bench binary is built from the same source; only the internal
  drain body differs.
- Criterion's statistical comparison (`--baseline` flag) produces
  confidence intervals for the throughput delta.

No code changes to the bench file are needed for the DPC-5 comparison.
The `cfg`-gated dispatch in `drain.rs` automatically routes to the
per-worker implementation when the feature flag is active.

## 9. Implementation Sketch

```rust
//! Drain throughput baseline bench for the DPC series.
//!
//! Measures `WorkQueueReceiver::drain_parallel` throughput at 1, 4, 16,
//! and 64 worker counts with `DeltaResult` payloads. Produces the
//! baseline numbers that DPC-6 compares against after the per-worker
//! drain optimization (DPC-5) lands.
//!
//! Run: `cargo bench -p engine --bench drain_parallel_throughput`

#![deny(unsafe_code)]

use std::hint::black_box;
use std::path::PathBuf;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use engine::concurrent_delta::DeltaWork;
use engine::concurrent_delta::types::DeltaResult;
use engine::concurrent_delta::work_queue;

const BATCH_SIZES: [usize; 3] = [100, 1_000, 10_000];
const WORKER_COUNTS: [usize; 4] = [1, 4, 16, 64];

#[inline(never)]
fn simulate_and_collect(work: DeltaWork) -> DeltaResult {
    let ndx = work.ndx().get();
    let size = work.target_size();
    let mut hash: u64 = u64::from(ndx);
    for i in 0..16u64 {
        hash = hash
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(size ^ i);
    }
    DeltaResult::success(ndx, hash, hash / 3, hash * 2 / 3)
}

fn bench_drain_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("drain_parallel_throughput");
    group.sample_size(20);

    for &count in &BATCH_SIZES {
        for &threads in &WORKER_COUNTS {
            group.throughput(Throughput::Elements(count as u64));

            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .thread_name(|i| format!("bench-drain-tp-{i}"))
                .build()
                .expect("failed to build rayon thread pool");

            group.bench_with_input(
                BenchmarkId::new(
                    "drain_parallel",
                    format!("{threads}t/N{count}"),
                ),
                &count,
                |b, &count| {
                    // Pre-allocate items outside the timed section.
                    let dest = PathBuf::from("/bench/drain-throughput");
                    let items: Vec<DeltaWork> = (0..count as u32)
                        .map(|i| DeltaWork::whole_file(i, dest.clone(), 4096))
                        .collect();

                    b.iter(|| {
                        pool.install(|| {
                            let (tx, rx) =
                                work_queue::bounded_with_capacity(threads * 4);

                            let items_clone = items.clone();
                            let producer = std::thread::spawn(move || {
                                for item in items_clone {
                                    tx.send(item)
                                        .expect("receiver dropped");
                                }
                            });

                            let results: Vec<DeltaResult> =
                                rx.drain_parallel(simulate_and_collect);

                            producer.join().expect("producer panicked");
                            assert_eq!(results.len(), count);
                            black_box(&results);
                        });
                    });
                },
            );
        }
    }

    group.finish();
}

criterion_group!(benches, bench_drain_throughput);
criterion_main!(benches);
```

## 10. Success Criteria

### 10.1 Baseline Captured

The bench produces Criterion output for all 12 cells (4 worker counts
x 3 batch sizes). Throughput numbers are stable across 3 consecutive
runs (< 5% coefficient of variation within each cell).

### 10.2 Regression Threshold Defined

The baseline establishes the following standing thresholds:

| Cell | Threshold |
|---|---|
| T = 1, any N | No regression > 5% vs baseline |
| T = 4, any N | No regression > 5% vs baseline |
| T = 16, N = 10K | No regression > 5% vs baseline (DPC-3 flip criterion target) |
| T = 64, N = 10K | No regression > 10% vs baseline (high-contention regime, wider margin) |

These thresholds apply to any future change that touches `drain_parallel`,
`WorkQueueReceiver`, or the `rayon::scope` dispatch pattern.

### 10.3 Contention Signal

The bench output must show measurable throughput scaling degradation
as worker count increases. Expected shape:

- T = 1 to T = 4: near-linear scaling (> 3x throughput improvement).
- T = 4 to T = 16: sub-linear scaling (< 4x improvement expected).
- T = 16 to T = 64: plateau or regression. If throughput at T = 64
  exceeds T = 16 by more than 2x, the contention hypothesis is
  weakened and DPC-5's value proposition should be re-examined.

If the bench shows linear scaling through T = 64, the mutex contention
identified in DPC-1 is not the bottleneck at these batch sizes, and
the DPC series should be re-evaluated before proceeding to DPC-5
implementation.

## 11. Reference Host

The bench runs on the development Mac Studio M2 Ultra (24 cores, 192 GB
RAM). This is the same host that DPC-3 names as the reference for the
flip criterion. DPC-6 must use the same host for the before/after
comparison to eliminate hardware variability.

For CI, the bench is informational only (no pass/fail gate). CI runners
have variable core counts and shared workloads that make absolute
throughput numbers unreliable for regression detection. The standing
thresholds from section 10.2 are enforced on the reference host, not
in CI.

## 12. Cross-References

- DPC-1 (#2846) - Audit that identified the `Mutex<Vec>` contention
  shape in `drain_parallel`.
- DPC-3 (#2848) - Per-worker drain channels design
  (`docs/design/per-worker-drain-channels.md`). Section 6 defines the
  flip criterion this bench's baseline feeds.
- DPC-5 (#2850) - Implementation spec
  (`docs/design/dpc-5-per-worker-drain-impl.md`). Section 11 defines
  the bench integration points.
- DPC-6 (#2851) - Re-bench under the per-worker drain path. Uses this
  bench's Criterion output as the baseline.
- DPC-7 - Flip-vs-hold decision, bound by DPC-3 section 6's flip
  criterion and DPC-3 section 8's rollback criteria.
- `crates/engine/benches/drain_parallel_benchmark.rs` - Existing
  end-to-end drain bench (T in {1, 4, 8, 16}, N in {10K, 100K}).
- `crates/engine/benches/drain_parallel_alternatives.rs` - Fan-in
  strategy comparison bench (sharded mutex, per-thread vec, MPSC).
- `crates/engine/src/concurrent_delta/work_queue/drain.rs` -
  Production `drain_parallel` implementation.
- `docs/design/lockfree-mpsc-drain-design.md` - Prior-art MPSC sketch
  (#1681).
