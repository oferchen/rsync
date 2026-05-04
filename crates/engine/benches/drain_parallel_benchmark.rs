//! Work queue `drain_parallel` contention benchmark.
//!
//! Measures throughput of [`WorkQueueReceiver::drain_parallel`] at 10K and 100K
//! items across 1, 4, 8, and 16 rayon threads. Each worker simulates realistic
//! per-item cost via a rolling hash computation to prevent dead-code elimination.
//!
//! Run with: `cargo bench -p engine --bench drain_parallel_benchmark`

#![deny(unsafe_code)]

use std::path::PathBuf;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

use engine::concurrent_delta::DeltaWork;
use engine::concurrent_delta::work_queue;

/// Item counts to benchmark.
const COUNTS: [usize; 2] = [10_000, 100_000];

/// Thread counts to benchmark.
const THREAD_COUNTS: [usize; 4] = [1, 4, 8, 16];

/// Simulates per-item work by computing a simple rolling hash.
///
/// This prevents the optimizer from eliding the closure body while
/// staying cheap enough that the benchmark measures queue overhead
/// rather than pure computation.
#[inline(never)]
fn simulate_work(ndx: u32, size: u64) -> u64 {
    let mut hash: u64 = u64::from(ndx);
    for i in 0..64u64 {
        hash = hash
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(size ^ i);
    }
    hash
}

fn bench_drain_parallel(c: &mut Criterion) {
    let mut group = c.benchmark_group("drain_parallel");

    for &count in &COUNTS {
        for &threads in &THREAD_COUNTS {
            group.throughput(Throughput::Elements(count as u64));

            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .expect("failed to build rayon thread pool");

            group.bench_with_input(
                BenchmarkId::new("drain_parallel", format!("{threads}t/{count}")),
                &count,
                |b, &count| {
                    b.iter(|| {
                        pool.install(|| {
                            let (tx, rx) = work_queue::bounded_with_capacity(threads * 4);

                            let producer = std::thread::spawn(move || {
                                let dest = PathBuf::from("/bench/dst");
                                for i in 0..count as u32 {
                                    tx.send(DeltaWork::whole_file(i, dest.clone(), u64::from(i)))
                                        .expect("receiver dropped unexpectedly");
                                }
                            });

                            let results: Vec<u64> = rx.drain_parallel(|w| {
                                let hash = simulate_work(w.ndx().get(), w.target_size());
                                black_box(hash)
                            });

                            producer.join().expect("producer thread panicked");
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

criterion_group!(drain_parallel, bench_drain_parallel);
criterion_main!(drain_parallel);
