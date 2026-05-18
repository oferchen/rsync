//! Adaptive vs static `BufferPool` sizing benchmark.
//!
//! Compares a static fixed-size `BufferPool` against an "adaptive" pool wired
//! with the PID-style `AdaptiveBufferController` (the EMA-throughput-driven
//! controller introduced in #1834, source:
//! `crates/engine/src/local_copy/buffer_pool/buffer_controller.rs`).
//!
//! Each workload variant drives a synthetic "transfer loop": acquire a buffer,
//! fill it with a representative write, drop it, repeat. The adaptive pool
//! additionally feeds throughput samples back via `record_transfer` so the PID
//! controller's recommendation evolves over the run.
//!
//! Workload variants (group `adaptive_bufferpool`):
//! - `steady_uniform`  - 1000 acquires at a constant 64 KiB.
//! - `bursty_small`    - alternating bursts of 100 x 4 KiB and 10 x 1 MiB.
//! - `growing`         - 100 acquires each at 16 KiB, 64 KiB, 256 KiB, 1 MiB.
//! - `shrinking`       - reverse of `growing`.
//!
//! Measurements are single-threaded to isolate sizing decisions from
//! contention. The companion `buffer_pool_contention.rs` benchmark covers the
//! contended axis.
//!
//! Run with: `cargo bench -p engine --bench adaptive_bufferpool`

use std::sync::Arc;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

use engine::local_copy::buffer_pool::{BufferPool, ControllerConfig};

/// Pool default buffer size for the static and adaptive variants (128 KiB).
///
/// Matches `COPY_BUFFER_SIZE` so the static pool's thread-local fast path is
/// exercised on same-size acquires. Off-size acquires (e.g. 4 KiB or 1 MiB)
/// take the slow path on both pools, which is the realistic comparison.
const POOL_BUFFER_SIZE: usize = 128 * 1024;

/// Soft capacity of the central pool slot count.
const POOL_CAPACITY: usize = 32;

/// Target throughput setpoint fed to the PID controller (100 MiB/s).
///
/// Chosen as a mid-range LAN-class transfer rate so the controller has room to
/// adjust upward or downward in response to the synthetic samples.
const CONTROLLER_SETPOINT_BPS: u64 = 100 * 1024 * 1024;

/// Workload step: number of acquires at a given buffer size.
#[derive(Clone, Copy)]
struct Step {
    size: usize,
    count: usize,
}

const KIB: usize = 1024;
const MIB: usize = 1024 * 1024;

/// `steady_uniform`: 1000 acquires of 64 KiB constant.
fn workload_steady_uniform() -> Vec<Step> {
    vec![Step {
        size: 64 * KIB,
        count: 1000,
    }]
}

/// `bursty_small`: alternate 100 x 4 KiB then 10 x 1 MiB, five cycles.
fn workload_bursty_small() -> Vec<Step> {
    let mut steps = Vec::with_capacity(10);
    for _ in 0..5 {
        steps.push(Step {
            size: 4 * KIB,
            count: 100,
        });
        steps.push(Step {
            size: MIB,
            count: 10,
        });
    }
    steps
}

/// `growing`: 100 acquires each at 16 KiB, 64 KiB, 256 KiB, 1 MiB.
fn workload_growing() -> Vec<Step> {
    vec![
        Step {
            size: 16 * KIB,
            count: 100,
        },
        Step {
            size: 64 * KIB,
            count: 100,
        },
        Step {
            size: 256 * KIB,
            count: 100,
        },
        Step {
            size: MIB,
            count: 100,
        },
    ]
}

/// `shrinking`: reverse of growing.
fn workload_shrinking() -> Vec<Step> {
    let mut steps = workload_growing();
    steps.reverse();
    steps
}

/// Total bytes touched in one workload pass (used for throughput reporting).
fn workload_bytes(steps: &[Step]) -> u64 {
    steps
        .iter()
        .map(|s| (s.size as u64) * (s.count as u64))
        .sum()
}

/// Constructs a static fixed-size pool.
fn static_pool() -> Arc<BufferPool> {
    Arc::new(BufferPool::with_buffer_size(
        POOL_CAPACITY,
        POOL_BUFFER_SIZE,
    ))
}

/// Constructs an adaptive pool wired with the PID-style controller from
/// `buffer_controller.rs` (#1834).
///
/// `with_buffer_controller` also enables throughput tracking implicitly, so
/// `record_transfer` samples feed both the EMA and the PID accumulator.
fn adaptive_pool() -> Arc<BufferPool> {
    Arc::new(
        BufferPool::with_buffer_size(POOL_CAPACITY, POOL_BUFFER_SIZE)
            .with_buffer_controller(ControllerConfig::new(CONTROLLER_SETPOINT_BPS)),
    )
}

/// Drives a transfer loop against a static pool: acquire by file size,
/// touch first and last byte to defeat dead-store elimination, drop.
fn run_static(pool: &Arc<BufferPool>, steps: &[Step]) {
    for step in steps {
        let file_size = step.size as u64;
        for _ in 0..step.count {
            let mut buf = BufferPool::acquire_adaptive_from(Arc::clone(pool), file_size);
            let last = buf.len() - 1;
            buf[0] = 0xAB;
            buf[last] = 0xCD;
            black_box(&buf[0]);
            black_box(&buf[last]);
            drop(buf);
        }
    }
}

/// Drives a transfer loop against the adaptive pool: same as static, plus a
/// `record_transfer` call after each acquire so the controller observes the
/// throughput signal it was designed to consume.
fn run_adaptive(pool: &Arc<BufferPool>, steps: &[Step]) {
    // Synthetic 1 ms per acquire keeps the bytes-per-second figure in the same
    // order of magnitude as the configured setpoint so the controller is
    // actively exercised rather than saturated against a clamp.
    let synth_dt = Duration::from_millis(1);
    for step in steps {
        let file_size = step.size as u64;
        for _ in 0..step.count {
            let mut buf = BufferPool::acquire_controlled_from(Arc::clone(pool), file_size);
            let last = buf.len() - 1;
            buf[0] = 0xAB;
            buf[last] = 0xCD;
            black_box(&buf[0]);
            black_box(&buf[last]);
            drop(buf);
            pool.record_transfer(step.size, synth_dt);
        }
    }
}

/// Builds a deterministic workload trace consumed by every pool variant.
type WorkloadBuilder = fn() -> Vec<Step>;

/// Registers all workload x pool-variant cells into the Criterion group.
fn bench_adaptive_bufferpool(c: &mut Criterion) {
    let workloads: &[(&str, WorkloadBuilder)] = &[
        ("steady_uniform", workload_steady_uniform),
        ("bursty_small", workload_bursty_small),
        ("growing", workload_growing),
        ("shrinking", workload_shrinking),
    ];

    let mut group = c.benchmark_group("adaptive_bufferpool");

    for (name, build) in workloads {
        let steps = build();
        let bytes = workload_bytes(&steps);
        group.throughput(Throughput::Bytes(bytes));

        group.bench_with_input(BenchmarkId::new("static_pool", name), &steps, |b, steps| {
            let pool = static_pool();
            b.iter(|| run_static(&pool, steps));
        });

        group.bench_with_input(
            BenchmarkId::new("adaptive_pool", name),
            &steps,
            |b, steps| {
                let pool = adaptive_pool();
                b.iter(|| run_adaptive(&pool, steps));
            },
        );
    }

    group.finish();
}

criterion_group!(adaptive_bufferpool, bench_adaptive_bufferpool);
criterion_main!(adaptive_bufferpool);
