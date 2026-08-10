//! DDP-C1 / DDP-C4 dispatch-matrix tests for the delete emitter.
//!
//! The recorded dispatch order is the load-bearing upstream-parity
//! invariant from section 9.1 of the parallel-deterministic-delete
//! design.

use std::path::PathBuf;

use protocol::DeleteStats;

use super::super::super::{DeleteEntryKind, DeletePlanMap, DirTraversalCursor};
use super::super::{DeleteEmitter, DeleteEvent, DeleteFs, RealDeleteFs, RecordingDeleteFs};
use super::{dir_child, entry, plan};

#[test]
fn empty_plan_map_returns_immediately() {
    let cursor = DirTraversalCursor::new(PathBuf::from("root"));
    let mut emitter = DeleteEmitter::new(RecordingDeleteFs::new(), DeletePlanMap::new(), cursor);
    emitter.emit_all().expect("empty drain succeeds");
    assert!(emitter.fs().events().is_empty());
    assert_eq!(emitter.stats(), DeleteStats::default());
    assert_eq!(emitter.io_error(), 0);
    assert_eq!(emitter.exit_code(), 0);
}

#[test]
fn dispatch_table_matches_planned_kind() {
    let plans = DeletePlanMap::new();
    plans.insert(plan(
        "d",
        vec![
            entry("file", DeleteEntryKind::File),
            entry("dir", DeleteEntryKind::Dir),
            entry("link", DeleteEntryKind::Symlink),
            entry("dev", DeleteEntryKind::Device),
            entry("fifo", DeleteEntryKind::Special),
        ],
    ));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
    cursor.observe_segment(PathBuf::from("d"), &[]);

    let mut emitter = DeleteEmitter::new(RecordingDeleteFs::new(), plans, cursor);
    emitter.emit_all().expect("drain succeeds");

    let events = emitter.fs().events();
    assert_eq!(
        events,
        vec![
            DeleteEvent {
                path: PathBuf::from("d/file"),
                kind: DeleteEntryKind::File,
            },
            DeleteEvent {
                path: PathBuf::from("d/dir"),
                kind: DeleteEntryKind::Dir,
            },
            DeleteEvent {
                path: PathBuf::from("d/link"),
                kind: DeleteEntryKind::Symlink,
            },
            DeleteEvent {
                path: PathBuf::from("d/dev"),
                kind: DeleteEntryKind::Device,
            },
            DeleteEvent {
                path: PathBuf::from("d/fifo"),
                kind: DeleteEntryKind::Special,
            },
        ],
    );
    let stats = emitter.stats();
    assert_eq!(stats.files, 1);
    assert_eq!(stats.dirs, 1);
    assert_eq!(stats.symlinks, 1);
    assert_eq!(stats.devices, 1);
    assert_eq!(stats.specials, 1);
    assert_eq!(stats.total(), 5);
    assert_eq!(emitter.io_error(), 0);
}

#[test]
fn three_dirs_emit_in_cursor_order_within_plan_order() {
    let plans = DeletePlanMap::new();
    plans.insert(plan(
        "root/a",
        vec![
            entry("a_two", DeleteEntryKind::File),
            entry("a_one", DeleteEntryKind::Symlink),
        ],
    ));
    plans.insert(plan(
        "root/b",
        vec![
            entry("b_two", DeleteEntryKind::Dir),
            entry("b_one", DeleteEntryKind::File),
        ],
    ));
    plans.insert(plan(
        "root/c",
        vec![
            entry("c_two", DeleteEntryKind::Special),
            entry("c_one", DeleteEntryKind::Device),
        ],
    ));
    plans.insert(plan("root", vec![]));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("root"));
    cursor.observe_segment(
        PathBuf::from("root"),
        &[
            dir_child("root", "a"),
            dir_child("root", "b"),
            dir_child("root", "c"),
        ],
    );

    let mut emitter = DeleteEmitter::new(RecordingDeleteFs::new(), plans, cursor);
    emitter.emit_all().expect("drain succeeds");

    let expected: Vec<DeleteEvent> = [
        ("root/a/a_two", DeleteEntryKind::File),
        ("root/a/a_one", DeleteEntryKind::Symlink),
        ("root/b/b_two", DeleteEntryKind::Dir),
        ("root/b/b_one", DeleteEntryKind::File),
        ("root/c/c_two", DeleteEntryKind::Special),
        ("root/c/c_one", DeleteEntryKind::Device),
    ]
    .iter()
    .map(|(p, k)| DeleteEvent {
        path: PathBuf::from(*p),
        kind: *k,
    })
    .collect();
    assert_eq!(emitter.fs().events(), expected);
    let stats = emitter.stats();
    assert_eq!(stats.total(), 6);
}

