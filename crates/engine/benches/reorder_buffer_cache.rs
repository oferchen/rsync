//! Cache-behavior benchmark for the `concurrent_delta::ReorderBuffer`.
//!
//! Purpose: provide evidence on whether the current `ReorderBuffer`
//! storage layout (`Box<[Option<T>]>` ring buffer) suffers cache-unfriendly
//! access patterns at 1M parallel results. The actual `perf stat` /
//! `cachegrind` runs are operator-driven and not part of CI.
//!
//! What is measured:
//! - Insert 1M out-of-order indexed items, then drain them in order.
//! - Item payload size is varied across {32 B, 256 B, 4 KB} so the
//!   slot footprint sweeps from "comfortably L1/L2-resident" all the
//!   way past the LLC for the 1M-element working set.
//! - Insertion order patterns: {fully reverse, random shuffle,
//!   near-in-order with 10% deltas}, mirroring the realistic mix of
//!   completion orders that rayon workers produce.
//!
//! All payloads are pre-allocated outside the timed section so the
//! benchmark measures `ReorderBuffer` operations, not allocator work.
//!
//! Throughput is reported via `Throughput::Elements(1_000_000)` so
//! `cargo bench` output prints ops/sec for each (payload, pattern)
//! cell.
//!
//! # Running
//!
//! The 1M cells are expensive (5-30 s per iteration), so the bench is
//! gated behind the `BENCH_REORDER_CACHE=1` environment variable. The
//! default `cargo bench` run is a no-op:
//!
//! ```sh
//! # default run: skips entirely
//! cargo bench -p engine --bench reorder_buffer_cache
//!
//! # full 1M cache-behavior sweep (opt-in)
//! BENCH_REORDER_CACHE=1 cargo bench -p engine --bench reorder_buffer_cache
//! ```
//!
//! ## perf stat (Linux)
//!
//! Use `perf stat` to capture cache-miss/cache-reference counters
//! around the timed section. Build the bench binary, then drive it
//! directly with `--profile-time` so the wall-clock window matches
//! perf's sampling window:
//!
//! ```sh
//! BENCH_REORDER_CACHE=1 cargo bench -p engine --bench reorder_buffer_cache --no-run
//! BIN=$(ls -t target/release/deps/reorder_buffer_cache-* | head -n1)
//! perf stat -e cache-misses,cache-references,L1-dcache-load-misses,LLC-load-misses \
//!     -- BENCH_REORDER_CACHE=1 "$BIN" --bench --profile-time 10
//! ```
//!
//! ## cachegrind (any platform via valgrind)
//!
//! Cachegrind is platform-agnostic but ~50x slower than native; expect
//! the 1M cells to take minutes. Drive a single cell at a time using
//! Criterion's filter:
//!
//! ```sh
//! BENCH_REORDER_CACHE=1 cargo bench -p engine --bench reorder_buffer_cache --no-run
//! BIN=$(ls -t target/release/deps/reorder_buffer_cache-* | head -n1)
//! valgrind --tool=cachegrind \
//!     BENCH_REORDER_CACHE=1 "$BIN" --bench reorder_cache_1m/payload256/shuffle
//! cg_annotate cachegrind.out.<pid>
//! ```
//!
//! # How to interpret the numbers
//!
//! The bench provides evidence for one of two design directions:
//!
//! - **Favorable (cache-friendly): high L1 hit rate, low LLC miss rate,
//!   throughput stays flat across patterns.** The ring buffer's slot
//!   layout works at scale. No layout change is justified - the
//!   existing `Box<[Option<T>]>` stays. Optimisation effort should move
//!   to other hot spots in the consumer (e.g., spill).
//! - **Unfavorable (cache-hostile): cache-miss rate scales with payload
//!   size, random shuffle is much slower than near-in-order, LLC misses
//!   dominate insert/drain.** That justifies a layout change. The two
//!   credible options:
//!     1. Replace the `Box<[Option<T>]>` with a flat `Vec<T>` plus a
//!        bitmap of occupancy. Removes the per-slot enum tag/padding
//!        and packs more items into a cache line.
//!     2. Split storage into hot (sequence-index) and cold (payload)
//!        arenas so drain-time scans only touch the index.
//!     For a 1M-file transfer, even a 2-3x reduction in LLC traffic
//!     should be visible end-to-end via the existing throughput benches.

#![deny(unsafe_code)]

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use engine::concurrent_delta::ReorderBuffer;
use std::hint::black_box;

const ITEM_COUNT: usize = 1_000_000;
const PAYLOAD_SIZES: &[usize] = &[32, 256, 4096];

/// Pattern describing how worker completion order maps to insertion
/// order into the reorder buffer.
#[derive(Copy, Clone)]
enum Pattern {
    /// Fully reverse: worker N-1 finishes first, worker 0 finishes last.
    /// Maximises the time items spend buffered and the working-set size.
    Reverse,
    /// Random shuffle over the entire range. Approximates a worst-case
    /// access pattern where no temporal locality exists.
    Shuffle,
    /// Near-in-order: most items arrive close to `next_expected`, with
    /// 10% reordered into local windows of up to 32 slots. Closest to
    /// the production rayon worker pattern.
    NearInOrder,
}

impl Pattern {
    const fn tag(self) -> &'static str {
        match self {
            Pattern::Reverse => "reverse",
            Pattern::Shuffle => "shuffle",
            Pattern::NearInOrder => "near_in_order",
        }
    }
}

