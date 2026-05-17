//! Parallel-dispatch overhead decomposition benchmark.
//!
//! The production concurrent delta pipeline pays three dispatch-time costs
//! that together gate end-to-end throughput at 100K-file transfers:
//!
//! 1. Thread spawn + join - the `std::thread::spawn` producer / consumer
//!    threads that sit around the rayon worker pool.
//! 2. Channel send / recv - the bounded `crossbeam_channel` work queue.
//! 3. `ReorderBuffer` insert + drain - re-sequences out-of-order worker
//!    completions before downstream consumers see them.
//!
//! This bench isolates each component at the 100K work-item scale so the next
//! round of optimisation has evidence for which one dominates. Each group is
//! deliberately stripped of the other two: no payload work, no rayon pool, no
//! cross-component interaction. The numbers are not meant to predict
//! end-to-end transfer throughput - they answer the narrower question "where
//! is the dispatch budget going?".
//!
//! # Groups
//!
//! ## `thread_spawn_only`
//! Spawn N OS threads, each does the cheapest possible amount of work
//! (`black_box(thread_id)`), then `join` them all. Measures the pure cost of
//! the OS-level thread lifecycle that dispatch pays once per transfer (one
//! producer thread, one consumer thread, plus the rayon pool). Sweeps thread
//! count over {1, 4, 8, 16}.
//!
//! ## `channel_only`
//! Pre-allocates 100K [`DeltaWork`] items, then sends them through the
//! engine's actual [`work_queue::bounded`] (a `crossbeam_channel`) and drains
//! them sequentially via the receiver's `Iterator` impl. No reorder buffer,
//! no parallel workers. Mirrors the role of #4203's `sync_channel_overhead`
//! bench but uses the channel kind production code uses.
//!
//! ## `reorderbuffer_only`
//! Pre-allocates 100K out-of-order sequence numbers, then inserts them into a
//! freshly constructed [`ReorderBuffer`] and drains in order. Single-threaded
//! (no channel, no spawn). Isolates the ring-buffer insert + drain cost from
//! every other dispatch component.
//!
//! # Pre-allocation contract
//!
//! Every group pre-allocates its inputs (work items, sequence vectors)
//! outside the timed section via Criterion's `iter_batched`. The timed
//! section only covers the operation under test. This mirrors the discipline
//! used by `reorder_buffer_cache.rs` and is necessary to keep allocator noise
//! out of the per-component cost figures.
//!
//! # Cross-references
//!
//! - #4180 - `reorder_buffer_cache` bench. Measures cache-residency of the
//!   ring buffer at 1M items with varied payload sizes. Complementary to
//!   `reorderbuffer_only` here: that bench varies payload size to expose
//!   cache pressure, this bench fixes payload at `u64` and isolates dispatch.
//! - #4203 - `sync_channel_overhead` bench. Measures the same dimension as
//!   `channel_only`, but at the `std::sync::mpsc::sync_channel` level. The
//!   two benches together let reviewers see whether `crossbeam_channel`'s
//!   reported speed advantage holds at the engine's actual usage pattern.
//! - #4204 - `ReorderBuffer` memory occupancy bench. Reports `max_depth`
//!   under varied drift windows. Complementary to `reorderbuffer_only`:
//!   that one diagnoses memory pressure, this one diagnoses CPU cost.
//! - #1885 - in-bench metrics observation hooks. The Criterion measurements
//!   here can be paired with [`ReorderBuffer::metrics`] for stall-time
//!   evidence inside a single run.
//!
//! # Action this informs
//!
//! - If `channel_only` dominates - prioritise #1681 (lock-free MPSC); the
//!   bounded `crossbeam_channel` mutex is the bottleneck.
//! - If `thread_spawn_only` dominates - prioritise #1370 (per-thread buffer
//!   pools) and avoid spawning workers per transfer entirely.
//! - If `reorderbuffer_only` dominates - prioritise #1271 (buffer slab) so
//!   inserts can elide the `Box<[Option<T>]>` indirection.
//!
//! # Running
//!
//! ```sh
//! cargo bench -p engine --bench parallel_dispatch_overhead
//! ```
//!
//! Each group reports throughput via `Throughput::Elements(100_000)` so the
//! output prints comparable ops/sec figures across components and thread
//! counts.

#![deny(unsafe_code)]

use std::hint::black_box;
use std::path::PathBuf;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use engine::concurrent_delta::DeltaWork;
use engine::concurrent_delta::ReorderBuffer;
use engine::concurrent_delta::work_queue;

/// Total work items per group iteration. Matches the 100K target referenced
/// in the bench's parent task.
const ITEM_COUNT: usize = 100_000;

/// Thread counts to sweep for the `thread_spawn_only` group. The production
/// pipeline spawns a single producer plus one consumer per transfer, so 1 is
/// the minimum useful figure; 4 / 8 / 16 mirror the rayon pool sizes the
/// dispatch path actually drives.
const THREAD_COUNTS: [usize; 4] = [1, 4, 8, 16];

/// Builds 100K pre-allocated `DeltaWork` items used by the channel group.
///
/// The destination path is created once and cloned per item so allocation
/// cost stays out of the timed section. The size field encodes the sequence
/// number so the receiver can verify ordering cheaply if it ever needs to.
fn build_work_items(count: usize) -> Vec<DeltaWork> {
    let dest = PathBuf::from("/bench/dispatch");
    (0..count as u32)
        .map(|i| DeltaWork::whole_file(i, dest.clone(), u64::from(i)))
        .collect()
}