#[test]
fn cursor_outpaces_plans_parks_at_gap_and_resumes() {
    let plans = DeletePlanMap::new();
    plans.insert(plan("root", vec![]));
    plans.insert(plan("root/a", vec![entry("x", DeleteEntryKind::File)]));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("root"));
    cursor.observe_segment(
        PathBuf::from("root"),
        &[
            dir_child("root", "a"),
            dir_child("root", "b"),
            dir_child("root", "c"),
        ],
    );

    let mut emitter = DeleteEmitter::new(RecordingDeleteFs::new(), plans, cursor);
    emitter
        .emit_all()
        .expect("first drain succeeds up to the gap");
    assert_eq!(
        emitter
            .fs()
            .events()
            .iter()
            .map(|e| e.path.clone())
            .collect::<Vec<_>>(),
        vec![PathBuf::from("root/a/x")],
    );

    emitter
        .plans
        .insert(plan("root/b", vec![entry("y", DeleteEntryKind::Symlink)]));
    emitter
        .plans
        .insert(plan("root/c", vec![entry("z", DeleteEntryKind::Dir)]));
    emitter.emit_all().expect("resume succeeds");
    let tail_paths: Vec<PathBuf> = emitter
        .fs()
        .events()
        .iter()
        .skip(1)
        .map(|e| e.path.clone())
        .collect();
    assert_eq!(
        tail_paths,
        vec![PathBuf::from("root/b/y"), PathBuf::from("root/c/z")],
    );
    let stats = emitter.stats();
    assert_eq!(stats.files, 1);
    assert_eq!(stats.symlinks, 1);
    assert_eq!(stats.dirs, 1);
}

#[test]
fn real_delete_fs_round_trip_on_tempdir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("f");
    std::fs::write(&file, b"x").expect("write file");
    let dir = tmp.path().join("d");
    std::fs::create_dir(&dir).expect("mkdir");

    let fs = RealDeleteFs;
    fs.unlink_file(&file).expect("unlink_file");
    fs.rmdir(&dir).expect("rmdir");

    assert!(!file.exists());
    assert!(!dir.exists());
}

#[test]
fn mixed_kind_plan_preserves_input_dispatch_order() {
    // A single plan that touches every upstream-distinguishable kind
    // in a non-sorted order must dispatch in exactly the order
    // entries appear in `plan.extras` (the order phase-1 already
    // froze via `sort_by_name`). This is the load-bearing
    // upstream-parity invariant from section 9.1.
    let plans = DeletePlanMap::new();
    plans.insert(plan(
        "d",
        vec![
            entry("a-file", DeleteEntryKind::File),
            entry("b-dir", DeleteEntryKind::Dir),
            entry("c-link", DeleteEntryKind::Symlink),
            entry("d-dev", DeleteEntryKind::Device),
            entry("e-fifo", DeleteEntryKind::Special),
        ],
    ));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
    cursor.observe_segment(PathBuf::from("d"), &[]);

    let mut emitter = DeleteEmitter::new(RecordingDeleteFs::new(), plans, cursor);
    emitter.emit_all().expect("drain succeeds");

    let kinds: Vec<DeleteEntryKind> = emitter.fs().events().iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        vec![
            DeleteEntryKind::File,
            DeleteEntryKind::Dir,
            DeleteEntryKind::Symlink,
            DeleteEntryKind::Device,
            DeleteEntryKind::Special,
        ],
    );
}

#[test]
fn fully_drained_plan_map_yields_clean_ok() {
    // Plan-map exhaustion: after one successful drain the cursor is
    // empty, a second emit_all is a no-op that returns Ok(()) with
    // no new events and no io_error.
    let plans = DeletePlanMap::new();
    plans.insert(plan("d", vec![entry("x", DeleteEntryKind::File)]));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
    cursor.observe_segment(PathBuf::from("d"), &[]);

    let mut emitter = DeleteEmitter::new(RecordingDeleteFs::new(), plans, cursor);
    emitter.emit_all().expect("first drain succeeds");
    let after_first = emitter.fs().events().len();
    emitter.emit_all().expect("second drain is a no-op");
    assert_eq!(emitter.fs().events().len(), after_first);
    assert_eq!(emitter.io_error(), 0);
    assert_eq!(emitter.exit_code(), 0);
}
