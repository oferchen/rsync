//! Stress tests for ATU-7 (#2384): a panic in a receiver-side worker
//! that still holds a clone of one of the shared `Arc` handles must
//! surface as a strongly typed error variant on the drain path,
//! carrying the residual `Arc::strong_count`. Prior to ATU-3
//! (PR #4357), the same failure mode collapsed to an opaque
//! `io::Error::Other("...")` with no machine-readable shape and no
//! strong-count, so operators could neither match on the variant nor
//! see how many workers were still leaking the handle.
//!
//! Two drain sites are exercised:
//!
//! 1. `DeleteContext::emit_one` / `into_emitter` -> the typed payload
//!    of `DeleteError::PlanMapStillShared { strong_count }` is
//!    preserved end-to-end through the `From<DeleteError> for
//!    io::Error` boundary.
//! 2. `ReorderBuffer::insert` capacity overflow -> the producer's
//!    panic does not collapse the consumer's drain into an opaque
//!    error: the `CapacityExceeded` typed error continues to surface
//!    with its descriptive `Display`.
//!
//! Note: a prior third test exercised
//! `ParallelDeltaApplier::finish_file`'s `ApplierStillReferenced`
//! variant by holding the per-file slot inside a blocking writer while
//! the main thread raced the drain. FFB-2 (the `flush_workers` /
//! `drain_inflight` barrier API) bakes the barrier into `finish_file`
//! itself, so that scenario is now a deadlock by construction rather
//! than an observable race. The variant remains in the public API as a
//! post-barrier invariant assertion; the Display payload is exercised
//! by the unit tests in `crates/engine/src/concurrent_delta/parallel_apply.rs`.
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
// Test 2: ReorderBuffer drain returns typed CapacityExceeded, not Other.
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
