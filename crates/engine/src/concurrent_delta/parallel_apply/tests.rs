//! Unit and property tests for the parallel receive-side delta applier.
//!
//! Relocated verbatim from `parallel_apply/mod.rs` as part of the module
//! decomposition. Pulls the applier surface in through `super::*` (the
//! hub's public + `pub(in parallel_apply)` re-exports) and names the
//! remaining dependencies explicitly so the suite stays compilable from
//! its own file.

use std::collections::HashMap;
use std::io::{self, Cursor, Write};
use std::sync::{Arc, Mutex};

use checksums::strong::strategy::{
    ChecksumAlgorithmKind, ChecksumDigest, ChecksumStrategy, ChecksumStrategySelector,
};
use proptest::prelude::*;

use super::super::types::FileNdx;
use super::*;

/// In-memory sink that records every byte written so tests can compare
/// parallel vs sequential output.
pub(super) struct VecSink {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl VecSink {
    pub(super) fn new() -> (Self, Arc<Mutex<Vec<u8>>>) {
        let buf = Arc::new(Mutex::new(Vec::new()));
        (Self { buf: buf.clone() }, buf)
    }
}

impl Write for VecSink {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut guard = self.buf.lock().expect("sink mutex poisoned");
        guard.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn sequential_apply(chunks: &[DeltaChunk]) -> Vec<u8> {
    let mut by_file: HashMap<FileNdx, Vec<&DeltaChunk>> = HashMap::new();
    for c in chunks {
        by_file.entry(c.ndx).or_default().push(c);
    }
    let mut ndxs: Vec<FileNdx> = by_file.keys().copied().collect();
    ndxs.sort();
    let mut out = Vec::new();
    for ndx in ndxs {
        let mut per_file = by_file.remove(&ndx).expect("present");
        per_file.sort_by_key(|c| c.chunk_sequence);
        for c in per_file {
            out.extend_from_slice(&c.data);
        }
    }
    out
}

pub(super) fn collect_file(
    applier: &ParallelDeltaApplier,
    ndx: FileNdx,
    buf: Arc<Mutex<Vec<u8>>>,
) -> Vec<u8> {
    let _writer = applier.finish_file(ndx).expect("finish_file");
    buf.lock().expect("sink mutex").clone()
}

#[test]
fn single_file_in_order_matches_sequential() {
    let applier = ParallelDeltaApplier::new(2);
    let (sink, buf) = VecSink::new();
    applier.register_file(0u32, Box::new(sink)).unwrap();

    let chunks: Vec<DeltaChunk> = (0..16)
        .map(|i| DeltaChunk::literal(0u32, i, vec![i as u8; 8]))
        .collect();
    let expected = sequential_apply(&chunks);

    for c in chunks {
        applier.apply_one_chunk(c).unwrap();
    }
    assert_eq!(collect_file(&applier, FileNdx::new(0), buf), expected);
}

#[test]
fn single_file_out_of_order_preserves_byte_order() {
    let applier = ParallelDeltaApplier::new(4);
    let (sink, buf) = VecSink::new();
    applier.register_file(0u32, Box::new(sink)).unwrap();

    let chunks: Vec<DeltaChunk> = (0..32)
        .map(|i| DeltaChunk::literal(0u32, i, vec![i as u8; 4]))
        .collect();
    let expected = sequential_apply(&chunks);

    let mut shuffled = chunks.clone();
    // Deterministic non-trivial permutation: rotate by 7.
    shuffled.rotate_left(7);

    for c in shuffled {
        applier.apply_one_chunk(c).unwrap();
    }
    assert_eq!(collect_file(&applier, FileNdx::new(0), buf), expected);
}

#[test]
fn missing_file_registration_errors() {
    let applier = ParallelDeltaApplier::new(1);
    let err = applier
        .apply_one_chunk(DeltaChunk::literal(7u32, 0, vec![1, 2, 3]))
        .unwrap_err();
    assert!(err.to_string().contains("unknown"));
}

#[test]
fn double_registration_errors() {
    let applier = ParallelDeltaApplier::new(1);
    let (sink_a, _) = VecSink::new();
    let (sink_b, _) = VecSink::new();
    applier.register_file(3u32, Box::new(sink_a)).unwrap();
    let err = applier.register_file(3u32, Box::new(sink_b)).unwrap_err();
    assert!(err.to_string().contains("already registered"));
}

#[test]
fn finish_file_with_pending_chunks_errors() {
    let applier = ParallelDeltaApplier::new(1);
    let (sink, _) = VecSink::new();
    applier.register_file(0u32, Box::new(sink)).unwrap();

    // Submit out-of-order chunk; sequence 0 never arrives.
    applier
        .apply_one_chunk(DeltaChunk::literal(0u32, 1, vec![0u8; 4]))
        .unwrap();
    match applier.finish_file(0u32) {
        Ok(_) => panic!("finish_file should fail with pending chunks"),
        Err(e) => assert!(e.to_string().contains("still buffered")),
    }
}

#[test]
fn bytes_written_tracks_in_order_writes() {
    let applier = ParallelDeltaApplier::new(2);
    let (sink, _) = VecSink::new();
    applier.register_file(0u32, Box::new(sink)).unwrap();
    applier
        .apply_one_chunk(DeltaChunk::literal(0u32, 0, vec![1u8; 100]))
        .unwrap();
    assert_eq!(applier.bytes_written(0u32).unwrap(), 100);
    applier
        .apply_one_chunk(DeltaChunk::literal(0u32, 1, vec![2u8; 50]))
        .unwrap();
    assert_eq!(applier.bytes_written(0u32).unwrap(), 150);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn random_chunk_sizes_and_permutations_match_sequential(
        sizes in prop::collection::vec(1usize..=64usize, 1..=48),
        seed in 0u64..512,
    ) {
        let chunks: Vec<DeltaChunk> = sizes
            .iter()
            .enumerate()
            .map(|(i, &len)| {
                let payload: Vec<u8> = (0..len)
                    .map(|b| ((i as u64 ^ seed ^ b as u64) & 0xff) as u8)
                    .collect();
                DeltaChunk::literal(0u32, i as u64, payload)
            })
            .collect();
        let expected = sequential_apply(&chunks);

        // Permute deterministically by `seed` to simulate parallel-completion order.
        let mut order: Vec<usize> = (0..chunks.len()).collect();
        // Fisher-Yates with a small xorshift seeded by `seed`.
        let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        for i in (1..order.len()).rev() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let j = (state as usize) % (i + 1);
            order.swap(i, j);
        }
        let permuted: Vec<DeltaChunk> = order.into_iter().map(|i| chunks[i].clone()).collect();

        let applier = ParallelDeltaApplier::new(((seed % 8) + 1) as usize);
        let (sink, buf) = VecSink::new();
        applier.register_file(0u32, Box::new(sink)).unwrap();
        for c in permuted {
            applier.apply_one_chunk(c).unwrap();
        }
        let actual = collect_file(&applier, FileNdx::new(0), buf);
        prop_assert_eq!(actual, expected);
    }
}

#[test]
fn cursor_writer_round_trip() {
    // Smoke test that the trait-object writer wraps anything `Write + Send`.
    let applier = ParallelDeltaApplier::new(1);
    let cursor: Cursor<Vec<u8>> = Cursor::new(Vec::new());
    applier.register_file(0u32, Box::new(cursor)).unwrap();
    applier
        .apply_one_chunk(DeltaChunk::literal(0u32, 0, vec![9u8; 32]))
        .unwrap();
    let _writer = applier.finish_file(0u32).unwrap();
}

#[test]
fn flush_workers_returns_immediately_when_no_inflight() {
    // FFB-2: with no apply calls outstanding, `flush_workers` must
    // observe zero in-flight handles and return without blocking.
    let applier = ParallelDeltaApplier::new(1);
    let (sink, _) = VecSink::new();
    applier.register_file(0u32, Box::new(sink)).unwrap();
    let start = std::time::Instant::now();
    applier.flush_workers(0u32).expect("flush_workers");
    // Generous bound so loaded CI hosts do not flake; the call must
    // be effectively instant because the inflight counter starts at
    // zero and no worker is registered.
    assert!(
        start.elapsed() < std::time::Duration::from_millis(50),
        "flush_workers should not block when nothing is in flight"
    );
}

#[test]
fn flush_workers_returns_ok_for_unknown_ndx() {
    // FFB-2: absent slot is the same observable outcome as
    // fully-drained slot; the API contract is "wait until idle", and
    // a slot that does not exist is idle by definition.
    let applier = ParallelDeltaApplier::new(1);
    applier.flush_workers(99u32).expect("no-op flush_workers");
}

#[test]
fn flush_workers_blocks_until_worker_drops_arc() {
    // FFB-2: a worker thread holds a SlotHandle clone for ~50ms;
    // flush_workers must not return until the handle drops. Uses
    // raw `slot_for` to exercise the barrier directly without going
    // through `apply_one_chunk` (which internally bounds the
    // handle lifetime to the call itself).
    let applier = Arc::new(ParallelDeltaApplier::new(1));
    let (sink, _) = VecSink::new();
    applier.register_file(0u32, Box::new(sink)).unwrap();

    let (acquired_tx, acquired_rx) = std::sync::mpsc::channel::<()>();
    let worker_applier = Arc::clone(&applier);
    let hold_duration = std::time::Duration::from_millis(50);
    let worker = std::thread::spawn(move || {
        let handle = worker_applier
            .slot_for(FileNdx::new(0))
            .expect("slot registered");
        acquired_tx.send(()).expect("handshake send");
        std::thread::sleep(hold_duration);
        drop(handle);
    });

    // Wait for the worker to acquire its handle deterministically.
    // The sleep-based barrier raced on macOS nightly when the OS
    // didn't schedule the worker before the main thread started the
    // timer, causing flush_workers to return immediately (inflight=0)
    // and the elapsed-time assertion to fire.
    acquired_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("worker acquired handle");

    let start = std::time::Instant::now();
    applier.flush_workers(0u32).expect("flush_workers");
    let elapsed = start.elapsed();
    worker.join().expect("worker thread");
    assert!(
        elapsed >= std::time::Duration::from_millis(40),
        "flush_workers returned too early: {elapsed:?}"
    );
}

#[test]
fn drain_inflight_drains_all_files() {
    // FFB-2: register N files, hand a SlotHandle clone out to a
    // worker per file, call drain_inflight, assert it blocks until
    // every worker drops its handle.
    const N: u32 = 6;
    let applier = Arc::new(ParallelDeltaApplier::new(2));
    for ndx in 0..N {
        let (sink, _) = VecSink::new();
        applier.register_file(ndx, Box::new(sink)).unwrap();
    }

    let hold_duration = std::time::Duration::from_millis(40);
    let (acquired_tx, acquired_rx) = std::sync::mpsc::channel::<()>();
    let mut handles = Vec::with_capacity(N as usize);
    for ndx in 0..N {
        let worker_applier = Arc::clone(&applier);
        let acquired_tx = acquired_tx.clone();
        handles.push(std::thread::spawn(move || {
            let handle = worker_applier
                .slot_for(FileNdx::new(ndx))
                .expect("slot registered");
            acquired_tx.send(()).expect("handshake send");
            std::thread::sleep(hold_duration);
            drop(handle);
        }));
    }
    drop(acquired_tx);

    // Wait for every worker to grab its handle before the drain call.
    // Replaces a sleep-based barrier that raced on macOS where workers
    // had not yet entered slot_for when drain_inflight was invoked.
    for _ in 0..N {
        acquired_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("worker acquired handle");
    }

    let start = std::time::Instant::now();
    applier.drain_inflight().expect("drain_inflight");
    let elapsed = start.elapsed();
    for h in handles {
        h.join().expect("worker thread");
    }
    assert!(
        elapsed >= std::time::Duration::from_millis(30),
        "drain_inflight returned before workers dropped: {elapsed:?}"
    );
}

#[test]
fn finish_file_calls_flush_workers_internally() {
    // FFB-2 Option D: finish_file bakes the barrier in. A worker
    // that holds a SlotHandle clone for a bounded duration must not
    // cause finish_file to return ApplierStillReferenced; instead
    // finish_file blocks until the worker drops the handle, then
    // succeeds.
    //
    // The handshake replaces the previous sleep-based "let the
    // worker get going" coordination, which raced on macOS where
    // the main thread reached finish_file before the worker had
    // acquired the SlotHandle (inflight stayed 0, the barrier
    // returned immediately, and the elapsed-time assertion fired).
    let applier = Arc::new(ParallelDeltaApplier::new(1));
    let (sink, _) = VecSink::new();
    applier.register_file(0u32, Box::new(sink)).unwrap();

    let (acquired_tx, acquired_rx) = std::sync::mpsc::channel::<()>();
    let worker_applier = Arc::clone(&applier);
    let worker = std::thread::spawn(move || {
        let handle = worker_applier
            .slot_for(FileNdx::new(0))
            .expect("slot registered");
        acquired_tx.send(()).expect("handshake send");
        std::thread::sleep(std::time::Duration::from_millis(40));
        drop(handle);
    });
    acquired_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("worker acquired handle");

    let start = std::time::Instant::now();
    let _writer = applier.finish_file(0u32).expect("finish_file");
    let elapsed = start.elapsed();
    worker.join().expect("worker thread");
    assert!(
        elapsed >= std::time::Duration::from_millis(30),
        "finish_file returned before worker dropped: {elapsed:?}"
    );
}

#[test]
fn finish_file_payload_arc_uncontended_after_worker_drop() {
    // DG-3.d: After DG-3.c retyped `DecrementGuard` to
    // `Arc<BarrierState>`, the worker's lingering decrement-guard
    // clone no longer touches the payload Arc that `finish_file`
    // calls `Arc::try_unwrap` on. Verify the strong-count trajectory
    // claim from the DG-2.a spec section 3 at the actual try_unwrap
    // call site: once the worker has fully released its
    // `SlotHandle`, the entry's `Arc<SlotData>` has strong count 1
    // (DashMap only) and `try_unwrap` would succeed deterministically
    // without spinning.
    //
    // Establishing this fact in a test is the precondition for DG-4
    // (removing the spin-then-yield workaround in
    // `drain.rs::finish_file`). If a future change re-introduces a
    // payload-Arc clone on the worker's drop path, this test fails
    // before the spin can mask the regression.
    let applier = Arc::new(ParallelDeltaApplier::new(1));
    let (sink, _) = VecSink::new();
    applier.register_file(0u32, Box::new(sink)).unwrap();

    // Synchronisation: worker signals `acquired` after it owns the
    // SlotHandle, then blocks on `release_rx` so the main thread
    // controls when the handle drops. After `release_tx` fires the
    // worker drops the handle and signals `dropped` so the main
    // thread can observe the post-drop strong count deterministically
    // (no time-based barrier).
    let (acquired_tx, acquired_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let (dropped_tx, dropped_rx) = std::sync::mpsc::channel::<()>();
    let worker_applier = Arc::clone(&applier);
    let worker = std::thread::spawn(move || {
        let handle = worker_applier
            .slot_for(FileNdx::new(0))
            .expect("slot registered");
        acquired_tx.send(()).expect("acquired handshake");
        release_rx.recv().expect("release handshake");
        drop(handle);
        dropped_tx.send(()).expect("dropped handshake");
    });

    // Wait until the worker owns the SlotHandle. While it is held the
    // payload Arc strong count is at least 2 (DashMap + the handle's
    // `data` clone) and would block `try_unwrap`.
    acquired_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("worker acquired SlotHandle");
    let while_held = applier
        .files
        .get(&FileNdx::new(0))
        .map(|guard| Arc::strong_count(&guard.value().data))
        .expect("slot present");
    assert!(
        while_held >= 2,
        "payload Arc strong count while worker holds handle must be >= 2, got {while_held}"
    );

    // Release the worker, then wait for the drop to fully retire the
    // SlotHandle (including the DecrementGuard's drop body). The
    // dropped_tx send happens after `drop(handle)` returns, so by
    // the time we receive on `dropped_rx` every field of the handle
    // - including the payload Arc clone the handle held and the
    // bookkeeping Arc the DecrementGuard carried - has been released.
    release_tx.send(()).expect("release send");
    dropped_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("worker dropped SlotHandle");
    worker.join().expect("worker thread");

    // DG-3.c invariant: after the worker drops, the only remaining
    // `Arc<SlotData>` clone is the one owned by the DashMap. The
    // DecrementGuard's `Arc<BarrierState>` clone is irrelevant here
    // - it lives on a disjoint allocation per the Option-B split in
    // `docs/design/dg-2a-option-b-spec.md` section 3.
    let after_drop = applier
        .files
        .get(&FileNdx::new(0))
        .map(|guard| Arc::strong_count(&guard.value().data))
        .expect("slot present");
    assert_eq!(
        after_drop, 1,
        "payload Arc strong count after worker drop must be 1 (DashMap only), got {after_drop}"
    );

    // finish_file removes the entry from the DashMap and runs
    // `Arc::try_unwrap` on the payload Arc. With strong_count==1 the
    // unwrap succeeds on the first attempt; the spin-then-yield
    // workaround in drain.rs is uncontended on this path.
    let _writer = applier.finish_file(0u32).expect("finish_file");
}

#[test]
fn finish_file_payload_arc_uncontended_under_burst() {
    // DG-3.d: stress variant. Drive many short-lived SlotHandles
    // serially through the same file (mirroring the receiver
    // pipeline's per-chunk dispatch) and verify the post-drop
    // payload Arc strong count returns to 1 after every burst. A
    // regression that re-introduces a payload-Arc clone on the
    // worker drop path leaves a non-1 count visible here even
    // before finish_file runs.
    let applier = Arc::new(ParallelDeltaApplier::new(2));
    let (sink, _) = VecSink::new();
    applier.register_file(0u32, Box::new(sink)).unwrap();

    for sequence in 0..32u64 {
        applier
            .apply_one_chunk(DeltaChunk::literal(0u32, sequence, vec![sequence as u8; 8]))
            .expect("apply_one_chunk");
        // After apply_one_chunk returns the SlotHandle has fully
        // dropped (it was a local in apply_one_chunk's body). The
        // payload Arc should be back to DashMap-only.
        let count = applier
            .files
            .get(&FileNdx::new(0))
            .map(|guard| Arc::strong_count(&guard.value().data))
            .expect("slot present");
        assert_eq!(
            count, 1,
            "payload Arc strong count after chunk {sequence} should be 1, got {count}"
        );
    }

    // finish_file completes without hitting the spin loop's
    // strong-count>1 branch.
    let _writer = applier.finish_file(0u32).expect("finish_file");
}

#[test]
fn finish_file_payload_and_barrier_arcs_are_distinct_allocations() {
    // DG-3.d: structural witness that the DG-2.a Option-B split is
    // intact. The entry's `data: Arc<SlotData>` and
    // `barrier: Arc<BarrierState>` Arcs point at different
    // allocations - if a future refactor collapses them back into
    // one (e.g. by re-introducing a combined struct behind a single
    // Arc) the wakeup-before-drop race documented in DG-1 returns
    // and the spin-then-yield workaround in drain.rs becomes
    // load-bearing again.
    let applier = ParallelDeltaApplier::new(1);
    let (sink, _) = VecSink::new();
    applier.register_file(0u32, Box::new(sink)).unwrap();
    let entry = applier.files.get(&FileNdx::new(0)).expect("slot present");
    let data_addr = Arc::as_ptr(&entry.value().data).addr();
    let barrier_addr = Arc::as_ptr(&entry.value().barrier).addr();
    assert_ne!(
        data_addr, barrier_addr,
        "SlotEntry.data and SlotEntry.barrier must point at distinct allocations \
             so the worker's DecrementGuard drop cannot extend the payload Arc's strong count"
    );
}

#[test]
fn flush_workers_survives_spurious_wakeup() {
    // Condvars are permitted to wake spuriously; the wait_while
    // predicate in `BarrierState::wait_until_idle` must re-check
    // under the mutex and continue waiting. We exercise the
    // predicate by notifying the slot's condvar manually while the
    // inflight counter is still > 0, then verifying flush_workers
    // only returns once the counter actually reaches zero. The
    // AtomicBool gate proves the flusher did not exit until the
    // handle drop fired the real (non-spurious) decrement.
    use std::sync::atomic::{AtomicBool, Ordering};

    let applier = Arc::new(ParallelDeltaApplier::new(1));
    let (sink, _) = VecSink::new();
    applier.register_file(0u32, Box::new(sink)).unwrap();

    // Grab a handle to bump inflight to 1, then arrange for spurious
    // notifications to land while flush_workers is waiting.
    let handle = applier.slot_for(FileNdx::new(0)).expect("slot registered");

    // Snapshot the inner `Arc<BarrierState>` so a sibling thread can
    // notify the slot's Condvar without going through the apply
    // path. DG-3.b stores `SlotEntry` in the DashMap; the test
    // reads `entry.barrier` (the bookkeeping Arc) and pokes the
    // shared Condvar through it.
    let barrier = applier
        .files
        .get(&FileNdx::new(0))
        .map(|guard| Arc::clone(&guard.value().barrier))
        .expect("slot present");

    let notifier_barrier = Arc::clone(&barrier);
    let notifier = std::thread::spawn(move || {
        for _ in 0..5 {
            std::thread::sleep(std::time::Duration::from_millis(8));
            notifier_barrier.notify.notify_all();
        }
    });

    // Tracks whether the flusher returned before we released the
    // handle. If the wait predicate was wrong and a spurious wakeup
    // shipped through `wait_while`, the flusher would join before
    // `released` flipped to true.
    let released = Arc::new(AtomicBool::new(false));
    let released_for_flusher = Arc::clone(&released);
    let flush_applier = Arc::clone(&applier);
    let flusher = std::thread::spawn(move || {
        flush_applier.flush_workers(0u32).expect("flush_workers");
        assert!(
            released_for_flusher.load(Ordering::SeqCst),
            "flush_workers returned before the slot handle was released - \
                 spurious wakeup escaped the wait_while predicate"
        );
    });

    // Let the notifier fire several spurious wakeups, then release
    // the handle so the predicate finally evaluates to false.
    std::thread::sleep(std::time::Duration::from_millis(60));
    released.store(true, Ordering::SeqCst);
    drop(handle);

    notifier.join().expect("notifier thread");
    flusher.join().expect("flusher thread");
}

#[test]
fn parallel_apply_error_display_carries_ndx_and_strong_count() {
    let err = ParallelApplyError::ApplierStillReferenced {
        ndx: FileNdx::new(7),
        strong_count: 3,
        kind: "finish_file",
    };
    let msg = err.to_string();
    assert!(msg.contains("finish_file"));
    assert!(msg.contains("ndx=7"));
    assert!(msg.contains("strong_count=3"));
}

#[test]
fn parallel_apply_error_converts_into_io_error_with_typed_message() {
    let err: io::Error = ParallelApplyError::SlotPoisoned {
        ndx: FileNdx::new(2),
        kind: "finish_file",
    }
    .into();
    assert_eq!(err.kind(), io::ErrorKind::Other);
    let msg = err.to_string();
    assert!(msg.contains("poisoned"));
    assert!(msg.contains("ndx=2"));
}

#[test]
fn new_defaults_strategy_to_md5() {
    // BR-3i.b: `new(concurrency)` must default to the protocol >= 30
    // fallback (MD5) so existing test/bench callers keep working without
    // observing a behaviour change.
    let applier = ParallelDeltaApplier::new(1);
    assert_eq!(
        applier.strategy().algorithm_kind(),
        ChecksumAlgorithmKind::Md5
    );
    assert_eq!(applier.strategy().digest_len(), 16);
}

#[test]
fn with_strategy_threads_negotiated_algorithm() {
    // BR-3i.b: `with_strategy(concurrency, strategy)` is the constructor
    // the receiver pipeline will use once the negotiated algorithm is
    // wired in. Verify it stores and exposes the supplied trait object.
    let strategy: Arc<dyn ChecksumStrategy> = Arc::from(ChecksumStrategySelector::for_algorithm(
        ChecksumAlgorithmKind::Xxh3,
        0,
    ));
    let applier = ParallelDeltaApplier::with_strategy(4, Arc::clone(&strategy));
    assert_eq!(
        applier.strategy().algorithm_kind(),
        ChecksumAlgorithmKind::Xxh3
    );
    // The applier shares the strategy by Arc, so cheap clones reach
    // rayon workers without re-boxing.
    assert!(Arc::ptr_eq(applier.strategy(), &strategy));
}

#[test]
fn unverified_chunk_preserves_writer_byte_stream() {
    // BR-3i.c: when a chunk carries no `expected_strong`, the applier
    // still computes a digest (so the parallel verify path has stable
    // CPU cost) but skips comparison, leaving the writer byte stream
    // unchanged. Backward-compatible callers must keep working.
    let strategy: Arc<dyn ChecksumStrategy> = Arc::from(ChecksumStrategySelector::for_algorithm(
        ChecksumAlgorithmKind::Xxh3,
        0,
    ));
    let applier = ParallelDeltaApplier::with_strategy(1, strategy);
    let (sink, buf) = VecSink::new();
    applier.register_file(0u32, Box::new(sink)).unwrap();
    applier
        .apply_one_chunk(DeltaChunk::literal(0u32, 0, vec![0xAB; 64]))
        .unwrap();
    let _writer = applier.finish_file(0u32).unwrap();
    assert_eq!(buf.lock().unwrap().len(), 64);
}

/// Helper: builds a chunk whose `expected_strong` matches the digest
/// the supplied strategy will compute over `data`. Used by the BR-3i.c
/// happy-path tests so the fixture stays in lockstep with the
/// negotiated algorithm.
pub(super) fn chunk_with_correct_digest(
    strategy: &dyn ChecksumStrategy,
    ndx: u32,
    sequence: u64,
    data: Vec<u8>,
) -> DeltaChunk {
    let digest = strategy.compute(&data);
    DeltaChunk::literal(ndx, sequence, data).with_expected_strong(digest)
}

#[test]
fn verify_chunk_accepts_matching_digest_md5() {
    // BR-3i.c happy path: MD5 (protocol >= 30 fallback) chunk with the
    // correct expected digest applies cleanly and writes the original
    // bytes to the sink.
    let applier = ParallelDeltaApplier::new(1);
    let strategy = Arc::clone(applier.strategy());
    let (sink, buf) = VecSink::new();
    applier.register_file(0u32, Box::new(sink)).unwrap();
    let chunk = chunk_with_correct_digest(strategy.as_ref(), 0, 0, vec![0x42; 128]);
    applier.apply_one_chunk(chunk).unwrap();
    let _writer = applier.finish_file(0u32).unwrap();
    assert_eq!(buf.lock().unwrap().len(), 128);
    assert!(buf.lock().unwrap().iter().all(|&b| b == 0x42));
}

#[test]
fn verify_chunk_accepts_matching_digest_xxh3() {
    // BR-3i.c happy path under the XXH3 negotiated algorithm: confirms
    // the dispatch routes through the configured strategy, not a
    // hard-coded MD5 path.
    let strategy: Arc<dyn ChecksumStrategy> = Arc::from(ChecksumStrategySelector::for_algorithm(
        ChecksumAlgorithmKind::Xxh3,
        0,
    ));
    let applier = ParallelDeltaApplier::with_strategy(2, Arc::clone(&strategy));
    let (sink, buf) = VecSink::new();
    applier.register_file(0u32, Box::new(sink)).unwrap();
    let chunk = chunk_with_correct_digest(strategy.as_ref(), 0, 0, vec![0xAA; 200]);
    applier.apply_one_chunk(chunk).unwrap();
    let _writer = applier.finish_file(0u32).unwrap();
    assert_eq!(buf.lock().unwrap().len(), 200);
}

#[test]
fn verify_chunk_rejects_mismatched_digest_and_does_not_write() {
    // BR-3i.c error path: a chunk with a deliberately wrong expected
    // digest must fail verification, surface the typed
    // `ChecksumMismatch`, and never reach the destination writer.
    let applier = ParallelDeltaApplier::new(1);
    let (sink, buf) = VecSink::new();
    applier.register_file(0u32, Box::new(sink)).unwrap();
    // Bogus expected digest: all-zero MD5 (16 bytes) will not match any
    // non-empty payload's real digest.
    let bogus = ChecksumDigest::new(&[0u8; 16]);
    let chunk = DeltaChunk::literal(0u32, 0, vec![0x99; 64]).with_expected_strong(bogus);
    let err = applier.apply_one_chunk(chunk).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("checksum mismatch"), "msg was: {msg}");
    assert!(msg.contains("ndx=0"), "msg was: {msg}");
    assert!(msg.contains("sequence=0"), "msg was: {msg}");
    assert!(msg.contains("algorithm=md5"), "msg was: {msg}");
    // The writer must remain untouched: the verify failure happens
    // before the per-file mutex is taken.
    assert!(buf.lock().unwrap().is_empty());
}

#[test]
fn checksum_mismatch_error_converts_into_io_error_with_typed_message() {
    let err: io::Error = ParallelApplyError::ChecksumMismatch {
        ndx: FileNdx::new(9),
        chunk_sequence: 42,
        algorithm: ChecksumAlgorithmKind::Md5,
        expected: "deadbeef".to_string(),
        actual: "cafef00d".to_string(),
    }
    .into();
    assert_eq!(err.kind(), io::ErrorKind::Other);
    let msg = err.to_string();
    assert!(msg.contains("checksum mismatch"));
    assert!(msg.contains("ndx=9"));
    assert!(msg.contains("sequence=42"));
    assert!(msg.contains("algorithm=md5"));
    assert!(msg.contains("expected=deadbeef"));
    assert!(msg.contains("actual=cafef00d"));
}
