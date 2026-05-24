//! DG-3.e (#2694): 1000-thread x 10K-iter stress for the register ->
//! dispatch -> decrement cycle under the DG-3 SlotData/BarrierState
//! split.
//!
//! DG-3.a..d completed the DG-2 Option B split: `SlotBarrier` became
//! `BarrierState` (notify-only) + `SlotData` (payload), and
//! `DecrementGuard` now holds only `Arc<BarrierState>`.
//! The strong-count trajectories of the two halves are disjoint, so the
//! `finish_file` `Arc::try_unwrap` on `Arc<SlotData>` no longer observes
//! the worker's lingering `Arc<BarrierState>` clone after `notify_all`.
//!
//! This stress test exercises the post-split applier under a workload
//! that *was* the historical reproducer for the release race:
//!
//! 1. Spawn 1000 OS threads (the worker fan-out).
//! 2. Each thread runs 10K independent register/dispatch/finish cycles
//!    on its own per-worker NDX range. Each cycle:
//!    - `register_file(ndx, sink)`
//!    - `apply_one_chunk(literal(ndx, 0, payload))`
//!    - `finish_file(ndx)`
//!    The `finish_file` step is where the DG-1 race used to surface:
//!    `Arc::try_unwrap` on the payload Arc would observe `strong_count
//!    >= 2` because `DecrementGuard::drop` had already fired
//!    `notify_all` but its `Arc<SlotBarrier>` field had not yet dropped.
//!    The DG-3 split moves the notify-bearing Arc off the payload
//!    allocation entirely, so this loop now runs clean on every
//!    platform.
//! 3. After every worker returns, assert:
//!    - every cycle returned `Ok` (no Arc::try_unwrap failure, no
//!      slot-poisoning panic),
//!    - `drain_inflight()` returns `Ok` (no leaked in-flight counters),
//!    - the per-worker sink counter equals the number of completed
//!      cycles x chunk payload size (no dropped or corrupted writes).
//!
//! Memory references:
//! `[[project_concurrent_dispatch_test_flake]]` - prior `expected > 0`
//! flake on Windows in the sibling
//! `concurrent_register_and_dispatch_on_overlapping_files` test, fixed
//! via the SSC-1 registrations_done atomic + yield-loop. This stress
//! test does not need that workaround because every worker registers
//! its own NDX range, so there is no race between registrar and
//! dispatcher threads.
//! `[[project_slothandle_decrementguard_release_race]]` - root-cause
//! audit of the SlotHandle/DecrementGuard release race and the
//! spin-then-yield workaround that DG-3's Option B split superseded.
//!
//! # Feature gating
//!
//! Gated behind the `dg-stress` Cargo feature (which transitively pulls
//! in `parallel-receive-delta` for the applier type). Not part of the
//! standard nextest run because 10M total cycles + 1000 OS threads is
//! too heavy for every PR's lint cell; a dedicated non-required CI cell
//! runs this on Linux + Windows + macOS for any PR touching
//! `parallel_apply/`.

#![cfg(all(feature = "dg-stress", feature = "parallel-receive-delta"))]

use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use engine::concurrent_delta::{DeltaChunk, ParallelDeltaApplier};

/// Worker count - matches the DG-3.e spec.
const WORKERS: u32 = 1_000;
/// Cycles per worker - matches the DG-3.e spec.
const ITERS_PER_WORKER: u32 = 10_000;
/// Payload bytes per cycle. Kept small so the test bookkeeping is
/// trivial and so the 10M-cycle loop stays I/O-cheap.
const CHUNK_BYTES: usize = 8;

/// In-memory sink whose only job is to count bytes. Mirrors the
/// `CountingSink` in `parallel_apply_concurrent.rs` so the two stress
/// tests stay independent.
struct CountingSink {
    written: Arc<AtomicU64>,
}

