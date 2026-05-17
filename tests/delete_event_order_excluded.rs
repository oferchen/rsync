//! G5 / #2279 - `--delete-excluded` event-order parity vs upstream rsync 3.4.1.
//!
//! Asserts that `--delete-excluded` combined with each timing mode
//! (`--delete-during`, `--delete-before`, `--delete-after`, `--delete-delay`)
//! produces the same per-directory delete sequences as upstream rsync,
//! including for files whose paths match the exclude pattern. Upstream rsync
//! flips the `DEL_EXCLUDED` bit (`flist.c`) so excluded paths are visited by
//! `delete_in_dir()`; oc-rsync's parallel-deterministic emitter must reach
//! the same conclusion regardless of the timing window.
//!
//! Each (timing-mode, --delete-excluded) combination layers the timing-mode
//! ordering invariant on top of the per-directory parity check.
//!
//! GATE: `OC_RSYNC_DELETE_INTEROP=1`. Will fail on master until DDP-E1-E5
//! lands.

#![cfg(unix)]

mod integration;

use integration::delete_event_order_harness::{
    Event, PairOutcome, Scenario, SyscallKind, diff_delete_groups, first_index, last_index,
    run_pair_capture, skip,
};

/// Layered ordering check matching the per-mode invariant of the sibling
/// test files.
fn assert_timing_invariant(label: &str, mode: &str, events: &[Event]) {
    let last_delete = last_index(events, |e| {
        matches!(e.kind, SyscallKind::Unlink | SyscallKind::Rmdir)
    });
    let first_delete = first_index(events, |e| {
        matches!(e.kind, SyscallKind::Unlink | SyscallKind::Rmdir)
    });
    let first_create = first_index(events, |e| matches!(e.kind, SyscallKind::OpenCreate));
    let last_create = last_index(events, |e| matches!(e.kind, SyscallKind::OpenCreate));
    let last_rename = last_index(events, |e| matches!(e.kind, SyscallKind::Rename));

    match mode {
        "--delete-before" => {
            if let (Some(ld), Some(fc)) = (last_delete, first_create) {
                assert!(
                    ld < fc,
                    "{label} {mode}: last delete idx {ld} must precede first create idx {fc}",
                );
            }
        }
        "--delete-after" => {
            if let (Some(lc), Some(fd)) = (last_create, first_delete) {
                assert!(
                    lc < fd,
                    "{label} {mode}: last create idx {lc} must precede first delete idx {fd}",
                );
            }
        }
        "--delete-delay" => {
            if let (Some(lr), Some(fd)) = (last_rename, first_delete) {
                assert!(
                    lr < fd,
                    "{label} {mode}: last rename idx {lr} must precede first delete idx {fd}",
                );
            }
        }
        // --delete-during: no global ordering invariant; per-dir parity is
        // the only assertion.
        _ => {}
    }
}

fn run_combo(mode: &str) {
    let scenario = Scenario::excluded_default();
    let outcome = match run_pair_capture(
        &scenario,
        &[mode, "--delete-excluded"],
        &["--exclude=*.bak"],
    ) {
        Ok(o) => o,
        Err(e) => panic!("harness failure for {mode}: {e}"),
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
        "{mode} + --delete-excluded per-directory parity failure:\n{}",
        group_diffs.join("\n")
    );

    assert_timing_invariant("upstream", mode, &upstream.events);
    assert_timing_invariant("oc-rsync", mode, &oc_rsync.events);
}

#[test]
fn delete_event_order_excluded_during() {
    run_combo("--delete-during");
}

#[test]
fn delete_event_order_excluded_before() {
    run_combo("--delete-before");
}

#[test]
fn delete_event_order_excluded_after() {
    run_combo("--delete-after");
}

#[test]
fn delete_event_order_excluded_delay() {
    run_combo("--delete-delay");
}
