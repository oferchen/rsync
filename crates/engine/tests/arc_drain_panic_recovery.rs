//! Stress tests for ATU-7: a panic in a receiver-side worker
//! that still holds a clone of one of the shared `Arc` handles must
//! surface as a strongly typed error variant on the drain path,
//! carrying the residual `Arc::strong_count`. Prior to ATU-3
//!, the same failure mode collapsed to an opaque
//! `io::Error::Other("...")` with no machine-readable shape and no
//! strong-count, so operators could neither match on the variant nor
//! see how many workers were still leaking the handle.
//!
//! The drain site exercised is `DeleteContext::emit_one` /
//! `into_emitter`: the typed payload of
//! `DeleteError::PlanMapStillShared { strong_count }` is preserved
//! end-to-end through the `From<DeleteError> for io::Error` boundary.
//!
//! Every test isolates the worker-side panic with
//! [`std::panic::catch_unwind`] so the harness stays green even when
//! the receiver thread is intentionally aborted.

use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;
use std::thread;

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
