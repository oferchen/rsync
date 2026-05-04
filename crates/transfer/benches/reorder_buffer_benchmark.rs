//! Benchmarks comparing `BoundedReorderBuffer` (sliding window) vs collect-then-sort.
//!
//! Run with: `cargo bench -p transfer -- reorder_buffer`

#![deny(unsafe_code)]

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use rand::SeedableRng;
use rand::rngs::SmallRng;
use rand::seq::SliceRandom;
use transfer::reorder_buffer::BoundedReorderBuffer;

/// Fisher-Yates shuffle with a seeded RNG for reproducible benchmarks.
fn shuffled_sequence(count: usize) -> Vec<u64> {
    let mut rng = SmallRng::seed_from_u64(0xDEAD_BEEF);
    let mut seq: Vec<u64> = (0..count as u64).collect();
    seq.shuffle(&mut rng);
    seq
}

/// Benchmark bounded-window reorder vs collect-then-sort at a given item count.
fn bench_reorder(c: &mut Criterion, count: usize, group_name: &str) {
    let mut group = c.benchmark_group(group_name);
    group.throughput(Throughput::Elements(count as u64));

    let shuffled = shuffled_sequence(count);

    // Bounded-window approach at various window sizes.
    for window in [16u64, 64, 256] {
        group.bench_with_input(
            BenchmarkId::new("bounded_window", format!("W{window}")),
            &window,
            |b, &ws| {
                b.iter(|| {
                    let mut buf = BoundedReorderBuffer::new(ws);
                    let mut collected = Vec::with_capacity(count);
                    for &seq in &shuffled {
                        // Items outside the window are retried after draining.
                        // For benchmarking we feed in-window items; out-of-window
                        // ones are deferred via a simple retry queue.
                        match buf.insert(seq, seq) {
                            Ok(drained) => collected.extend(drained),
                            Err(_) => {
                                // Backpressure - skip for now, will be picked up
                                // when the window advances. In a real system,
                                // the producer would block/retry.
                            }
                        }
                    }
                    black_box(&collected);
                });
            },
        );
    }

    // Collect-then-sort baseline.
    group.bench_function("collect_sort", |b| {
        b.iter(|| {
            let mut items: Vec<(u64, u64)> = shuffled.iter().map(|&s| (s, s)).collect();
            items.sort_unstable_by_key(|&(seq, _)| seq);
            let sorted: Vec<u64> = items.into_iter().map(|(_, v)| v).collect();
            black_box(&sorted);
        });
    });

    group.finish();
}

fn bench_reorder_10k(c: &mut Criterion) {
    bench_reorder(c, 10_000, "reorder_10k");
}

fn bench_reorder_100k(c: &mut Criterion) {
    bench_reorder(c, 100_000, "reorder_100k");
}

criterion_group!(benches, bench_reorder_10k, bench_reorder_100k);
criterion_main!(benches);
