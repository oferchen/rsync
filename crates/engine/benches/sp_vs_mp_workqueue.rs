//! Single-producer vs multi-producer `WorkQueue` overhead benchmark (#1572).
//!
//! Quantifies the cost difference between the default single-producer (SP)
//! [`WorkQueueSender`] path and the optional multi-producer (MP) path that is
//! gated behind the `multi-producer` cargo feature in `crates/engine/Cargo.toml`
//! (`multi-producer = []`, declared at the `[features]` table). The MP group is
//! compiled out of the default build and only participates when the bench is
//! invoked with `--features multi-producer`; this preserves the compile-time
//! single-producer invariant for production binaries.
//!
//! # What the result informs
//!
//! Per the audit at `docs/audits/workqueue-sender-multi-producer-audit.md`
//! (PR #4173), every live production producer site is correctly
//! single-producer today and the audit recommends keeping the `multi-producer`
//! feature gated. This bench produces the quantitative evidence behind that
//! recommendation:
//!
//! - If MP throughput at 4 producers is within noise of SP (<5% delta), the
//!   feature can be promoted to a default-on capability with no measurable
//!   regression for the SP-only callers that exist today.
//! - If MP throughput is materially worse (>15% slower) at the matched 100K
//!   item count, the feature stays opt-in. New callers that genuinely need
//!   fan-in must accept the documented overhead.
//! - If MP throughput exceeds SP by >=15%, the feature is a strict win for
//!   any future fan-in caller and the gate is purely a compile-time
//!   single-producer enforcement convenience.
//!
//! The decision criteria mirror the buffer-pool sharded-benchmark gate
//! (`docs/audits/bufferpool-sharded-benchmark-plan.md`), keeping the project's
//! "speedup must clear noise plus measurement variance" convention consistent
//! across optimisation decisions.
//!
//! # Cross-references
//!
//! - PR #4173 - `WorkQueueSender` multi-producer usage audit
//!   (`docs/audits/workqueue-sender-multi-producer-audit.md`). Documents the
//!   feature-gate shape this bench exercises.
//! - #4203 - `sync_channel_overhead` bench. Measures the same channel
//!   dimension as the SP group here but against `std::sync::mpsc::sync_channel`
//!   instead of `crossbeam_channel::bounded`. Together they let reviewers see
//!   whether the channel backend, not the producer count, dominates cost.
//! - #4206 - `parallel_dispatch_overhead` bench
//!   (`crates/engine/benches/parallel_dispatch_overhead.rs`). Decomposes
//!   dispatch into thread spawn / channel / reorder-buffer at 100K items.
//!   The `channel_only` group there is the single-producer baseline this
//!   bench's SP group is calibrated against.
//!
//! # Groups
//!
//! ## `sp/1p/100k`
//! One producer thread pushes `TOTAL_ITEMS` pre-allocated `DeltaWork` items
//! into the queue. A single drain thread consumes them sequentially via the
//! receiver's `Iterator` impl. This is the default-build production path.
//!
//! ## `mp/4p/100k` (feature `multi-producer`)
//! Four producer threads each push `TOTAL_ITEMS / 4` pre-allocated `DeltaWork`
//! items into a shared queue via the gated `Clone` impl on `WorkQueueSender`.
//! A single drain thread consumes the merged stream. Capacity is the same as
//! the SP group so backpressure characteristics match.
//!
//! # Pre-allocation contract
//!
//! Every group pre-allocates its `DeltaWork` items outside the timed section
//! via Criterion's `iter_batched`. The timed section only covers channel
//! construction, producer thread spawn, send / receive, and producer join.
//! This mirrors the discipline used by `parallel_dispatch_overhead.rs` and is
//! necessary to keep allocator noise out of the per-component cost figures.
//!
//! # Throughput reporting
//!
//! Both groups report `Throughput::Elements(TOTAL_ITEMS as u64)` so the
//! Criterion output prints comparable items/sec figures regardless of how the
//! work is split across producers. Per the task spec, `TOTAL_ITEMS = 100_000`.
//!
//! # Ignore gate
//!
//! `MP_PRODUCERS * (TOTAL_ITEMS / MP_PRODUCERS)` equals `TOTAL_ITEMS`; the SP
//! and MP groups both move the same total amount of work. On contemporary
//! development hardware each iteration completes in well under the ~3 second
//! per-iteration budget called out in the parent task, so no `#[ignore]` gate
//! is required. If a future platform regresses past that budget, gate the
//! affected group via an env-var check (mirroring the pattern used by other
//! engine benches) rather than disabling the bench entirely.
//!
//! # Running
//!
//! ```sh
//! # SP group only (default build).
//! cargo bench -p engine --bench sp_vs_mp_workqueue
//!
//! # SP and MP groups together.
//! cargo bench -p engine --features multi-producer --bench sp_vs_mp_workqueue
//! ```

#![deny(unsafe_code)]

use std::hint::black_box;
use std::path::PathBuf;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use engine::concurrent_delta::DeltaWork;
use engine::concurrent_delta::work_queue;

/// Total work items moved per iteration. Matches the 100K figure called out
/// in task #1572 and the `parallel_dispatch_overhead` baseline (#4206) so
/// numbers are directly comparable across benches.
const TOTAL_ITEMS: usize = 100_000;

