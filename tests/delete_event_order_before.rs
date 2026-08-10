//! G2 / #2276 - `--delete-before` event-order parity vs upstream rsync 3.4.1.
//!
//! Asserts that every `unlink`/`rmdir` emitted under `--delete-before`
//! precedes every `openat(O_CREAT)` for newly-transferred files. Upstream
//! rsync calls `do_delete_pass()` before any transfer work in this mode
//! (`generator.c`); oc-rsync's parallel-deterministic emitter must preserve
//! that strict ordering even though its internal pipeline is concurrent.
//!
//! Also re-uses `diff_delete_groups` to assert the per-directory delete sets
//! match upstream's.
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
fn delete_event_order_before_matches_upstream() {
    let scenario = Scenario::before_after_default();
    let outcome = match run_pair_capture(&scenario, &["--delete-before"], &[]) {
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
        "--delete-before per-directory parity failure:\n{}",
        group_diffs.join("\n")
    );

    for (label, run) in [("upstream", &upstream), ("oc-rsync", &oc_rsync)] {
        let last_delete = last_index(&run.events, |e| {
            matches!(e.kind, SyscallKind::Unlink | SyscallKind::Rmdir)
        });
        let first_create = first_index(&run.events, |e| matches!(e.kind, SyscallKind::OpenCreate));
        let (Some(ld), Some(fc)) = (last_delete, first_create) else {
            continue;
        };
        assert!(
            ld < fc,
            "{label}: --delete-before violated ordering invariant; \
             last delete at idx {ld} followed by first create at idx {fc}",
        );
    }
}
