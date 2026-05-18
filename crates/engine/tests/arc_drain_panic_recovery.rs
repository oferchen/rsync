//! Stress tests for ATU-7 (#2384): a panic in a receiver-side worker
//! that still holds a clone of one of the shared `Arc` handles must
//! surface as a strongly typed error variant on the drain path,
//! carrying the residual `Arc::strong_count`. Prior to ATU-3
//! (PR #4357), the same failure mode collapsed to an opaque
//! `io::Error::Other("...")` with no machine-readable shape and no
//! strong-count, so operators could neither match on the variant nor
//! see how many workers were still leaking the handle.
//!
//! Three drain sites are exercised:
//!
//! 1. `DeleteContext::emit_one` / `into_emitter` -> the typed payload
//!    of `DeleteError::PlanMapStillShared { strong_count }` is
//!    preserved end-to-end through the `From<DeleteError> for
//!    io::Error` boundary.
//! 2. `ParallelDeltaApplier::finish_file` -> the typed payload of
//!    `ParallelApplyError::ApplierStillReferenced { ndx, strong_count,
//!    kind }` is preserved end-to-end through `From<ParallelApplyError>
//!    for io::Error`.
//! 3. `ReorderBuffer::insert` capacity overflow -> the producer's
//!    panic does not collapse the consumer's drain into an opaque
//!    error: the `CapacityExceeded` typed error continues to surface
//!    with its descriptive `Display`.
//!
//! Every test isolates the worker-side panic with
//! [`std::panic::catch_unwind`] so the harness stays green even when
//! the receiver thread is intentionally aborted.

use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use engine::delete::{DeleteContext, DeletePlanMap, RecordingDeleteFs};

/// Parses the `strong_count=N` token out of a typed error's `Display`
/// payload. Returns `None` when the token is absent or non-numeric so
/// the assertion site can report the offending message verbatim.
fn extract_strong_count(message: &str) -> Option<usize> {
    message
        .split("strong_count=")
        .nth(1)
        .and_then(|tail| tail.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|digits| digits.parse().ok())
}

// ---------------------------------------------------------------------------
// Test 1: DeleteContext drain surfaces typed PlanMapStillShared on panic.
// ---------------------------------------------------------------------------

/// A panicking receiver-side worker holds a clone of the
/// [`DeletePlanMap`] [`Arc`]. The main thread tries to drain the
/// [`DeleteContext`] and must see the typed
/// `DeleteError::PlanMapStillShared { strong_count >= 2 }` payload
/// rather than an opaque `io::Error::Other` with no diagnostics.
#[test]
fn delete_context_drain_surfaces_typed_plan_map_still_shared() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plans = Arc::new(DeletePlanMap::new());
    let ctx =
        DeleteContext::with_shared_plan_map(Arc::clone(&plans), tmp.path().to_path_buf(), true);

    // Spawn a "receiver" thread that takes a clone of the plan map,
    // leaks it via `mem::forget`, and then panics. The leak models the
    // worst-case ATU-3 failure mode: a panicking worker that is unable
    // to drop its `Arc` clone during stack unwind (for example because
    // it was parked inside a third-party callback). The `catch_unwind`
    // contains the panic so the test harness does not abort.
    let plans_for_thread = Arc::clone(&plans);
    let join = thread::spawn(move || {
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            std::mem::forget(Arc::clone(&plans_for_thread));
            panic!("simulated receiver-thread panic with leaked Arc clone");
        }));
        assert!(result.is_err(), "receiver thread must have panicked");
    });
    join.join().expect("receiver thread join");

    // Main thread now performs the drain. With the leaked clone still
    // outstanding, `into_emitter` (called via the public `emit_one`)
    // must surface the typed `DeleteError::PlanMapStillShared` payload.
    let err = ctx
        .emit_one(RecordingDeleteFs::new())
        .expect_err("plan map still shared must surface as drain error");

    // Boundary: typed error is wrapped in io::Error::other, but the
    // typed `Display` payload survives intact. Critically, the error
    // is NOT a bare opaque `Other` with a generic message - the typed
    // shape from ATU-3 is preserved.
    assert_eq!(err.kind(), io::ErrorKind::Other);
    let msg = err.to_string();
    assert!(
        msg.contains("DeleteContext::into_emitter"),
        "expected typed DeleteContext::into_emitter prefix, got: {msg}"
    );
    assert!(
        msg.contains("DeletePlanMap still shared"),
        "expected DeletePlanMap-still-shared variant, got: {msg}"
    );
    let strong_count = extract_strong_count(&msg)
        .unwrap_or_else(|| panic!("strong_count token missing in: {msg}"));
    assert!(
        strong_count >= 2,
        "expected strong_count >= 2, got {strong_count} (msg: {msg})"
    );

    // Hold the original `plans` clone to the end so the leaked clone
    // is not the only strong reference at the failure site.
    drop(plans);
}

