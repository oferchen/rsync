//! BufferPool lock contention benchmark.
//!
//! Compares the lock-free `ArrayQueue`-backed `BufferPool` against a
//! `Mutex<Vec<Vec<u8>>>` baseline across 1, 4, 8, and 16 concurrent threads.
//! Each thread repeatedly acquires a buffer, writes to it, and releases it.
//!
//! Run with: `cargo bench -p engine --bench buffer_pool_benchmark`

use std::sync::{Arc, Mutex};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

use engine::local_copy::buffer_pool::BufferPool;

/// Buffer size matching the production pool default (128 KB).
const BUFFER_SIZE: usize = 128 * 1024;

/// Number of acquire/release cycles each thread performs per iteration.
const OPS_PER_THREAD: usize = 200;

/// Pool capacity - large enough that buffers are typically reused, not dropped.
const POOL_CAPACITY: usize = 32;

/// Mutex-based buffer pool baseline for comparison.
///
/// This mirrors what `BufferPool` would look like if backed by a simple
/// `Mutex<Vec<Vec<u8>>>` instead of a lock-free `ArrayQueue`.
struct MutexPool {
    buffers: Mutex<Vec<Vec<u8>>>,
    buffer_size: usize,
    max_buffers: usize,
}

impl MutexPool {
    fn new(max_buffers: usize, buffer_size: usize) -> Self {
        Self {
            buffers: Mutex::new(Vec::with_capacity(max_buffers)),
            buffer_size,
            max_buffers,
        }
    }

    fn acquire(&self) -> Vec<u8> {
        self.buffers
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| vec![0u8; self.buffer_size])
    }

    fn release(&self, buffer: Vec<u8>) {
        let mut pool = self.buffers.lock().unwrap();
        if pool.len() < self.max_buffers {
            pool.push(buffer);
        }
    }
}

/// Benchmarks concurrent acquire/release on the lock-free `BufferPool`.
fn bench_arrayqueue_contention(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer_pool_contention/arrayqueue");

    for num_threads in [1, 4, 8, 16] {
        let total_ops = (num_threads * OPS_PER_THREAD) as u64;
        group.throughput(Throughput::Elements(total_ops));

        let pool = Arc::new(BufferPool::with_buffer_size(POOL_CAPACITY, BUFFER_SIZE));

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{num_threads}_threads")),
            &num_threads,
            |b, &threads| {
                b.iter(|| {
                    std::thread::scope(|s| {
                        for _ in 0..threads {
                            let pool = &pool;
                            s.spawn(move || {
                                for _ in 0..OPS_PER_THREAD {
                                    let mut buf = pool.acquire();
                                    // Simulate a small write to prevent the compiler from
                                    // eliding the acquire entirely.
                                    buf[0] = 0xAB;
                                    black_box(&buf[0]);
                                    drop(buf);
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

/// Benchmarks concurrent acquire/release on the `Mutex<Vec>` baseline.
fn bench_mutex_contention(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer_pool_contention/mutex");

    for num_threads in [1, 4, 8, 16] {
        let total_ops = (num_threads * OPS_PER_THREAD) as u64;
        group.throughput(Throughput::Elements(total_ops));

        let pool = Arc::new(MutexPool::new(POOL_CAPACITY, BUFFER_SIZE));

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{num_threads}_threads")),
            &num_threads,
            |b, &threads| {
                b.iter(|| {
                    std::thread::scope(|s| {
                        for _ in 0..threads {
                            let pool = &pool;
                            s.spawn(move || {
                                for _ in 0..OPS_PER_THREAD {
                                    let mut buf = pool.acquire();
                                    buf[0] = 0xAB;
                                    black_box(&buf[0]);
                                    pool.release(buf);
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
    buffer_pool_contention,
    bench_arrayqueue_contention,
    bench_mutex_contention
);

criterion_main!(buffer_pool_contention);
