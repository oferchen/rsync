//! DDP-D2 hardlink-aware delete tests via the `CohortIndex` snapshot.
//!
//! Per `docs/design/hardlink-delete-audit.md` Option A and upstream
//! `delete.c:130-225`, the emitter still unlinks every destination
//! path even when the cohort retains other refs (the kernel
//! reconciles ref counts). The snapshot powers cohort-aware itemize
//! bookkeeping via `cohort_records`, not the unlink decision itself.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use protocol::flist::FileEntry;

use super::super::super::cohort_index::CohortIndex;
use super::super::super::{DeleteEntry, DeleteEntryKind, DeletePlanMap, DirTraversalCursor};
use super::super::policy::IOERR_GENERAL;
use super::super::{DeleteEmitter, EmitterErrorPolicy, HardlinkCohortId, RecordingDeleteFs};
use super::{ScriptedDeleteFs, entry, plan};

fn tagged_entry(name: &str, kind: DeleteEntryKind, cohort: HardlinkCohortId) -> DeleteEntry {
    DeleteEntry::with_cohort(OsString::from(name), kind, cohort)
}

/// Builds a `CohortIndex` from a synthetic segment with `member_count`
/// source-side refs sharing one cohort. The leader's name is
/// `leader`; followers are `member0`, `member1`, etc.
fn cohort_index_for(leader: &str, member_count: usize) -> Arc<CohortIndex> {
    let mut entries = Vec::with_capacity(1 + member_count);
    let mut head = FileEntry::new_file(PathBuf::from(leader), 0, 0o644);
    head.set_hardlink_idx(u32::MAX);
    entries.push(head);
    for i in 0..member_count {
        let mut e = FileEntry::new_file(PathBuf::from(format!("member{i}")), 0, 0o644);
        e.set_hardlink_idx(0);
        entries.push(e);
    }
    CohortIndex::build_from_flist_segment(&entries)
}

#[test]
fn emitter_with_cohort_index_records_cohort_per_dispatch() {
    // The destination has three refs in one cohort (`leader`,
    // `member0`, `member1`); the source has two (`leader` and
    // `member0`). Upstream's policy is to unlink all three dest
    // paths; the cohort log must reflect that with cohort tags on
    // the two known members and no tag on the third.
    let cohort = HardlinkCohortId::new(0);
    let index = cohort_index_for("leader", 1); // source = leader + member0
    let plans = DeletePlanMap::new();
    plans.insert(plan(
        "d",
        vec![
            tagged_entry("leader", DeleteEntryKind::File, cohort),
            tagged_entry("member0", DeleteEntryKind::File, cohort),
            entry("member1", DeleteEntryKind::File),
        ],
    ));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
    cursor.observe_segment(PathBuf::from("d"), &[]);

    let mut emitter = DeleteEmitter::with_cohort_index(
        RecordingDeleteFs::new(),
        plans,
        cursor,
        EmitterErrorPolicy::default(),
        Arc::clone(&index),
    );
    emitter.emit_all().expect("drain succeeds");

    // The kernel-policy invariant: every dest path was unlinked
    // (matches upstream `delete.c:130-225`).
    let events = emitter.fs().events();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].path, PathBuf::from("d/leader"));
    assert_eq!(events[1].path, PathBuf::from("d/member0"));
    assert_eq!(events[2].path, PathBuf::from("d/member1"));

    // The cohort log records the per-dispatch cohort tag.
    let records = emitter.cohort_records();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0].cohort, Some(cohort));
    assert_eq!(records[0].surviving_source_refs, 2);
    assert_eq!(records[1].cohort, Some(cohort));
    assert_eq!(records[1].surviving_source_refs, 2);
    // The orphan-on-the-dest gets no cohort tag.
    assert_eq!(records[2].cohort, None);
    assert_eq!(records[2].surviving_source_refs, 0);
}

#[test]
fn emitter_without_cohort_index_records_no_cohort_log() {
    // Baseline: with no CohortIndex attached, the cohort log stays
    // empty even when the plan carries cohort tags. This keeps the
    // legacy hot path zero-overhead.
    let cohort = HardlinkCohortId::new(7);
    let plans = DeletePlanMap::new();
    plans.insert(plan(
        "d",
        vec![tagged_entry("x", DeleteEntryKind::File, cohort)],
    ));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
    cursor.observe_segment(PathBuf::from("d"), &[]);

    let mut emitter = DeleteEmitter::new(RecordingDeleteFs::new(), plans, cursor);
    emitter.emit_all().expect("drain succeeds");

    assert!(emitter.cohort_index().is_none());
    assert!(emitter.cohort_records().is_empty());
    assert_eq!(emitter.stats().files, 1);
}

#[test]
fn emitter_cohort_index_does_not_change_dispatch_order() {
    // Attaching a CohortIndex must not perturb the syscall order -
    // the cohort log is observer-only. The dispatch must match
    // exactly what the no-cohort variant produces.
    let cohort = HardlinkCohortId::new(0);
    let index = cohort_index_for("leader", 2);

    let baseline_plans = DeletePlanMap::new();
    baseline_plans.insert(plan(
        "d",
        vec![
            entry("leader", DeleteEntryKind::File),
            entry("member0", DeleteEntryKind::File),
            entry("member1", DeleteEntryKind::File),
        ],
    ));
    let mut baseline_cursor = DirTraversalCursor::new(PathBuf::from("d"));
    baseline_cursor.observe_segment(PathBuf::from("d"), &[]);
    let mut baseline_emitter =
        DeleteEmitter::new(RecordingDeleteFs::new(), baseline_plans, baseline_cursor);
    baseline_emitter.emit_all().unwrap();
    let baseline_events = baseline_emitter.fs().events();

    let cohort_plans = DeletePlanMap::new();
    cohort_plans.insert(plan(
        "d",
        vec![
            tagged_entry("leader", DeleteEntryKind::File, cohort),
            tagged_entry("member0", DeleteEntryKind::File, cohort),
            tagged_entry("member1", DeleteEntryKind::File, cohort),
        ],
    ));
    let mut cohort_cursor = DirTraversalCursor::new(PathBuf::from("d"));
    cohort_cursor.observe_segment(PathBuf::from("d"), &[]);
    let mut cohort_emitter = DeleteEmitter::with_cohort_index(
        RecordingDeleteFs::new(),
        cohort_plans,
        cohort_cursor,
        EmitterErrorPolicy::default(),
        index,
    );
    cohort_emitter.emit_all().unwrap();
    assert_eq!(cohort_emitter.fs().events(), baseline_events);
}

#[test]
fn cohort_log_skips_failed_dispatch() {
    // A failed dispatch must NOT add a cohort record - the cohort
    // log is meant to mirror successful syscalls (so it matches the
    // emitter's stats counter, which is also success-only).
    let cohort = HardlinkCohortId::new(0);
    let index = cohort_index_for("leader", 0); // single-ref source
    let fs = ScriptedDeleteFs::new().fail("d/leader", io::ErrorKind::Other);
    let plans = DeletePlanMap::new();
    plans.insert(plan(
        "d",
        vec![tagged_entry("leader", DeleteEntryKind::File, cohort)],
    ));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
    cursor.observe_segment(PathBuf::from("d"), &[]);

    let mut emitter =
        DeleteEmitter::with_cohort_index(fs, plans, cursor, EmitterErrorPolicy::default(), index);
    emitter
        .emit_all()
        .expect("non-fatal failure keeps draining");

    assert!(
        emitter.cohort_records().is_empty(),
        "failed dispatch must not appear in cohort log",
    );
    assert_eq!(emitter.io_error(), IOERR_GENERAL);
}