/// Builds the insertion order for the given pattern.
///
/// Returned `Vec<u64>` contains a permutation of `0..count` describing
/// the order in which items are handed to the reorder buffer.
fn build_order(count: usize, pattern: Pattern) -> Vec<u64> {
    match pattern {
        Pattern::Reverse => (0..count as u64).rev().collect(),
        Pattern::Shuffle => {
            // Fisher-Yates with a deterministic LCG so runs are
            // reproducible across machines.
            let mut seq: Vec<u64> = (0..count as u64).collect();
            let mut state: u64 = 0x5DEE_CE66_D00B_5EE5;
            for i in (1..seq.len()).rev() {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let j = ((state >> 33) as usize) % (i + 1);
                seq.swap(i, j);
            }
            seq
        }
        Pattern::NearInOrder => {
            // Start in-order, then reorder 10% of indices within a
            // 32-slot local window. Matches the typical rayon
            // completion drift.
            let mut seq: Vec<u64> = (0..count as u64).collect();
            let mut state: u64 = 0x00C0_FFEE_DEAD_BEEF;
            let window = 32usize;
            let target = count / 10;
            for _ in 0..target {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let i = ((state >> 33) as usize) % count;
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let span = ((state >> 33) as usize) % window;
                let j = (i + span).min(count - 1);
                seq.swap(i, j);
            }
            seq
        }
    }
}

/// Pre-allocates `count` payloads of `payload_size` bytes. Each
/// payload's first 8 bytes encode its sequence number so the drain
/// step can verify ordering cheaply.
fn build_payloads(count: usize, payload_size: usize) -> Vec<Vec<u8>> {
    (0..count)
        .map(|i| {
            let mut v = vec![0u8; payload_size];
            let bytes = (i as u64).to_le_bytes();
            let n = bytes.len().min(payload_size);
            v[..n].copy_from_slice(&bytes[..n]);
            v
        })
        .collect()
}

/// Runs the insert-then-drain workload against a single
/// `ReorderBuffer`. Returns a checksum over the drained payloads so
/// the optimiser cannot elide the work.
///
/// Pre-condition: `capacity` is sized so every insert in `order`
/// succeeds without growing the ring (see `capacity_for`). Falling
/// back to `force_insert` would distort the cache-behavior numbers
/// because resize allocates a fresh backing buffer.
fn run_workload(order: &[u64], payloads: Vec<Vec<u8>>, capacity: usize) -> u64 {
    let mut buf: ReorderBuffer<Vec<u8>> = ReorderBuffer::new(capacity);
    // Move payloads out by sequence index. Storing them in an Option
    // vec keeps the per-iteration take() O(1).
    let mut slots: Vec<Option<Vec<u8>>> = payloads.into_iter().map(Some).collect();

    let mut checksum: u64 = 0;
    for &seq in order {
        let payload = slots[seq as usize]
            .take()
            .expect("each sequence is consumed exactly once");
        buf.insert(seq, payload)
            .expect("capacity_for guarantees the insert fits");
        for v in buf.drain_ready() {
            checksum = checksum.wrapping_add(v.len() as u64);
        }
    }
    for v in buf.drain_ready() {
        checksum = checksum.wrapping_add(v.len() as u64);
    }
    checksum
}

/// Capacity sized to cover the worst-case gap each pattern can
/// produce, so the timed section measures the steady-state ring
/// behaviour rather than `grow()` work.
fn capacity_for(pattern: Pattern, count: usize) -> usize {
    match pattern {
        // Reverse fills the ring before any drain. Cap at count so
        // every item fits without `force_insert`.
        Pattern::Reverse => count,
        // Shuffle's expected max gap is ~count; cap at count to stay
        // on the O(1) fast path.
        Pattern::Shuffle => count,
        // Near-in-order tops out at the 32-slot window, but we leave
        // headroom for the LCG occasionally swapping across boundaries.
        Pattern::NearInOrder => 4096,
    }
}

fn bench_cell(c: &mut Criterion, payload_size: usize, pattern: Pattern) {
    let group_name = format!("reorder_cache_1m/payload{payload_size}/{}", pattern.tag());
    let mut group = c.benchmark_group(&group_name);
    group.throughput(Throughput::Elements(ITEM_COUNT as u64));
    // 1M items per iteration is heavy; minimum sample size keeps
    // wall-clock manageable while still smoothing per-run jitter.
    group.sample_size(10);

    let order = build_order(ITEM_COUNT, pattern);
    let capacity = capacity_for(pattern, ITEM_COUNT);

    group.bench_with_input(
        BenchmarkId::from_parameter(format!("cap{capacity}")),
        &order,
        |b, order| {
            b.iter_batched(
                || build_payloads(ITEM_COUNT, payload_size),
                |payloads| black_box(run_workload(black_box(order), payloads, capacity)),
                criterion::BatchSize::PerIteration,
            );
        },
    );

    group.finish();
}

/// Entry point for the 1M cache-behavior sweep. Skips entirely unless
/// `BENCH_REORDER_CACHE=1` is set so a default `cargo bench` run does
/// not spend minutes on this bench.
fn bench_reorder_cache(c: &mut Criterion) {
    if std::env::var("BENCH_REORDER_CACHE").is_err() {
        return;
    }
    for &payload_size in PAYLOAD_SIZES {
        for &pattern in &[Pattern::Reverse, Pattern::Shuffle, Pattern::NearInOrder] {
            bench_cell(c, payload_size, pattern);
        }
    }
}

criterion_group!(benches, bench_reorder_cache);
criterion_main!(benches);
