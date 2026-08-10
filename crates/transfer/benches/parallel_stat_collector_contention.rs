//! Criterion micro-benchmark: collector contention under the parallel-stat
//! fan-in pattern, parameterised on item count and rayon worker count.
//!
//! # Why this exists
//!
//! `crates/transfer/src/parallel_io.rs::map_blocking` is the rayon-backed
//! parallel-stat dispatcher used by the receiver and generator when the
//! file list crosses `ParallelThresholds::stat` (default 64 entries). It
//! currently collects results via `into_par_iter().map(f).collect()`,
//! which delegates to rayon's lock-free split-and-merge reducer.
//!
//! Several issues (#1192, #1271, #1297, #1370, #1682) ask whether moving
//! to (or back to) a shared `Arc<Mutex<Vec<R>>>` collector would simplify
//! the code, and how much throughput that would cost at 100K+ files on
//! 8/16-core hosts. The static audit in
//! `docs/audits/arc-mutex-vec-parallel-stat-contention.md` answers the
//! "where is it used today" question; this bench answers the matching
//! "how much would a regression cost" question with numbers, so future
//! reviewers can reject naive refactors with evidence rather than memory.
//!
//! # What it measures
//!
//! Per-item collector cost for four append strategies when N rayon workers
//! each push N_items / N records into a shared sink:
//!
//! 1. `Arc<Mutex<Vec<R>>>` - one shared mutex, the worst-case baseline.
//! 2. Sharded `Vec<Mutex<Vec<R>>>` indexed by `rayon::current_thread_index()`
//!    - matches the `WorkQueueReceiver::drain_parallel` pattern.
//! 3. `crossbeam_queue::SegQueue<R>` - lock-free, unbounded baseline.
//! 4. `crossbeam_channel::unbounded()` MPSC - the other common drop-in.
//!
//! Item count is fixed at 100K to model the receiver hot path; worker
//! counts sweep `{1, 4, 8, 16}` so the report shows the contention curve
//! crossing each scaling regime.
//!
//! # Expected outcome and the action it informs
//!
//! At T=1 all four shapes converge: the shared `Mutex<Vec>` wins by a
//! small margin because there is no atomic or queue node overhead at all.
//! As T grows, the single `Arc<Mutex<Vec>>` is expected to flatten (or
//! regress) past T=4-8 once the per-item critical section saturates the
//! mutex. The sharded `Mutex<Vec>` and lock-free queues should keep
//! scaling close to linear.
//!
//! Action this evidence informs:
//!
//! - If `Arc<Mutex<Vec>>` collapses at T=8 with a delta > ~20% versus
//!   the sharded variant, any future PR that proposes a shared mutex on
//!   the parallel-stat collector path must include a measurement that
//!   shows why this bench does not apply to it. (e.g., the lock is held
//!   for a few cycles or the collector is hit < 1k times per transfer.)
//! - If the crossover stays above T=16, the shared mutex remains a
//!   viable simplification for any new collector under 100K items.
//! - The numbers form the baseline for the lock-free replacement work
//!   tracked in #1271 (sharded queue) and #1370 (per-thread fold).
//!
//! Run: `cargo bench -p transfer -- parallel_stat_collector_contention`

#![deny(unsafe_code)]

use std::hint::black_box;
use std::sync::{Arc, Mutex};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use crossbeam_queue::SegQueue;
use rayon::ThreadPoolBuilder;

/// Item count modelling the receiver's 100K-file parallel-stat workload.
///
/// 100K matches the upper end of the hot path described in
/// `docs/audits/arc-mutex-vec-contention.md` and `MEMORY.md` ("Parallel
/// stat: `PARALLEL_STAT_THRESHOLD = 64`"). Keeping this constant rather
/// than a parameter lets the per-thread-count rows of the criterion
/// report be compared apples-to-apples.
const ITEMS: usize = 100_000;

/// Worker counts to sweep: covers serial baseline, mid-range laptops,
/// and high-core server hosts. The Mac Studio M2 Ultra reference machine
/// in the audit doc uses 16+ rayon threads, which is the upper bound here.
const WORKER_COUNTS: &[usize] = &[1, 4, 8, 16];

/// Stand-in for a stat result. Two pointer-sized fields keep the per-item
/// payload representative of `(PathBuf-like, file_size)` without dragging
/// in real allocations that would dominate the measurement.
#[derive(Clone, Copy)]
#[allow(dead_code)]
struct StatRecord {
    index: u64,
    size: u64,
}

impl StatRecord {
    #[inline]
    fn synth(i: usize) -> Self {
        Self {
            index: i as u64,
            // Touch a derived field so the compiler cannot fold the loop
            // to a constant store.
            size: (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15),
        }
    }
}

/// Returns a rayon pool sized to `threads`. We build a private pool per
/// bench so the global pool's worker count cannot skew the measurement.
fn make_pool(threads: usize) -> rayon::ThreadPool {
    ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("bench-collector-{i}"))
        .build()
        .expect("failed to build rayon pool")
}

