//! Criterion micro-benchmark: rayon dispatch shape on a 100K small-file
//! work stream - `par_bridge` (channel-backed) versus `into_par_iter`
//! (work-stealing deque).
//!
//! # Why this exists
//!
//! Several spots in the transfer and engine pipelines feed rayon a stream
//! of file-sized work items. Rayon offers two dispatch entry points:
//!
//! - `IntoParallelIterator::into_par_iter()` over an owned collection -
//!   the work is partitioned across Chase-Lev style work-stealing deques,
//!   one per worker, with split-on-the-fly load balancing.
//! - `ParallelBridge::par_bridge()` - wraps any sequential iterator into a
//!   parallel iterator by funnelling items through an internal MPMC
//!   channel. Convenient when the work source is a generator (no pre-
//!   allocated `Vec`), but every item pays one channel send + receive on
//!   top of the actual work.
//!
//! The hypothesis behind tasks #1284, #1370, #1681 is that at 100K small-
//! file scales the `par_bridge` channel becomes a measurable bottleneck:
//! the per-item cost approaches or exceeds the per-item cost of the work
//! itself, while the work-stealing deque amortises dispatch nearly to
//! zero. This bench gives the contention curve so future PRs that pick
//! one shape over the other can back the decision with numbers.
//!
//! # What it measures
//!
//! Three dispatch shapes feeding the same synthetic per-item workload
//! (a deterministic FNV-1a fold over a payload-size hint, sized to mimic
//! a small per-file fingerprint operation):
//!
//! 1. `vec.into_par_iter()` - owned collection, work-stealing deque.
//! 2. `vec.par_iter().cloned()` lifted through `par_bridge()` - same
//!    pre-allocated source, but funnelled through the bridge channel so
//!    the channel cost is isolated from any iteration overhead.
//! 3. `(0..ITEMS).map(WorkItem::synth).par_bridge()` - pure generator
//!    iterator with no pre-allocation, the case `par_bridge` exists for.
//!
//! Item count is fixed at `ITEMS = 100_000` to model the receiver's
//! small-file hot path (see `PARALLEL_STAT_THRESHOLD = 64` plus the 100K-
//! file scale in `parallel_stat_collector_contention.rs`). Worker counts
//! sweep `{1, 4, 8, 16}` so the report shows the contention curve crossing
//! each scaling regime.
//!
//! # Expected outcome and the action it informs
//!
//! At T=1 the three shapes converge: there is no contention on the
//! channel and no work to steal, so dispatch is dominated by the per-
//! item closure cost. As T grows, the two `par_bridge` rows should
//! flatten earlier than `into_par_iter` because the channel head becomes
//! a serial bottleneck. The pre-allocated `par_bridge` row isolates the
//! channel cost from generator-iterator state; the generator row is the
//! number that matters when the producer cannot materialise a `Vec`.
//!
//! Action this evidence informs:
//!
//! - If `into_par_iter` wins by > 20% at T >= 8, any new code path that
//!   reaches for `par_bridge` over a stream that can be collected first
//!   must justify why collection is not viable (memory pressure, infinite
//!   stream, etc.). Otherwise prefer `collect().into_par_iter()`.
//!   Relevant to #1284 (io_uring + rayon composition - the SQE producer
//!   is naturally a stream) and #1370 (per-thread fold over the file
//!   list).
//! - If `par_bridge` over a generator stays within 10% of
//!   `into_par_iter`, the convenience of streaming is worth preserving
//!   for follow-ups under #1681 where pre-allocation would double peak
//!   memory.
//! - The crossover point sets the threshold below which neither shape is
//!   worth optimising: at small N the per-item closure dominates and the
//!   choice of dispatch is noise.
//!
//! Run: `cargo bench -p transfer -- par_bridge_vs_deque`

#![deny(unsafe_code)]

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rayon::ThreadPoolBuilder;
use rayon::iter::{IntoParallelIterator, ParallelBridge, ParallelIterator};

/// Item count modelling a 100K small-file transfer. Held constant so the
/// per-worker rows of the criterion report compare apples-to-apples.
const ITEMS: usize = 100_000;

/// Worker counts to sweep: serial baseline, mid-range laptop, desktop,
/// and server-class host. Matches the sweep in
/// `parallel_stat_collector_contention.rs` so the two reports can be read
/// side by side.
const WORKER_COUNTS: &[usize] = &[1, 4, 8, 16];

