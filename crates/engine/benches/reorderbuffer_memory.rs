//! `ReorderBuffer` memory-occupancy benchmark.
//!
//! The [`ReorderBuffer`] is the merge point for parallel delta results in the
//! `concurrent_delta` pipeline. Workers complete in arbitrary order; the buffer
//! restores file-list order before downstream consumers (checksum verification,
//! metadata commit) observe results. At 100K+ files its occupancy directly
//! determines whether async dispatch can keep up with the receiver and whether
//! spill-to-tempfile (issue #1884) is needed.
//!
//! This benchmark synthesises 100K, 500K, and 1M out-of-order inserts across a
//! range of drift windows and reports:
//!
//! - Insert + drain throughput (Criterion `Throughput::Elements`).
//! - Peak buffer occupancy via the [`metrics().max_depth`] accessor exposed by
//!   the stall-metrics work in PR #4195.
//!
//! Run with: `cargo bench -p engine --bench reorderbuffer_memory`. The 1M case
//! is marked `#[ignore]` for default invocations and can be enabled by setting
//! `BENCH_REORDER_MEMORY_1M=1` in the environment.
//!
//! # Drift window
//!
//! Drift is how far ahead of `next_expected` an item can arrive before it has
//! to wait in the ring. Larger drift simulates more out-of-order completions:
//! a worker for sequence `next_expected + drift - 1` may finish first while
//! the worker for `next_expected` is still running. The four configured drift
//! values (32, 256, 2048, 16K) span the realistic range from tight scheduling
//! (small thread pool, balanced work) to long-tail stalls (large pool,
//! variable per-file cost).
//!
//! # Interpreting `max_depth`
//!
//! Compare the printed `max_depth` against the in-flight parallel dispatch
//! capacity (`work_queue` bound times worker count). If `max_depth` approaches
//! that bound the pipeline is fully saturated. If it greatly exceeds the bound
//! something upstream of the buffer is releasing more work than the consumer
//! can drain, and the spill layer (issue #1884) is the appropriate mitigation.
//!
//! # Favorable vs unfavorable readings
//!
//! - **Favorable** (`max_depth` stays a small multiple of drift across counts):
//!   the ring sits in cache, no allocation pressure, and the fixed-capacity
//!   path is sufficient. Adaptive sizing (issue #1834) is not warranted.
//! - **Unfavorable** (`max_depth` grows with the input count for a constant
//!   drift, or saturates the configured capacity): a slow item is starving
//!   delivery and the buffer is acting as an unbounded queue. This is the
//!   signal that the spill layer (issue #1884) or adaptive growth (issue
//!   #1834) needs to kick in.

#![deny(unsafe_code)]

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use engine::concurrent_delta::ReorderBuffer;
use std::hint::black_box;

/// Drift windows to evaluate. Each value is the maximum offset ahead of
/// `next_expected` at which an out-of-order insert may land.
const DRIFTS: [usize; 4] = [32, 256, 2_048, 16_384];

/// Item counts evaluated for routine runs.
const DEFAULT_COUNTS: [usize; 2] = [100_000, 500_000];

/// Item count for the heavy run, gated behind `BENCH_REORDER_MEMORY_1M`.
const HEAVY_COUNT: usize = 1_000_000;

/// Generates a deterministic out-of-order permutation of `0..count` where each
/// element is displaced by at most `drift - 1` positions from its original
/// index. The permutation is pre-allocated outside the timed section so the
/// benchmark measures buffer behaviour rather than RNG cost.
fn drifted_permutation(count: usize, drift: usize) -> Vec<u64> {
    assert!(drift > 0, "drift must be non-zero");
    let mut seq: Vec<u64> = (0..count as u64).collect();
    let mut state: u64 = 0x0123_4567_89AB_CDEF;
    for i in 0..seq.len() {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let span = ((state >> 33) as usize) % drift;
        let j = (i + span).min(seq.len() - 1);
        seq.swap(i, j);
    }
    seq
}

/// Drives a single insert + drain cycle and returns the buffer's peak depth.
///
/// The ring is sized to comfortably accommodate the drift window; if an item
/// still cannot fit (the random walk can briefly extend the gap), the buffer
/// is force-grown so the benchmark measures occupancy rather than rejection.
fn run_cycle(order: &[u64], capacity: usize) -> (u64, usize) {
    let mut buf: ReorderBuffer<u64> = ReorderBuffer::new(capacity);
    let mut sum: u64 = 0;
    for &seq in order {
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
    let metrics = buf.metrics();
    (sum, metrics.max_depth)
}

/// Benchmarks every (drift, count) combination in the supplied list. The
/// permutation is built once per pair and reused across Criterion samples so
/// the timed section is dominated by buffer operations.
fn bench_matrix(c: &mut Criterion, counts: &[usize], group_name: &str) {
    let mut group = c.benchmark_group(group_name);

    for &count in counts {
        group.throughput(Throughput::Elements(count as u64));
        if count >= 500_000 {
            group.sample_size(10);
        }

        for &drift in &DRIFTS {
            let order = drifted_permutation(count, drift);
            // Capacity is 4x drift so steady-state inserts stay on the O(1)
            // fast path; force_insert handles the rare overshoot.
            let capacity = (drift * 4).max(64);

            // Report the high-water mark once per (count, drift) pair so
            // operators see the peak occupancy alongside Criterion's timing.
            let (_sum, peak) = run_cycle(&order, capacity);
            println!(
                "reorderbuffer_memory: count={count} drift={drift} capacity={capacity} \
                 max_depth={peak}",
            );

            group.bench_with_input(
                BenchmarkId::new("insert_drain", format!("drift{drift}/n{count}")),
                &order,
                |b, order| {
                    b.iter(|| {
                        let (sum, peak) = run_cycle(black_box(order), capacity);
                        black_box((sum, peak));
                    });
                },
            );
        }
    }

    group.finish();
}

fn bench_default(c: &mut Criterion) {
    bench_matrix(c, &DEFAULT_COUNTS, "reorderbuffer_memory");
}

/// 1M run, gated behind `BENCH_REORDER_MEMORY_1M=1` to keep the default
/// benchmark wall-clock reasonable. Marked `#[ignore]`-equivalent by skipping
/// when the environment variable is unset.
fn bench_heavy(c: &mut Criterion) {
    if std::env::var("BENCH_REORDER_MEMORY_1M").is_err() {
        return;
    }
    bench_matrix(c, &[HEAVY_COUNT], "reorderbuffer_memory_1m");
}

criterion_group!(benches, bench_default, bench_heavy);
criterion_main!(benches);