impl Write for CountingSink {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.written.fetch_add(data.len() as u64, Ordering::Relaxed);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 1000-thread x 10K-iter stress for the register/dispatch/finish
/// cycle. See module docs for the DG-3 race surface this guards.
#[test]
fn concurrent_register_and_dispatch_stress_1000_threads_10k_iter() {
    let applier = Arc::new(ParallelDeltaApplier::new(WORKERS as usize));
    // Per-worker sink counter. Asserted post-join against the number of
    // successful cycles to catch dropped or cross-file writes.
    let sinks: Vec<Arc<AtomicU64>> = (0..WORKERS).map(|_| Arc::new(AtomicU64::new(0))).collect();
    // Per-worker completed-cycle counter. Lets the post-join assertion
    // localise any worker that failed mid-loop without losing the rest
    // of the diagnostic surface.
    let completed: Vec<Arc<AtomicUsize>> = (0..WORKERS)
        .map(|_| Arc::new(AtomicUsize::new(0)))
        .collect();

    let start = Instant::now();
    let handles: Vec<_> = (0..WORKERS)
        .map(|worker| {
            let applier = Arc::clone(&applier);
            let sink_counter = Arc::clone(&sinks[worker as usize]);
            let done_counter = Arc::clone(&completed[worker as usize]);
            std::thread::Builder::new()
                .name(format!("dg3-stress-{worker}"))
                .spawn(move || {
                    // Per-worker NDX range avoids any cross-worker
                    // contention on the DashMap shard during register
                    // and finish; the stress target is the per-slot
                    // Arc graph behaviour, not the outer-map shard
                    // contention exercised by the sibling
                    // `concurrent_files_under_dashmap_shards_match_expected_bytes`.
                    let base = worker * ITERS_PER_WORKER;
                    let payload = vec![worker as u8; CHUNK_BYTES];
                    for iter in 0..ITERS_PER_WORKER {
                        let ndx = base + iter;
                        let sink = CountingSink {
                            written: Arc::clone(&sink_counter),
                        };
                        applier
                            .register_file(ndx, Box::new(sink))
                            .expect("register_file under DG-3 stress");
                        let chunk = DeltaChunk::literal(ndx, 0, payload.clone());
                        applier
                            .apply_one_chunk(chunk)
                            .expect("apply_one_chunk under DG-3 stress");
                        // The release race the DG-3 split fixes lives
                        // inside `finish_file`'s `Arc::try_unwrap` on
                        // the per-slot payload Arc. Before DG-3, this
                        // call returned `ApplierStillReferenced` on
                        // Windows under load because
                        // `DecrementGuard::drop` had already fired
                        // `notify_all` but its `Arc<SlotBarrier>` was
                        // still live. Post DG-3, the notify-bearing
                        // Arc lives on a disjoint allocation and never
                        // blocks the payload unwrap.
                        applier
                            .finish_file(ndx)
                            .expect("finish_file under DG-3 stress");
                        done_counter.fetch_add(1, Ordering::Relaxed);
                    }
                })
                .expect("spawn DG-3 stress worker")
        })
        .collect();

    for handle in handles {
        handle.join().expect("DG-3 stress worker thread");
    }

    let elapsed = start.elapsed();
    let total_cycles = u64::from(WORKERS) * u64::from(ITERS_PER_WORKER);
    let cycles_per_sec = total_cycles as f64 / elapsed.as_secs_f64();
    eprintln!(
        "[parallel_apply_dg3_stress] workers={WORKERS} iters={ITERS_PER_WORKER} \
         total={total_cycles} elapsed={elapsed:?} cycles/sec={cycles_per_sec:.0}"
    );

    // No worker bailed out early. Each must have completed the full
    // 10K iterations or the join above would have surfaced a panic.
    for (worker, done) in completed.iter().enumerate() {
        assert_eq!(
            done.load(Ordering::Relaxed),
            ITERS_PER_WORKER as usize,
            "worker {worker} did not complete all iterations"
        );
    }

    // No bytes were dropped or cross-routed: each worker's sink
    // counter equals iters x payload bytes.
    let expected_per_worker = u64::from(ITERS_PER_WORKER) * CHUNK_BYTES as u64;
    for (worker, sink) in sinks.iter().enumerate() {
        let observed = sink.load(Ordering::Relaxed);
        assert_eq!(
            observed, expected_per_worker,
            "worker {worker} sink byte mismatch: observed={observed} expected={expected_per_worker}"
        );
    }

    // The DG-3 split's "barrier eventually notifies" invariant is
    // already covered by every successful `finish_file` above (which
    // calls `flush_workers` internally), but `drain_inflight` adds a
    // belt-and-braces check that no in-flight counter leaked across
    // the 10M-cycle loop.
    applier
        .drain_inflight()
        .expect("drain_inflight after DG-3 stress");
}
