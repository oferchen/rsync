//! G4 / #2278 - `--delete-delay` event-order parity vs upstream rsync 3.4.1.
//!
//! Asserts that under `--delete-delay`, deletes for a given destination
//! directory only execute after the final temp-file rename for any
//! transferred file in the run. Upstream rsync buffers the delete list with
//! `do_delayed_deletions()` (`delete.c`) and flushes it after the receiver's
//! final commit; oc-rsync's parallel-deterministic emitter must defer its
//! delete batch behind every rename produced by the writer pipeline.
//!
//! Per-directory delete sets are also compared via `diff_delete_groups`.
//!
//! GATE: `OC_RSYNC_DELETE_INTEROP=1`. Will fail on master until DDP-E1-E5
//! lands.

#![cfg(unix)]

mod integration;

use integration::delete_event_order_harness::{
    PairOutcome, Scenario, SyscallKind, diff_delete_groups, first_index, last_index,
    run_pair_capture, skip,
};

#[test]
fn delete_event_order_delay_matches_upstream() {
    let scenario = Scenario::before_after_default();
    let outcome = match run_pair_capture(&scenario, &["--delete-delay"], &[]) {
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

    let group_diffs = diff_delete_groups(&upstream.events, &oc_rsync.events);
    assert!(
        group_diffs.is_empty(),
        "--delete-delay per-directory parity failure:\n{}",
        group_diffs.join("\n")
    );

    for (label, run) in [("upstream", &upstream), ("oc-rsync", &oc_rsync)] {
        let last_rename = last_index(&run.events, |e| matches!(e.kind, SyscallKind::Rename));
        let first_delete = first_index(&run.events, |e| {
            matches!(e.kind, SyscallKind::Unlink | SyscallKind::Rmdir)
        });
        let (Some(lr), Some(fd)) = (last_rename, first_delete) else {
            continue;
        };
        assert!(
            lr < fd,
            "{label}: --delete-delay violated ordering invariant; \
             last rename at idx {lr} followed by first delete at idx {fd}",
        );
    }
}