// ---------------------------------------------------------------------------
// Test 2: ParallelDeltaApplier::finish_file surfaces ApplierStillReferenced.
// ---------------------------------------------------------------------------
//
// Only compiled when the `parallel-receive-delta` feature is enabled
// (see `crates/engine/Cargo.toml`). CI runs the engine crate with
// `--all-features`, so this test is exercised on every PR.

#[cfg(feature = "parallel-receive-delta")]
mod parallel_apply_drain {
    use super::{Arc, AssertUnwindSafe, Duration, extract_strong_count, mpsc, panic, thread};
    use std::io::{self, Write};

    use engine::concurrent_delta::{DeltaChunk, ParallelDeltaApplier};

    /// A [`Write`] adaptor that signals "I am inside the per-file slot
    /// lock" via `started_tx`, then blocks on `release_rx` so the main
    /// thread can race the drain against an outstanding worker. The
    /// blocking semantics let the test deterministically catch the
    /// applier mid-flight while the slot's inner `Arc<Mutex<FileSlot>>`
    /// still has a live worker-side clone.
    struct BlockingWriter {
        started_tx: Option<mpsc::Sender<()>>,
        release_rx: mpsc::Receiver<()>,
    }

    impl Write for BlockingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if let Some(tx) = self.started_tx.take() {
                let _ = tx.send(());
            }
            // Block until the main thread has had a chance to attempt
            // `finish_file`. Bounded so a regression cannot wedge the
            // test indefinitely.
            let _ = self.release_rx.recv_timeout(Duration::from_secs(10));
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A panicking apply-side worker keeps the per-file slot's inner
    /// `Arc<Mutex<FileSlot>>` live while the main thread races
    /// `finish_file`. The drain must surface
    /// `ParallelApplyError::ApplierStillReferenced { ndx, strong_count
    /// >= 2, kind }` instead of an opaque `io::Error::Other`.
    #[test]
    fn parallel_applier_finish_file_surfaces_typed_applier_still_referenced() {
        let applier = Arc::new(ParallelDeltaApplier::new(1));
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let writer = BlockingWriter {
            started_tx: Some(started_tx),
            release_rx,
        };
        applier
            .register_file(0u32, Box::new(writer))
            .expect("register_file");

        // Worker: applies a chunk; the writer blocks inside the
        // per-file slot lock so the worker is still holding the inner
        // `Arc<Mutex<FileSlot>>` clone at the moment the main thread
        // tries the drain. Wrap the call in `catch_unwind` so any
        // accidental panic from the worker side does not abort the
        // harness.
        let worker_applier = Arc::clone(&applier);
        let worker = thread::spawn(move || {
            let result = panic::catch_unwind(AssertUnwindSafe(|| {
                worker_applier
                    .apply_chunk_parallel(DeltaChunk::literal(0u32, 0, vec![0u8; 16]))
                    .expect("apply_chunk_parallel");
            }));
            // Bubble up the panic outcome so the join site can assert
            // it stayed contained. The drain assertion already ran on
            // the main thread.
            result.is_err()
        });

        // Wait for the worker to enter `BlockingWriter::write` - by
        // that point the slot's inner Arc has a live clone held inside
        // `apply_chunk_parallel`, so `finish_file`'s `Arc::try_unwrap`
        // will fail with the typed variant.
        started_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("worker entered writer");

        let err = match applier.finish_file(0u32) {
            Ok(_) => panic!("slot still referenced by mid-flight worker"),
            Err(err) => err,
        };

        // Boundary: typed error survives the `From<ParallelApplyError>
        // for io::Error` conversion. The `Display` payload carries
        // `ndx`, `strong_count`, and the call-site tag - none of which
        // were available in the pre-ATU-3 opaque `Other` form.
        assert_eq!(err.kind(), io::ErrorKind::Other);
        let msg = err.to_string();
        assert!(
            msg.contains("ParallelDeltaApplier::finish_file"),
            "expected ParallelDeltaApplier::finish_file prefix, got: {msg}"
        );
        assert!(
            msg.contains("file slot still referenced"),
            "expected typed still-referenced variant, got: {msg}"
        );
        assert!(
            msg.contains("ndx=0"),
            "expected ndx=0 in typed message, got: {msg}"
        );
        let strong_count = extract_strong_count(&msg)
            .unwrap_or_else(|| panic!("strong_count token missing in: {msg}"));
        assert!(
            strong_count >= 2,
            "expected strong_count >= 2, got {strong_count} (msg: {msg})"
        );

        // Release the writer so the worker thread can finish cleanly.
        let _ = release_tx.send(());
        let _ = release_tx.send(());
        let panicked = worker.join().expect("worker join");
        assert!(
            !panicked,
            "worker should not have panicked once the writer was released"
        );
    }
}

