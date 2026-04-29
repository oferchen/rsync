//! Scaling benchmark for the `concurrent_delta::ReorderBuffer` ring buffer.
//!
//! Measures insert + drain throughput at 10K, 100K, 1M, and 10M items, comparing
//! the production ring-buffer `ReorderBuffer` against a `BTreeMap`-backed
//! baseline. The ring buffer is expected to scale linearly (O(1) per item),
//! while the `BTreeMap` baseline scales as O(n log n) overall.
//!
//! Run with: `cargo bench -p engine --bench reorder_buffer_scaling`
//!
//! The 10M case is gated behind `--features bench-reorder-10m` style invocations
//! through the `BENCH_REORDER_10M=1` environment variable to keep the default
//! benchmark run within reasonable wall-clock time.

#![deny(unsafe_code)]

use std::collections::BTreeMap;

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use engine::concurrent_delta::ReorderBuffer;

/// Generates a deterministic shuffled permutation of `0..count` using a simple
/// linear congruential generator. Produces small local out-of-order gaps that
/// resemble realistic worker-thread completion patterns.
fn shuffled_with_local_swaps(count: usize) -> Vec<u64> {
    let mut seq: Vec<u64> = (0..count as u64).collect();
    let mut state: u64 = 0xDEAD_BEEF_CAFE_1234;
    // Swap each element with one within a small local window so out-of-order
    // gaps stay bounded by capacity and exercise the fast path.
    let window = 16;
    for i in 0..seq.len() {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let span = ((state >> 33) as usize) % window;
        let j = (i + span).min(seq.len() - 1);
        seq.swap(i, j);
    }
    seq
}

/// Inserts items into the ring-buffer `ReorderBuffer`, draining opportunistically
/// to keep the ring within capacity.
fn run_ring(insertion_order: &[u64], capacity: usize) -> u64 {
    let mut buf: ReorderBuffer<u64> = ReorderBuffer::new(capacity);
    let mut sum: u64 = 0;
    for &seq in insertion_order {
        // On capacity exceeded, drain ready items then force-insert to make room.
        if buf.insert(seq, seq).is_err() {
            for v in buf.drain_ready() {
                sum = sum.wrapping_add(v);
            }
            buf.force_insert(seq, seq);
        }
        for v in buf.drain_ready() {
            sum = sum.wrapping_add(v);
        }
    }
    for v in buf.drain_ready() {
        sum = sum.wrapping_add(v);
    }
    sum
}

/// Inserts items into a `BTreeMap`-backed reorder buffer, draining contiguous
/// runs from `next_expected`. Mirrors the pre-#1734 implementation that the
/// ring buffer replaced.
fn run_btreemap(insertion_order: &[u64]) -> u64 {
    let mut pending: BTreeMap<u64, u64> = BTreeMap::new();
    let mut next_expected: u64 = 0;
    let mut sum: u64 = 0;
    for &seq in insertion_order {
        pending.insert(seq, seq);
        while let Some(v) = pending.remove(&next_expected) {
            sum = sum.wrapping_add(v);
            next_expected += 1;
        }
    }
    sum
}

fn bench_count(c: &mut Criterion, count: usize, group_name: &str) {
    let mut group = c.benchmark_group(group_name);
    group.throughput(Throughput::Elements(count as u64));
    // Larger counts dominate criterion's default sample size; reduce for
    // 1M and 10M to keep wall time reasonable.
    if count >= 1_000_000 {
        group.sample_size(10);
    }

    let order = shuffled_with_local_swaps(count);

    // Capacity must comfortably exceed the local swap window (16) so the ring
    // path stays on the O(1) fast track without falling back to grow.
    let capacity = 1024usize;

    group.bench_with_input(
        BenchmarkId::new("ring", format!("cap{capacity}")),
        &order,
        |b, order| {
            b.iter(|| black_box(run_ring(black_box(order), capacity)));
        },
    );

    group.bench_with_input(
        BenchmarkId::new("btreemap", "baseline"),
        &order,
        |b, order| {
            b.iter(|| black_box(run_btreemap(black_box(order))));
        },
    );

    group.finish();
}

fn bench_10k(c: &mut Criterion) {
    bench_count(c, 10_000, "reorder_scaling_10k");
}

fn bench_100k(c: &mut Criterion) {
    bench_count(c, 100_000, "reorder_scaling_100k");
}

fn bench_1m(c: &mut Criterion) {
    bench_count(c, 1_000_000, "reorder_scaling_1m");
}

/// 10M scaling run, gated behind `BENCH_REORDER_10M=1` so default benchmark
/// invocations stay fast. Provides the data point the perf review needs to
/// confirm O(1) per item at million-plus file counts.
fn bench_10m(c: &mut Criterion) {
    if std::env::var("BENCH_REORDER_10M").is_err() {
        return;
    }
    bench_count(c, 10_000_000, "reorder_scaling_10m");
}

criterion_group!(benches, bench_10k, bench_100k, bench_1m, bench_10m);
criterion_main!(benches);
