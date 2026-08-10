//! Criterion micro-benchmark: drain strategies for the `WorkQueueReceiver`
//! fan-in pattern, parameterised on item count and rayon worker count.
//!
//! # Why this exists
//!
//! `crates/engine/src/concurrent_delta/work_queue/drain.rs::drain_parallel`
//! is the consumer-side fan-in used by the concurrent delta pipeline to
//! aggregate per-file results from N rayon workers back into a single
//! `Vec<R>`. Today it uses a sharded `Vec<Mutex<Vec<R>>>` keyed by
//! `rayon::current_thread_index()` and flattens the shards after the
//! rayon scope completes.
//!
//! The PR #4173 audit flagged this site as one of two remaining
//! production `Mutex<Vec>` collectors. Task #1682 wants the alternatives
//! benchmarked at the 10K and 100K item scales so the team can decide
//! whether to migrate the current sharded mutex to a lock-free shape (the
//! candidate replacements tracked in #1681).
//!
//! # Cross-references
//!
//! - #4170 (`crates/transfer/benches/parallel_stat_collector_contention.rs`)
//!   - The established `Arc<Mutex<Vec>>` collector contention bench. This
//!     file follows the same parameter shape (worker sweep, criterion
//!     throughput in elements/sec) but targets the drain side specifically,
//!     using a `DeltaWork` payload rather than the synthetic stat record.
//! - #4173 - WorkQueueSender / `Mutex<Vec>` audit that names this site.
//! - #4203 - sync-channel overhead reference: the MPSC strategy below
//!   uses `crossbeam_channel::unbounded` for parity with that bench so the
//!   numbers compose.
//!
//! Action this evidence informs:
//!
//! - #1681 - lock-free MPSC drain_parallel replacement. If the MPSC or
//!   per-thread-Vec variant beats the sharded mutex at T=8/16 by a margin
//!   large enough to justify the churn, #1681 picks the winner. If the
//!   sharded mutex is within noise at both sizes, #1681 closes as "no
//!   change warranted, current shape is fine".
//!
//! # What it measures
//!
//! Three fan-in strategies, all driven on the same private rayon pool so
//! the only difference between groups is the collector itself:
//!
//! 1. `sharded_mutex_vec` - `Vec<Mutex<Vec<R>>>` indexed by
//!    `rayon::current_thread_index()`, one rayon task per item. This is
//!    what `drain_parallel` does today; it is the baseline the
//!    alternatives must beat.
//! 2. `per_thread_vec` - one rayon task per worker (not per item) via
//!    `par_chunks(items.len() / threads)`; each task owns a
//!    `Vec<R>` exclusively and returns it. No mutex, no atomic on the
//!    hot path; merge cost paid once at the end of the parallel iterator.
//! 3. `mpsc_unbounded_channel` - `crossbeam_channel::unbounded` MPSC,
//!    one rayon task per item. Each worker `send`s its result; a final
//!    drain loop collects.
//!
//! The two scales (10K, 100K) bracket the realistic concurrent-delta
//! workload: 10K is a mid-size sync, 100K is the upper bound called out
//! in `MEMORY.md` ("Parallel stat: PARALLEL_STAT_THRESHOLD = 64"). The
//! worker sweep (4, 8, 16) matches the rayon pool sizes the dispatch
//! path actually drives in production.
//!
//! Pre-allocated `DeltaWork` items live outside the timed section so the
//! benchmark only captures fan-in cost, not item construction.
//!
//! Run: `cargo bench -p engine --bench drain_parallel_alternatives`

#![deny(unsafe_code)]

use std::hint::black_box;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rayon::ThreadPoolBuilder;
use rayon::prelude::*;

use engine::concurrent_delta::DeltaWork;

/// Item counts to sweep. 10K models a mid-size sync; 100K models the
/// upper end of the concurrent-delta hot path. Same bracket the related
/// `drain_parallel_benchmark` bench uses, so numbers compose.
const ITEM_COUNTS: &[usize] = &[10_000, 100_000];

/// Rayon worker counts to sweep. 4 / 8 / 16 covers the production pool
/// sizes the concurrent delta pipeline drives. Matches the worker bracket
/// in `parallel_stat_collector_contention.rs` (minus T=1, which is not
/// the interesting case for a fan-in collector).
const WORKER_COUNTS: &[usize] = &[4, 8, 16];

/// Stand-in for a per-item drain result. A `u64` keeps the per-element
/// payload small enough that the measurement reflects collector overhead
/// rather than result construction.
type DrainResult = u64;

/// Simulates per-item compute. Kept identical across all three strategies
/// so the only difference between groups is the fan-in collector itself.
#[inline(never)]
fn simulate_work(ndx: u32, size: u64) -> DrainResult {
    let mut hash: u64 = u64::from(ndx);
    for i in 0..16u64 {
        hash = hash
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(size ^ i);
    }
    hash
}

/// Builds `count` pre-allocated `DeltaWork` items. Constructed once per
/// `(count, threads)` cell so allocation cost stays outside the timed
/// section.
fn build_work_items(count: usize) -> Vec<DeltaWork> {
    let dest = PathBuf::from("/bench/drain");
    (0..count as u32)
        .map(|i| DeltaWork::whole_file(i, dest.clone(), u64::from(i)))
        .collect()
}

/// Returns a private rayon pool sized to `threads` so the global pool's
/// worker count cannot skew the measurement.
fn make_pool(threads: usize) -> rayon::ThreadPool {
    ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("bench-drain-{i}"))
        .build()
        .expect("failed to build rayon pool")
}