/// Generates a deterministic shuffled permutation of `0..count` using the
/// same LCG family as the other reorder benches so reviewers can compare
/// numbers directly. Small local windows mirror realistic rayon worker drift.
fn shuffled_sequences(count: usize) -> Vec<u64> {
    let mut seq: Vec<u64> = (0..count as u64).collect();
    let mut state: u64 = 0xA5A5_5A5A_DEAD_BEEF;
    let window = 16usize;
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

/// `thread_spawn_only` - measures pure OS thread lifecycle cost.
///
/// Each cell spawns `threads` OS threads; every thread reads `black_box` on
/// its index, then the main thread joins them all. Reporting throughput as
/// `ITEM_COUNT` keeps the y-axis comparable with the other two groups even
/// though the spawn count is much smaller; the figure to read is the per-cell
/// wall time, with thread count as the swept axis.
fn bench_thread_spawn_only(c: &mut Criterion) {
    let mut group = c.benchmark_group("parallel_dispatch_overhead/thread_spawn_only");
    group.throughput(Throughput::Elements(ITEM_COUNT as u64));
    group.sample_size(20);

    for &threads in &THREAD_COUNTS {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{threads}t")),
            &threads,
            |b, &threads| {
                b.iter(|| {
                    let handles: Vec<_> = (0..threads)
                        .map(|tid| {
                            std::thread::spawn(move || {
                                black_box(tid);
                            })
                        })
                        .collect();
                    for h in handles {
                        h.join().expect("worker thread panicked");
                    }
                });
            },
        );
    }

    group.finish();
}

/// `channel_only` - measures the engine's bounded work-queue send/recv cost.
///
/// Pre-allocates 100K `DeltaWork` items outside the timed section, then sends
/// them through `work_queue::bounded` from a dedicated producer thread and
/// drains them sequentially on the main thread via the receiver's `Iterator`
/// impl. This is the same channel kind the production dispatch uses, but
/// with no reorder buffer and no rayon worker pool.
fn bench_channel_only(c: &mut Criterion) {
    let mut group = c.benchmark_group("parallel_dispatch_overhead/channel_only");
    group.throughput(Throughput::Elements(ITEM_COUNT as u64));
    group.sample_size(15);

    group.bench_with_input(
        BenchmarkId::from_parameter(format!("{ITEM_COUNT}")),
        &ITEM_COUNT,
        |b, _| {
            b.iter_batched(
                || build_work_items(ITEM_COUNT),
                |items| {
                    let (tx, rx) = work_queue::bounded();
                    let producer = std::thread::spawn(move || {
                        for w in items {
                            tx.send(w).expect("receiver dropped unexpectedly");
                        }
                    });
                    let mut received: u64 = 0;
                    for w in rx {
                        received = received.wrapping_add(u64::from(w.ndx().get()));
                    }
                    producer.join().expect("producer thread panicked");
                    black_box(received);
                },
                criterion::BatchSize::PerIteration,
            );
        },
    );

    group.finish();
}

/// `reorderbuffer_only` - measures `ReorderBuffer` insert + drain cost.
///
/// Pre-allocates a 100K-element shuffled sequence outside the timed section,
/// then inserts every entry into a freshly constructed `ReorderBuffer` and
/// drains the contiguous prefix opportunistically (mirrors the production
/// consumer loop). Single-threaded - no channel, no spawn. Capacity is sized
/// to cover the worst-case local shuffle window without `grow()` so the
/// numbers reflect steady-state ring behaviour, not reallocation cost.
fn bench_reorderbuffer_only(c: &mut Criterion) {
    let mut group = c.benchmark_group("parallel_dispatch_overhead/reorderbuffer_only");
    group.throughput(Throughput::Elements(ITEM_COUNT as u64));
    group.sample_size(20);

    // Capacity well above the 16-slot LCG window keeps the fast O(1) path
    // active for every insert and matches `reorder_buffer_scaling`.
    let capacity = 1024usize;
    let order = shuffled_sequences(ITEM_COUNT);

    group.bench_with_input(
        BenchmarkId::from_parameter(format!("cap{capacity}/{ITEM_COUNT}")),
        &order,
        |b, order| {
            b.iter(|| {
                let mut buf: ReorderBuffer<u64> = ReorderBuffer::new(capacity);
                let mut sum: u64 = 0;
                for &seq in order {
                    // Drain ready items first so the ring stays inside the
                    // bounded capacity; on the rare miss force_insert avoids
                    // skewing the cell with a CapacityExceeded retry loop.
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
                black_box(sum);
            });
        },
    );

    group.finish();
}

/// Entry point that runs all three groups. At 100K items every group
/// completes in well under the ~3 second per-iteration budget that the
/// parent task specifies as the gating threshold, so no `#[ignore]`-style
/// env gate is required by default.
fn bench_parallel_dispatch_overhead(c: &mut Criterion) {
    bench_thread_spawn_only(c);
    bench_channel_only(c);
    bench_reorderbuffer_only(c);
}

criterion_group!(benches, bench_parallel_dispatch_overhead);
criterion_main!(benches);
