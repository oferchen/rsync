//! Benchmarks for performance optimizations.
//!
//! Run with: `cargo bench -p engine --features optimized-buffers,batch-sync`

use std::sync::Arc;

use criterion::{
    black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput,
};

#[cfg(feature = "optimized-buffers")]
use engine::local_copy::buffer_pool::BufferPool;

/// Size used by default for copy operations.
const COPY_BUFFER_SIZE: usize = 256 * 1024;

/// Benchmark direct allocation vs buffer pool.
#[cfg(feature = "optimized-buffers")]
fn bench_buffer_allocation(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer_allocation");
    group.throughput(Throughput::Elements(1));

    // Benchmark direct allocation
    group.bench_function("direct_alloc", |b| {
        b.iter(|| {
            let buffer = vec![0u8; COPY_BUFFER_SIZE];
            black_box(buffer)
        });
    });

    // Benchmark buffer pool acquisition (cold - no buffers in pool)
    let pool = Arc::new(BufferPool::new(4));
    group.bench_function("pool_acquire_cold", |b| {
        b.iter(|| {
            let guard = BufferPool::acquire_from(Arc::clone(&pool));
            black_box(&*guard);
            drop(guard);
        });
    });

    // Benchmark buffer pool acquisition (warm - buffer already in pool)
    {
        // Warm up the pool
        let guard = BufferPool::acquire_from(Arc::clone(&pool));
        drop(guard);
    }
    group.bench_function("pool_acquire_warm", |b| {
        b.iter(|| {
            let guard = BufferPool::acquire_from(Arc::clone(&pool));
            black_box(&*guard);
            drop(guard);
        });
    });

    group.finish();
}

/// Benchmark sequential buffer allocations vs pool reuse.
#[cfg(feature = "optimized-buffers")]
fn bench_sequential_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("sequential_buffer_ops");

    for count in [10, 100, 1000] {
        group.throughput(Throughput::Elements(count as u64));

        group.bench_with_input(
            BenchmarkId::new("direct_alloc", count),
            &count,
            |b, &count| {
                b.iter(|| {
                    for _ in 0..count {
                        let buffer = vec![0u8; COPY_BUFFER_SIZE];
                        black_box(&buffer[0]);
                    }
                });
            },
        );

        let pool = Arc::new(BufferPool::new(4));
        group.bench_with_input(
            BenchmarkId::new("pool_reuse", count),
            &count,
            |b, &count| {
                b.iter(|| {
                    for _ in 0..count {
                        let guard = BufferPool::acquire_from(Arc::clone(&pool));
                        black_box(&*guard);
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark concurrent buffer access patterns.
#[cfg(feature = "optimized-buffers")]
fn bench_concurrent_access(c: &mut Criterion) {
    use std::thread;

    let mut group = c.benchmark_group("concurrent_buffer_access");
    group.throughput(Throughput::Elements(100));

    let pool = Arc::new(BufferPool::new(8));

    group.bench_function("4_threads_100_ops_each", |b| {
        b.iter(|| {
            let mut handles = Vec::new();
            for _ in 0..4 {
                let pool = Arc::clone(&pool);
                handles.push(thread::spawn(move || {
                    for _ in 0..100 {
                        let guard = BufferPool::acquire_from(Arc::clone(&pool));
                        black_box(&*guard);
                    }
                }));
            }
            for handle in handles {
                handle.join().unwrap();
            }
        });
    });

    group.finish();
}

#[cfg(feature = "optimized-buffers")]
criterion_group!(
    buffer_benchmarks,
    bench_buffer_allocation,
    bench_sequential_operations,
    bench_concurrent_access
);

#[cfg(not(feature = "optimized-buffers"))]
criterion_group!(buffer_benchmarks,);

criterion_main!(buffer_benchmarks);