/// Production-style sharded `Mutex<Vec<R>>` strategy: one mutex-guarded
/// shard per worker, indexed by `rayon::current_thread_index()`, flattened
/// on completion. Mirrors `WorkQueueReceiver::drain_parallel` exactly,
/// minus the bounded `WorkQueue` channel feeding it.
fn drain_sharded_mutex<F>(
    items: &[DeltaWork],
    pool: &rayon::ThreadPool,
    threads: usize,
    f: F,
) -> Vec<DrainResult>
where
    F: Fn(&DeltaWork) -> DrainResult + Send + Sync,
{
    let shards: Vec<Mutex<Vec<DrainResult>>> = (0..threads)
        .map(|_| Mutex::new(Vec::with_capacity(items.len() / threads + 1)))
        .collect();

    pool.scope(|s| {
        for work in items {
            let f = &f;
            let shards = &shards;
            s.spawn(move |_| {
                let result = f(work);
                let idx = rayon::current_thread_index().unwrap_or(0) % threads;
                shards[idx]
                    .lock()
                    .expect("shard mutex poisoned")
                    .push(result);
            });
        }
    });

    shards
        .into_iter()
        .flat_map(|m| m.into_inner().expect("shard mutex poisoned"))
        .collect()
}

/// Per-thread `Vec<R>` accumulators with a final concat. The item slice
/// is split into `threads` contiguous chunks; each chunk is processed by
/// a single rayon task that owns its result `Vec<R>` exclusively. The
/// per-worker `Vec`s are returned via `collect_into_vec` and concatenated
/// once at the end. No mutex, no atomic on the hot path; the only
/// synchronization is the implicit barrier at the end of the parallel
/// iterator.
///
/// This is the "concat-only" lower bound the comparison is asking for:
/// it is what `drain_parallel` could look like if the per-result lock
/// were removed entirely.
fn drain_per_thread_vec<F>(
    items: &[DeltaWork],
    pool: &rayon::ThreadPool,
    threads: usize,
    f: F,
) -> Vec<DrainResult>
where
    F: Fn(&DeltaWork) -> DrainResult + Send + Sync,
{
    // Ceiling division so the last chunk absorbs any remainder. This
    // gives exactly `threads` chunks (or fewer when `items.len() <
    // threads`, which is not a regime this bench exercises).
    let chunk_size = items.len().div_ceil(threads);

    let partials: Vec<Vec<DrainResult>> = pool.install(|| {
        items
            .par_chunks(chunk_size)
            .map(|chunk| {
                let mut local: Vec<DrainResult> = Vec::with_capacity(chunk.len());
                for w in chunk {
                    local.push(f(w));
                }
                local
            })
            .collect()
    });

    let mut out = Vec::with_capacity(items.len());
    for partial in partials {
        out.extend(partial);
    }
    out
}

/// MPSC channel strategy: a single `crossbeam_channel::unbounded` is
/// cloned across all workers. Each worker spawns one task per item and
/// sends its result; a final drain loop collects into the result `Vec`.
fn drain_mpsc_channel<F>(items: &[DeltaWork], pool: &rayon::ThreadPool, f: F) -> Vec<DrainResult>
where
    F: Fn(&DeltaWork) -> DrainResult + Send + Sync,
{
    let (tx, rx) = crossbeam_channel::unbounded::<DrainResult>();
    pool.scope(|s| {
        for work in items {
            let f = &f;
            let tx = tx.clone();
            s.spawn(move |_| {
                let result = f(work);
                tx.send(result).expect("drain channel closed");
            });
        }
    });
    drop(tx);
    let mut out: Vec<DrainResult> = Vec::with_capacity(items.len());
    while let Ok(r) = rx.recv() {
        out.push(r);
    }
    out
}

fn bench_drain_alternatives(c: &mut Criterion) {
    let mut group = c.benchmark_group("drain_parallel_alternatives");
    group.sample_size(15);

    // Pre-allocate items once per item-count so allocation stays outside
    // the timed section.
    let item_sets: Vec<(usize, Arc<Vec<DeltaWork>>)> = ITEM_COUNTS
        .iter()
        .map(|&n| (n, Arc::new(build_work_items(n))))
        .collect();

    for (count, items) in &item_sets {
        let count = *count;
        group.throughput(Throughput::Elements(count as u64));

        for &threads in WORKER_COUNTS {
            let pool = make_pool(threads);

            {
                let items = Arc::clone(items);
                group.bench_with_input(
                    BenchmarkId::new("sharded_mutex_vec", format!("T{threads}/N{count}")),
                    &threads,
                    |b, &t| {
                        b.iter(|| {
                            let out = drain_sharded_mutex(&items, &pool, t, |w| {
                                simulate_work(w.ndx().get(), w.target_size())
                            });
                            debug_assert_eq!(out.len(), count);
                            black_box(out);
                        });
                    },
                );
            }

            {
                let items = Arc::clone(items);
                group.bench_with_input(
                    BenchmarkId::new("per_thread_vec", format!("T{threads}/N{count}")),
                    &threads,
                    |b, &t| {
                        b.iter(|| {
                            let out = drain_per_thread_vec(&items, &pool, t, |w| {
                                simulate_work(w.ndx().get(), w.target_size())
                            });
                            debug_assert_eq!(out.len(), count);
                            black_box(out);
                        });
                    },
                );
            }

            {
                let items = Arc::clone(items);
                group.bench_with_input(
                    BenchmarkId::new("mpsc_unbounded_channel", format!("T{threads}/N{count}")),
                    &threads,
                    |b, _| {
                        b.iter(|| {
                            let out = drain_mpsc_channel(&items, &pool, |w| {
                                simulate_work(w.ndx().get(), w.target_size())
                            });
                            debug_assert_eq!(out.len(), count);
                            black_box(out);
                        });
                    },
                );
            }
        }
    }

    group.finish();
}

criterion_group!(benches, bench_drain_alternatives);
criterion_main!(benches);
