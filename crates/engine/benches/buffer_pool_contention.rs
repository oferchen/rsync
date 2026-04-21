//! Criterion benchmarks for `BufferPool` contention under parallel workloads.
//!
//! Measures acquire/release throughput at varying thread counts and tracks
//! hit vs miss rates to quantify the effectiveness of the two-level cache
//! (thread-local slot + central Mutex pool).

use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use engine::local_copy::buffer_pool::BufferPool;
use rayon::ThreadPoolBuilder;

/// Number of acquire/release operations per benchmark iteration.
const OPS_PER_ITER: u64 = 10_000;

/// Thread counts to benchmark for contention scaling.
const THREAD_COUNTS: &[usize] = &[2, 4, 8, 16];

/// Simulates a short borrow typical of parallel stat workloads.
///
/// Each operation acquires a buffer, touches a few bytes (simulating a
/// metadata read), then drops the guard to return it. This mirrors the
/// receiver's `quick_check_ok_stateless` pattern where buffers are held
/// only long enough to read file metadata.
#[inline]
fn short_borrow_cycle(pool: &Arc<BufferPool>) {
    let mut guard = BufferPool::acquire_from(Arc::clone(pool));
    // Simulate minimal work - touch first and last bytes.
    guard[0] = 0xAA;
    let last = guard.len() - 1;
    guard[last] = 0xBB;
    // Guard dropped here, returning buffer to pool.
}

/// Single-threaded acquire/release throughput.
///
/// Establishes a baseline with zero contention. The thread-local cache
/// should handle nearly all operations after the first acquire, yielding
/// close to 100% hit rate.
fn bench_single_threaded(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer_pool/single_thread");
    group.throughput(Throughput::Elements(OPS_PER_ITER));

    group.bench_function("acquire_release", |b| {
        let pool = Arc::new(BufferPool::new(4));
        b.iter(|| {
            for _ in 0..OPS_PER_ITER {
                short_borrow_cycle(&pool);
            }
        });
    });

    group.finish();
}

/// Multi-threaded contention at 2, 4, 8, 16 threads.
///
/// Each thread performs `OPS_PER_ITER / thread_count` operations, keeping
/// total work constant to isolate contention overhead. Uses rayon with
/// controlled thread counts via `ThreadPoolBuilder`.
fn bench_multi_threaded_contention(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer_pool/contention");
    group.throughput(Throughput::Elements(OPS_PER_ITER));

    for &threads in THREAD_COUNTS {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{threads}_threads")),
            &threads,
            |b, &threads| {
                let pool = Arc::new(BufferPool::new(threads));
                let rayon_pool = ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .expect("failed to build rayon pool");

                let ops_per_thread = (OPS_PER_ITER as usize) / threads;

                b.iter(|| {
                    rayon_pool.scope(|s| {
                        for _ in 0..threads {
                            let pool = Arc::clone(&pool);
                            s.spawn(move |_| {
                                for _ in 0..ops_per_thread {
                                    short_borrow_cycle(&pool);
                                }
                            });
                        }
                    });
                });
            },
        );
    }

    group.finish();
}

/// Measures hit rate vs miss rate under multi-threaded stat workload.
///
/// Uses the pool's built-in telemetry (`total_hits` / `total_misses`) to
/// report the fraction of acquires served from cache vs fresh allocation.
/// A high hit rate indicates the two-level cache is effective; a low hit
/// rate indicates excessive Mutex contention or undersized pool.
fn bench_hit_miss_rate(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer_pool/hit_miss");
    group.throughput(Throughput::Elements(OPS_PER_ITER));

    for &threads in THREAD_COUNTS {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{threads}_threads")),
            &threads,
            |b, &threads| {
                let rayon_pool = ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .expect("failed to build rayon pool");

                let ops_per_thread = (OPS_PER_ITER as usize) / threads;

                b.iter_custom(|iters| {
                    let mut total_elapsed = std::time::Duration::ZERO;

                    for _ in 0..iters {
                        // Fresh pool each iteration to get clean hit/miss counters.
                        let pool = Arc::new(BufferPool::new(threads));

                        let start = std::time::Instant::now();
                        rayon_pool.scope(|s| {
                            for _ in 0..threads {
                                let pool = Arc::clone(&pool);
                                s.spawn(move |_| {
                                    for _ in 0..ops_per_thread {
                                        short_borrow_cycle(&pool);
                                    }
                                });
                            }
                        });
                        total_elapsed += start.elapsed();

                        // Report hit/miss telemetry on first iteration only
                        // to avoid flooding output.
                        if iters == 1 {
                            let hits = pool.total_hits();
                            let misses = pool.total_misses();
                            let total = hits + misses;
                            if total > 0 {
                                let hit_pct = (hits as f64 / total as f64) * 100.0;
                                eprintln!(
                                    "  [{threads} threads] hits={hits}, misses={misses}, \
                                     hit_rate={hit_pct:.1}%"
                                );
                            }
                        }
                    }

                    total_elapsed
                });
            },
        );
    }

    group.finish();
}

/// Simulated stat workload - many rapid short borrows.
///
/// Models the parallel stat pattern from the receiver where many files
/// are stat'd in quick succession. Each borrow is extremely short (just
/// metadata field access), maximizing contention pressure on the pool.
fn bench_stat_workload(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer_pool/stat_workload");

    // Higher operation count to stress short-borrow patterns.
    let stat_ops: u64 = 50_000;
    group.throughput(Throughput::Elements(stat_ops));

    for &threads in THREAD_COUNTS {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{threads}_threads")),
            &threads,
            |b, &threads| {
                let pool = Arc::new(BufferPool::with_buffer_size(threads, 4096));
                let rayon_pool = ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .expect("failed to build rayon pool");

                let ops_per_thread = (stat_ops as usize) / threads;

                b.iter(|| {
                    rayon_pool.scope(|s| {
                        for _ in 0..threads {
                            let pool = Arc::clone(&pool);
                            s.spawn(move |_| {
                                for _ in 0..ops_per_thread {
                                    // Minimal borrow - simulate reading a single
                                    // metadata field (e.g., mtime, size).
                                    let guard = BufferPool::acquire_from(Arc::clone(&pool));
                                    std::hint::black_box(&*guard);
                                    drop(guard);
                                }
                            });
                        }
                    });
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_single_threaded,
    bench_multi_threaded_contention,
    bench_hit_miss_rate,
    bench_stat_workload,
);
criterion_main!(benches);
