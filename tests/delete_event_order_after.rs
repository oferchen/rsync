//! G3 / #2277 - `--delete-after` event-order parity vs upstream rsync 3.4.1.
//!
//! Asserts that every `openat(O_CREAT)` precedes every `unlink`/`rmdir`
//! under `--delete-after`. Upstream rsync runs `do_delete_pass()` only after
//! the file list and all transfers complete (`generator.c`); oc-rsync's
//! parallel-deterministic emitter must respect this invariant globally even
//! though file creates and deletions run on different worker threads.
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
fn delete_event_order_after_matches_upstream() {
    let scenario = Scenario::before_after_default();
    let outcome = match run_pair_capture(&scenario, &["--delete-after"], &[]) {
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
        "--delete-after per-directory parity failure:\n{}",
        group_diffs.join("\n")
    );

    for (label, run) in [("upstream", &upstream), ("oc-rsync", &oc_rsync)] {
        let last_create = last_index(&run.events, |e| matches!(e.kind, SyscallKind::OpenCreate));
        let first_delete = first_index(&run.events, |e| {
            matches!(e.kind, SyscallKind::Unlink | SyscallKind::Rmdir)
        });
        let (Some(lc), Some(fd)) = (last_create, first_delete) else {
            continue;
        };
        assert!(
            lc < fd,
            "{label}: --delete-after violated ordering invariant; \
             last create at idx {lc} followed by first delete at idx {fd}",
        );
    }
}
