//! Large-scale benchmark for `BoundedReorderBuffer` insert + drain.
//!
//! Measures the BTreeMap-backed reorder buffer at 1M and 10M item scale,
//! covering both the in-order fast path and the worst-case reverse-order
//! pattern (every insert lands at the high end of the window, so each gap
//! fill triggers a maximum-length contiguous drain).
//!
//! Run with: `cargo bench -p transfer --bench reorder_buffer_large_scale`
//!
//! The 10M case is gated behind the `BENCH_REORDER_10M=1` environment
//! variable so default benchmark runs stay within CI wall-clock budgets.

#![deny(unsafe_code)]

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use transfer::reorder_buffer::BoundedReorderBuffer;

/// Window size for the bounded reorder buffer at large scale.
///
/// Sized to comfortably exceed any realistic worker-thread completion gap
/// while keeping the BTreeMap working set small enough to stay in cache.
const WINDOW: u64 = 1024;

/// Drives an in-order insertion pattern: `seq` increments by 1 every step,
/// so each `insert` immediately drains a single item. Exercises the
/// fast-path branch in `BoundedReorderBuffer::insert`.
fn run_in_order(count: u64) -> u64 {
    let mut buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::new(WINDOW);
    let mut sum: u64 = 0;
    for seq in 0..count {
        let drained = buf
            .insert(seq, seq)
            .expect("in-order insert never triggers backpressure");
        for v in drained {
            sum = sum.wrapping_add(v);
        }
    }
    sum
}

/// Drives a worst-case reverse-order pattern within each window: items
/// arrive descending from `(window_base + WINDOW - 1)` down to
/// `window_base`, so the final insert at `window_base` triggers a full
/// `WINDOW`-length contiguous drain. Repeated for `count / WINDOW`
/// windows. This is the adversarial case for the BTreeMap insert path
/// because every insert must walk to a deeper internal node before the
/// final gap-fill drains the entire range in one pass.
fn run_reverse_order(count: u64) -> u64 {
    let count = (count / WINDOW) * WINDOW;
    let mut buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::new(WINDOW);
    let mut sum: u64 = 0;
    let mut base: u64 = 0;
    while base < count {
        // Insert WINDOW items high-to-low; only the final insert at `base`
        // closes the gap and triggers a single contiguous drain.
        for offset in (0..WINDOW).rev() {
            let drained = buf
                .insert(base + offset, base + offset)
                .expect("window-sized batch never triggers backpressure");
            for v in drained {
                sum = sum.wrapping_add(v);
            }
        }
        base += WINDOW;
    }
    sum
}

fn bench_count(c: &mut Criterion, count: u64, group_name: &str) {
    let mut group = c.benchmark_group(group_name);
    group.throughput(Throughput::Elements(count));
    // Million-plus item runs dominate criterion's default sample size; reduce
    // sample count to keep wall time bounded while still producing stable
    // per-element throughput numbers.
    group.sample_size(10);

    group.bench_with_input(
        BenchmarkId::new("in_order", format!("W{WINDOW}")),
        &count,
        |b, &count| {
            b.iter(|| black_box(run_in_order(black_box(count))));
        },
    );

    group.bench_with_input(
        BenchmarkId::new("reverse_order", format!("W{WINDOW}")),
        &count,
        |b, &count| {
            b.iter(|| black_box(run_reverse_order(black_box(count))));
        },
    );

    group.finish();
}

/// 1M scaling run. Always enabled so CI tracks regressions without
/// depending on opt-in environment configuration.
fn bench_reorder_buffer_1m(c: &mut Criterion) {
    bench_count(c, 1_000_000, "reorder_buffer_large_scale_1m");
}

/// 10M scaling run, gated behind `BENCH_REORDER_10M=1` so default
/// benchmark invocations stay fast. Provides the data point that
/// confirms BTreeMap insert + drain stays well-behaved at the 10M-file
/// transfer ceiling.
fn bench_reorder_buffer_10m(c: &mut Criterion) {
    if std::env::var("BENCH_REORDER_10M").is_err() {
        return;
    }
    bench_count(c, 10_000_000, "reorder_buffer_large_scale_10m");
}

criterion_group!(benches, bench_reorder_buffer_1m, bench_reorder_buffer_10m);
criterion_main!(benches);
