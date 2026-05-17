//! G1 / #2275 - `--delete-during` event-order parity vs upstream rsync 3.4.1.
//!
//! Asserts that oc-rsync's per-directory `unlink`/`rmdir` burst sequence
//! matches upstream's when running with `--delete-during`. Upstream rsync's
//! `delete.c:delete_in_dir()` emits all deletes for a directory in one
//! contiguous batch as the receiver enters that directory; this test
//! validates that oc-rsync's parallel-deterministic delete pipeline preserves
//! the same per-directory grouping (cross-directory interleaving is allowed
//! because oc-rsync runs the pipeline concurrently).
//!
//! Scenario: source missing 3 files in `/a`, 2 in `/a/x`, 1 in `/b`. Both
//! source and destination keep one or more files that survive the sync so
//! that creates run alongside deletes.
//!
//! GATE: opts in via `OC_RSYNC_DELETE_INTEROP=1`. Will fail on master until
//! DDP-E1-E5 wires the parallel-deterministic delete emitter.

#![cfg(unix)]

mod integration;

use integration::delete_event_order_harness::{
    PairOutcome, Scenario, diff_delete_groups, run_pair_capture, skip,
};

#[test]
fn delete_event_order_during_matches_upstream() {
    let scenario = Scenario::during_default();
    let outcome = match run_pair_capture(&scenario, &["--delete-during"], &[]) {
        Ok(o) => o,
        Err(e) => panic!("harness failure: {e}"),
    };
    let (upstream, oc_rsync) = match outcome {
        PairOutcome::Skipped(reason) => {
            skip(&reason);
            return;
        }
        PairOutcome::Captured { upstream, oc_rsync } => (upstream, oc_rsync),
    };

    let diffs = diff_delete_groups(&upstream.events, &oc_rsync.events);
    assert!(
        diffs.is_empty(),
        "--delete-during per-directory parity failure:\n{}",
        diffs.join("\n")
    );
}