/// Stand-in for one file's worth of dispatch metadata. Two pointer-sized
/// fields keep the payload representative of `(file_index, byte_size)`
/// without dragging real I/O into the measurement.
#[derive(Clone, Copy)]
struct WorkItem {
    index: u64,
    payload_size: u64,
}

impl WorkItem {
    #[inline]
    fn synth(i: usize) -> Self {
        Self {
            index: i as u64,
            // Vary payload size across items so the per-item loop cannot
            // be folded to a constant by the optimiser.
            payload_size: 64 + (i as u64 & 0x3FF),
        }
    }

    /// Deterministic per-item work: a short FNV-1a fold over the payload
    /// size, sized to roughly match the cost of hashing a 64-byte file
    /// fingerprint. Returns a value the caller can `black_box` so the
    /// closure cannot be optimised away.
    #[inline]
    fn process(self) -> u64 {
        const FNV_OFFSET: u64 = 0xCBF2_9CE4_8422_2325;
        const FNV_PRIME: u64 = 0x0000_0100_0000_01B3;
        let mut hash = FNV_OFFSET ^ self.index;
        let bytes = self.payload_size.to_le_bytes();
        for b in bytes {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }
}

/// Returns a private rayon pool sized to `threads`. A dedicated pool per
/// bench keeps the global pool's worker count from skewing the result.
fn make_pool(threads: usize) -> rayon::ThreadPool {
    ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("bench-dispatch-{i}"))
        .build()
        .expect("failed to build rayon pool")
}

/// `into_par_iter` over an owned `Vec<WorkItem>`. The work-stealing
/// deque shape: each worker drains its local queue, stealing from peers
/// when empty.
fn dispatch_into_par_iter(pool: &rayon::ThreadPool, items: &[WorkItem]) -> u64 {
    pool.install(|| {
        items
            .to_vec()
            .into_par_iter()
            .map(WorkItem::process)
            .reduce(|| 0u64, |a, b| a ^ b)
    })
}

/// `par_bridge` over `items.iter().cloned()`. The source is pre-
/// allocated so the only difference versus `into_par_iter` is the bridge
/// channel; this isolates the channel cost from iterator-state overhead.
fn dispatch_par_bridge_vec(pool: &rayon::ThreadPool, items: &[WorkItem]) -> u64 {
    pool.install(|| {
        items
            .iter()
            .copied()
            .par_bridge()
            .map(WorkItem::process)
            .reduce(|| 0u64, |a, b| a ^ b)
    })
}

/// `par_bridge` over a generator iterator (`(0..ITEMS).map(synth)`). No
/// pre-allocation - the case `par_bridge` is designed for. The generator
/// produces items as the bridge consumes them.
fn dispatch_par_bridge_generator(pool: &rayon::ThreadPool) -> u64 {
    pool.install(|| {
        (0..ITEMS)
            .map(WorkItem::synth)
            .par_bridge()
            .map(WorkItem::process)
            .reduce(|| 0u64, |a, b| a ^ b)
    })
}

/// Registers one bench group with rows for each dispatch shape, parametric
/// on the rayon worker count. Throughput is `Elements(100_000)` so the
/// reader gets items/sec directly off the criterion summary.
fn bench_dispatch(c: &mut Criterion) {
    // Pre-allocate the work-item vector once, outside the timed section.
    // Both `into_par_iter` and the `par_bridge` over a Vec consume a
    // freshly cloned view of this slice so the per-iteration cost of
    // building the source is not charged to the dispatch measurement.
    let items: Vec<WorkItem> = (0..ITEMS).map(WorkItem::synth).collect();

    let mut group = c.benchmark_group("par_bridge_vs_deque");
    group.throughput(Throughput::Elements(ITEMS as u64));
    group.sample_size(20);

    for &threads in WORKER_COUNTS {
        let pool = make_pool(threads);

        group.bench_with_input(
            BenchmarkId::new("into_par_iter_vec", format!("T{threads}")),
            &threads,
            |b, _| b.iter(|| black_box(dispatch_into_par_iter(&pool, &items))),
        );

        group.bench_with_input(
            BenchmarkId::new("par_bridge_vec_iter", format!("T{threads}")),
            &threads,
            |b, _| b.iter(|| black_box(dispatch_par_bridge_vec(&pool, &items))),
        );

        group.bench_with_input(
            BenchmarkId::new("par_bridge_generator", format!("T{threads}")),
            &threads,
            |b, _| b.iter(|| black_box(dispatch_par_bridge_generator(&pool))),
        );
    }

    group.finish();
}

criterion_group!(benches, bench_dispatch);
criterion_main!(benches);