/// Drives `body` from `threads` workers, partitioning `ITEMS` evenly. The
/// last worker absorbs any remainder so the total push count is always
/// exactly `ITEMS`.
fn fan_out<F>(pool: &rayon::ThreadPool, threads: usize, body: F)
where
    F: Fn(usize, usize) + Sync,
{
    pool.scope(|s| {
        let per_worker = ITEMS / threads;
        let remainder = ITEMS % threads;
        for w in 0..threads {
            let count = if w == threads - 1 {
                per_worker + remainder
            } else {
                per_worker
            };
            let body = &body;
            s.spawn(move |_| body(w, count));
        }
    });
}

/// Baseline: every worker pushes into a single shared `Mutex<Vec<R>>`.
/// This is the shape #1192 asks us to characterise.
fn shared_mutex(pool: &rayon::ThreadPool, threads: usize) -> usize {
    let sink: Arc<Mutex<Vec<StatRecord>>> = Arc::new(Mutex::new(Vec::with_capacity(ITEMS)));
    fan_out(pool, threads, |worker, count| {
        let base = worker * (ITEMS / threads);
        for i in 0..count {
            let rec = StatRecord::synth(base + i);
            let mut guard = sink.lock().expect("collector mutex poisoned");
            guard.push(rec);
        }
    });
    let len = sink.lock().expect("collector mutex poisoned").len();
    black_box(len)
}

/// Sharded `Mutex<Vec<R>>` indexed by rayon worker id, matching
/// `WorkQueueReceiver::drain_parallel`. Contention drops to the cost of
/// one mutex per worker.
fn sharded_mutex(pool: &rayon::ThreadPool, threads: usize) -> usize {
    let shards: Arc<Vec<Mutex<Vec<StatRecord>>>> = Arc::new(
        (0..threads)
            .map(|_| Mutex::new(Vec::with_capacity(ITEMS / threads + 1)))
            .collect(),
    );
    fan_out(pool, threads, |worker, count| {
        let shard_idx = rayon::current_thread_index().unwrap_or(worker) % threads;
        let base = worker * (ITEMS / threads);
        for i in 0..count {
            let rec = StatRecord::synth(base + i);
            let mut guard = shards[shard_idx].lock().expect("shard mutex poisoned");
            guard.push(rec);
        }
    });
    let total: usize = shards
        .iter()
        .map(|m| m.lock().expect("shard mutex poisoned").len())
        .sum();
    black_box(total)
}

/// Lock-free unbounded queue. Stand-in for any `SegQueue`-backed sink.
fn seg_queue(pool: &rayon::ThreadPool, threads: usize) -> usize {
    let queue: Arc<SegQueue<StatRecord>> = Arc::new(SegQueue::new());
    fan_out(pool, threads, |worker, count| {
        let base = worker * (ITEMS / threads);
        for i in 0..count {
            queue.push(StatRecord::synth(base + i));
        }
    });
    let mut drained = 0usize;
    while queue.pop().is_some() {
        drained += 1;
    }
    black_box(drained)
}

/// `crossbeam_channel::unbounded` MPSC. Different lock-free design
/// (Chase-Lev-style deque inside crossbeam) than `SegQueue`; included so
/// both common drop-ins are present in the report.
fn unbounded_channel(pool: &rayon::ThreadPool, threads: usize) -> usize {
    let (tx, rx) = crossbeam_channel::unbounded::<StatRecord>();
    fan_out(pool, threads, |worker, count| {
        let base = worker * (ITEMS / threads);
        for i in 0..count {
            tx.send(StatRecord::synth(base + i))
                .expect("collector channel closed");
        }
    });
    drop(tx);
    let mut drained = 0usize;
    while rx.recv().is_ok() {
        drained += 1;
    }
    black_box(drained)
}

/// Registers one bench group per collector shape, parametric on the rayon
/// worker count. Throughput is reported in elements/sec so the reader can
/// read "items/sec" directly off the criterion summary.
fn bench_collectors(c: &mut Criterion) {
    let mut group = c.benchmark_group("parallel_stat_collector_contention");
    group.throughput(Throughput::Elements(ITEMS as u64));
    group.sample_size(20);

    for &threads in WORKER_COUNTS {
        let pool = make_pool(threads);

        group.bench_with_input(
            BenchmarkId::new("arc_mutex_vec", format!("T{threads}")),
            &threads,
            |b, &t| b.iter(|| shared_mutex(&pool, t)),
        );

        group.bench_with_input(
            BenchmarkId::new("sharded_mutex_vec", format!("T{threads}")),
            &threads,
            |b, &t| b.iter(|| sharded_mutex(&pool, t)),
        );

        group.bench_with_input(
            BenchmarkId::new("crossbeam_seg_queue", format!("T{threads}")),
            &threads,
            |b, &t| b.iter(|| seg_queue(&pool, t)),
        );

        group.bench_with_input(
            BenchmarkId::new("crossbeam_unbounded_channel", format!("T{threads}")),
            &threads,
            |b, &t| b.iter(|| unbounded_channel(&pool, t)),
        );
    }

    group.finish();
}

criterion_group!(benches, bench_collectors);
criterion_main!(benches);