/// Producer thread count for the MP group. Matches the integration-test fan-in
/// count at `crates/engine/tests/multi_producer_work_queue.rs` and the audit's
/// "4 producers, capacity-1 to capacity-16" coverage matrix.
#[cfg(feature = "multi-producer")]
const MP_PRODUCERS: usize = 4;

/// Items each MP producer sends. `TOTAL_ITEMS / MP_PRODUCERS` keeps the total
/// offered work identical to the SP group, so throughput numbers compare
/// directly without normalisation.
#[cfg(feature = "multi-producer")]
const MP_ITEMS_PER_PRODUCER: usize = TOTAL_ITEMS / MP_PRODUCERS;

/// Bounded-channel capacity. Fixed across both groups so the capacity axis does
/// not confound the producer-count axis. Matches the default that
/// `work_queue::bounded()` would pick on a typical 8-core machine
/// (`2 * rayon::current_num_threads()`).
const CHANNEL_CAPACITY: usize = 16;

/// Builds `count` pre-allocated `DeltaWork` items used by both groups.
///
/// The destination path is constructed once and cloned per item so allocation
/// cost stays out of the timed section. The size field encodes the index so
/// the consumer can verify completeness cheaply if it ever needs to.
fn build_work_items(count: usize) -> Vec<DeltaWork> {
    let dest = PathBuf::from("/bench/sp_vs_mp");
    (0..count as u32)
        .map(|i| DeltaWork::whole_file(i, dest.clone(), u64::from(i)))
        .collect()
}

/// SP group: one producer thread pushes `TOTAL_ITEMS` items; a single drain
/// thread consumes them sequentially via the receiver's `Iterator` impl.
///
/// This is the default-build production path: `WorkQueueSender` is `Send` and
/// not `Clone`, the producer owns the only sender, and dropping the sender
/// after the last send closes the channel and lets the iterator reach EOF.
fn bench_sp(c: &mut Criterion) {
    let mut group = c.benchmark_group("sp_vs_mp_workqueue/sp");
    group.throughput(Throughput::Elements(TOTAL_ITEMS as u64));
    group.sample_size(15);

    group.bench_with_input(
        BenchmarkId::from_parameter(format!("1p/{TOTAL_ITEMS}")),
        &TOTAL_ITEMS,
        |b, _| {
            b.iter_batched(
                || build_work_items(TOTAL_ITEMS),
                |items| {
                    let (tx, rx) = work_queue::bounded_with_capacity(CHANNEL_CAPACITY);

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

/// MP group: `MP_PRODUCERS` producer threads each push `MP_ITEMS_PER_PRODUCER`
/// items via the gated `Clone` impl on `WorkQueueSender`. A single drain
/// thread consumes the merged stream.
///
/// Pre-allocated items are split into per-producer chunks outside the timed
/// section. The original `tx` is dropped after spawning all producers so the
/// channel closes once every cloned sender drops, mirroring the integration
/// test at `crates/engine/tests/multi_producer_work_queue.rs:68-69`.
#[cfg(feature = "multi-producer")]
fn bench_mp(c: &mut Criterion) {
    let mut group = c.benchmark_group("sp_vs_mp_workqueue/mp");
    group.throughput(Throughput::Elements(TOTAL_ITEMS as u64));
    group.sample_size(15);

    group.bench_with_input(
        BenchmarkId::from_parameter(format!("{MP_PRODUCERS}p/{TOTAL_ITEMS}")),
        &TOTAL_ITEMS,
        |b, _| {
            b.iter_batched(
                || {
                    // Pre-split into per-producer chunks so the timed section
                    // only pays for send / recv, not for slicing the input.
                    let all = build_work_items(TOTAL_ITEMS);
                    let mut chunks: Vec<Vec<DeltaWork>> = (0..MP_PRODUCERS)
                        .map(|_| Vec::with_capacity(MP_ITEMS_PER_PRODUCER))
                        .collect();
                    for (i, item) in all.into_iter().enumerate() {
                        chunks[i % MP_PRODUCERS].push(item);
                    }
                    chunks
                },
                |chunks| {
                    let (tx, rx) = work_queue::bounded_with_capacity(CHANNEL_CAPACITY);

                    let producers: Vec<_> = chunks
                        .into_iter()
                        .map(|chunk| {
                            let sender = tx.clone();
                            std::thread::spawn(move || {
                                for w in chunk {
                                    sender.send(w).expect("receiver dropped unexpectedly");
                                }
                            })
                        })
                        .collect();

                    // Drop the original sender so the channel closes when all
                    // cloned senders finish. Matches the shutdown protocol in
                    // the MP integration test at
                    // `crates/engine/tests/multi_producer_work_queue.rs:68-69`.
                    drop(tx);

                    let mut received: u64 = 0;
                    for w in rx {
                        received = received.wrapping_add(u64::from(w.ndx().get()));
                    }
                    for p in producers {
                        p.join().expect("producer thread panicked");
                    }
                    black_box(received);
                },
                criterion::BatchSize::PerIteration,
            );
        },
    );

    group.finish();
}

/// Entry point. The MP group only participates when the `multi-producer`
/// feature is enabled at build time; the default build runs the SP group
/// alone, mirroring the production-binary feature shape.
fn bench_sp_vs_mp_workqueue(c: &mut Criterion) {
    bench_sp(c);
    #[cfg(feature = "multi-producer")]
    bench_mp(c);
}

criterion_group!(benches, bench_sp_vs_mp_workqueue);
criterion_main!(benches);