// ---------------------------------------------------------------------------
// Test 3: ReorderBuffer drain returns typed CapacityExceeded, not Other.
// ---------------------------------------------------------------------------

/// A panicking producer pushes items into a shared
/// [`engine::concurrent_delta::ReorderBuffer`] until capacity is
/// exhausted, then panics. The consumer thread drains what is
/// available and verifies the next insert attempt returns the typed
/// `CapacityExceeded` error rather than a bare `io::Error::Other`. The
/// typed variant is what the ATU-3 audit demanded for every shared-Arc
/// drain site.
#[test]
fn reorder_buffer_capacity_exceeded_surfaces_typed_error_after_producer_panic() {
    use engine::concurrent_delta::ReorderBuffer;

    // Capacity 4: addressable sequences relative to next_expected=0
    // are offsets 0..=3, i.e. seqs 0..=3. Inserting seq 4 with no
    // drain has offset 4 and triggers CapacityExceeded.
    let buffer: Arc<Mutex<ReorderBuffer<u64>>> = Arc::new(Mutex::new(ReorderBuffer::new(4)));
    let (panic_tx, panic_rx) = mpsc::channel::<()>();

    // Producer: inserts seqs 1..=3 (leaving seq 0 absent so nothing
    // drains automatically), signals it has filled the addressable
    // out-of-order region, then panics. `catch_unwind` keeps the
    // panic contained so the test runner sees a clean exit.
    let producer_buf = Arc::clone(&buffer);
    let producer = thread::spawn(move || {
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            {
                let mut guard = producer_buf.lock().expect("buffer lock");
                for seq in 1u64..=3 {
                    guard.insert(seq, seq).expect("insert within capacity");
                }
            }
            panic_tx.send(()).expect("notify main");
            panic!("simulated producer panic after filling reorder buffer");
        }));
        assert!(outcome.is_err(), "producer must have panicked");
    });

    // Wait for the producer to finish filling before it panics.
    panic_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("producer signalled full buffer");
    producer.join().expect("producer thread join");

    // The producer's panic must not poison the typed-error contract:
    // the next addressable-overflow `insert` still returns the typed
    // `CapacityExceeded` (with its descriptive `Display`) rather than
    // a bare opaque error.
    let mut guard = buffer.lock().expect("buffer lock after producer panic");
    let err = guard
        .insert(4, 4)
        .expect_err("offset 4 exceeds capacity 4 after producer panic");
    let display = err.to_string();
    assert!(
        display.contains("reorder buffer capacity exceeded"),
        "expected typed CapacityExceeded Display, got: {display}"
    );

    // And the consumer-side drain still observes the items the
    // producer managed to insert before it panicked: the typed error
    // path does not corrupt the buffer's internal state.
    //
    // Sequences started at 1 (not 0) so `next_in_order` returns None
    // until we advance `next_expected` by inserting seq 0.
    assert!(
        guard.next_in_order().is_none(),
        "head sequence 0 not yet seen"
    );
    guard.insert(0, 0).expect("insert seq 0 to advance drain");
    let mut drained: Vec<u64> = Vec::new();
    while let Some(v) = guard.next_in_order() {
        drained.push(v);
    }
    assert_eq!(
        drained,
        vec![0, 1, 2, 3],
        "consumer must still drain producer's pre-panic inserts in order"
    );
    drop(guard);
}
