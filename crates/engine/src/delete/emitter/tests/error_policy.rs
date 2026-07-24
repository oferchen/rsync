//! DDP-C3 error-classification and continue-on-error tests for the
//! delete emitter.
//!
//! Mirrors upstream `delete.c:178-207` continue-on-error semantics and
//! `cleanup_and_exit` exit-code mapping.

use std::io;
use std::path::PathBuf;

use super::super::super::{DeleteEntryKind, DeletePlanMap, DirTraversalCursor};
use super::super::policy::{IOERR_GENERAL, IOERR_VANISHED_ONLY};
use super::super::{
    DeleteEmitter, DeleteEvent, EMITTER_PARTIAL_EXIT_CODE, EMITTER_VANISHED_EXIT_CODE,
    EmitterErrorPolicy,
};
use super::{ScriptedDeleteFs, entry, plan};

#[test]
fn non_empty_dir_recurses_via_nested_plan() {
    // The directory `d/sub` has a published nested plan with two
    // entries. The parent plan deletes `d/sub` as a Dir; the
    // emitter's first rmdir attempt yields ENOTEMPTY, so the
    // emitter must drain the nested plan (in plan order) and retry
    // rmdir, mirroring upstream `delete_dir_contents` +
    // `delete_item` retry (`delete.c:48-122`, `:161-163`).
    let fs = ScriptedDeleteFs::new().fail("d/sub", io::ErrorKind::DirectoryNotEmpty);
    let plans = DeletePlanMap::new();
    plans.insert(plan("d", vec![entry("sub", DeleteEntryKind::Dir)]));
    plans.insert(plan(
        "d/sub",
        vec![
            entry("inner-file", DeleteEntryKind::File),
            entry("inner-link", DeleteEntryKind::Symlink),
        ],
    ));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
    cursor.observe_segment(PathBuf::from("d"), &[]);

    let mut emitter = DeleteEmitter::new(fs, plans, cursor);
    emitter.emit_all().expect("drain succeeds");

    // Expected sequence: rmdir(sub) failed and was never recorded,
    // then inner-file unlink, inner-link unlink, then the retried
    // rmdir(sub) succeeded.
    let events = emitter.fs().events();
    assert_eq!(
        events,
        vec![
            DeleteEvent {
                path: PathBuf::from("d/sub/inner-file"),
                kind: DeleteEntryKind::File,
            },
            DeleteEvent {
                path: PathBuf::from("d/sub/inner-link"),
                kind: DeleteEntryKind::Symlink,
            },
            DeleteEvent {
                path: PathBuf::from("d/sub"),
                kind: DeleteEntryKind::Dir,
            },
        ],
    );
    assert_eq!(emitter.io_error(), 0);
    assert_eq!(emitter.stats().dirs, 1);
}

#[test]
fn non_empty_dir_after_peel_reports_notice_without_error() {
    // The directory `d/sub` still holds filtered/perishable content after
    // its nested plan drains, so the retried `rmdir` reports ENOTEMPTY a
    // second time. Upstream (delete.c:117-119 / :197-199) prints
    // "cannot delete non-empty directory: <dir>" via FINFO and treats the
    // outcome as DR_NOT_EMPTY: the user must learn the directory survived,
    // but the run neither counts it as deleted nor records an I/O error, so
    // the exit code stays 0. Scripting two ENOTEMPTY failures models the
    // initial rmdir plus the post-peel retry both failing.
    use logging::{DiagnosticEvent, InfoFlag, VerbosityConfig, drain_events, init};

    // NONREG (info_verbosity[0]) is the only info category present at the
    // default verbosity, matching upstream's ungated FINFO rendering.
    init(VerbosityConfig::from_verbose_level(0));
    let _ = drain_events();

    let fs = ScriptedDeleteFs::new()
        .fail("d/sub", io::ErrorKind::DirectoryNotEmpty)
        .fail("d/sub", io::ErrorKind::DirectoryNotEmpty);
    let plans = DeletePlanMap::new();
    plans.insert(plan("d", vec![entry("sub", DeleteEntryKind::Dir)]));
    plans.insert(plan("d/sub", vec![entry("inner", DeleteEntryKind::File)]));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
    cursor.observe_segment(PathBuf::from("d"), &[]);

    let mut emitter = DeleteEmitter::new(fs, plans, cursor);
    emitter
        .emit_all()
        .expect("drain succeeds despite non-empty dir");

    // The peeled entry was unlinked, but the surviving directory is neither
    // counted nor treated as an error.
    assert_eq!(
        emitter.fs().events(),
        vec![DeleteEvent {
            path: PathBuf::from("d/sub/inner"),
            kind: DeleteEntryKind::File,
        }],
    );
    assert_eq!(emitter.stats().dirs, 0);
    assert_eq!(emitter.io_error(), 0);
    assert_eq!(emitter.exit_code(), 0);

    let messages: Vec<String> = drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Info {
                flag: InfoFlag::Nonreg,
                message,
                ..
            } => Some(message),
            _ => None,
        })
        .collect();
    assert!(
        messages
            .iter()
            .any(|m| m == "cannot delete non-empty directory: d/sub"),
        "expected upstream non-empty-directory notice; got {messages:?}"
    );
}

#[test]
fn non_empty_dir_without_plan_falls_back_to_remove_dir_all() {
    // When `d/orphan` has no published plan, ENOTEMPTY must route
    // through `DeleteFs::remove_dir_all`, mirroring upstream's
    // `delete_dir_contents` peel.
    let fs = ScriptedDeleteFs::new().fail("d/orphan", io::ErrorKind::DirectoryNotEmpty);
    let plans = DeletePlanMap::new();
    plans.insert(plan("d", vec![entry("orphan", DeleteEntryKind::Dir)]));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
    cursor.observe_segment(PathBuf::from("d"), &[]);

    let mut emitter = DeleteEmitter::new(fs, plans, cursor);
    emitter.emit_all().expect("drain succeeds");

    // The recorder logs `remove_dir_all` as a single Dir event on
    // the directory path.
    assert_eq!(
        emitter.fs().events(),
        vec![DeleteEvent {
            path: PathBuf::from("d/orphan"),
            kind: DeleteEntryKind::Dir,
        }],
    );
    assert_eq!(emitter.io_error(), 0);
    assert_eq!(emitter.stats().dirs, 1);
}

#[test]
fn nonfatal_error_in_middle_keeps_draining_and_sets_io_error() {
    // The middle entry fails with EBUSY (Other). Continue policy is
    // on by default, so subsequent entries must still process and
    // io_error must reflect IOERR_GENERAL.
    let fs = ScriptedDeleteFs::new().fail("d/middle", io::ErrorKind::Other);
    let plans = DeletePlanMap::new();
    plans.insert(plan(
        "d",
        vec![
            entry("first", DeleteEntryKind::File),
            entry("middle", DeleteEntryKind::File),
            entry("last", DeleteEntryKind::File),
        ],
    ));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
    cursor.observe_segment(PathBuf::from("d"), &[]);

    let mut emitter = DeleteEmitter::new(fs, plans, cursor);
    emitter.emit_all().expect("drain succeeds on non-fatal err");

    let paths: Vec<PathBuf> = emitter
        .fs()
        .events()
        .iter()
        .map(|e| e.path.clone())
        .collect();
    // `middle` was never recorded because it errored before the
    // recording branch. `first` and `last` both succeeded.
    assert_eq!(
        paths,
        vec![PathBuf::from("d/first"), PathBuf::from("d/last")],
    );
    assert_eq!(emitter.io_error(), IOERR_GENERAL);
    assert_eq!(emitter.exit_code(), EMITTER_PARTIAL_EXIT_CODE);
    // Only successful entries bump the stats.
    assert_eq!(emitter.stats().files, 2);
}

#[test]
fn eperm_is_nonfatal_and_continues() {
    // EPERM on a destination entry is NOT fatal: upstream
    // (delete.c:86-210) logs FERROR_XFER, sets io_error |= IOERR_GENERAL,
    // and keeps deleting the remaining siblings. The drain must therefore
    // process `third`, record IOERR_GENERAL, and finish without surfacing
    // an aborting error.
    let fs = ScriptedDeleteFs::new().fail("d/second", io::ErrorKind::PermissionDenied);
    let plans = DeletePlanMap::new();
    plans.insert(plan(
        "d",
        vec![
            entry("first", DeleteEntryKind::File),
            entry("second", DeleteEntryKind::File),
            entry("third", DeleteEntryKind::File),
        ],
    ));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
    cursor.observe_segment(PathBuf::from("d"), &[]);

    let mut emitter = DeleteEmitter::new(fs, plans, cursor);
    emitter
        .emit_all()
        .expect("EPERM is non-fatal and the drain continues");

    let paths: Vec<PathBuf> = emitter
        .fs()
        .events()
        .iter()
        .map(|e| e.path.clone())
        .collect();
    // `second` errored before the recording branch; `first` and `third`
    // both succeeded because the drain did not abort.
    assert_eq!(
        paths,
        vec![PathBuf::from("d/first"), PathBuf::from("d/third")]
    );
    assert_eq!(emitter.io_error(), IOERR_GENERAL);
    assert_eq!(emitter.exit_code(), EMITTER_PARTIAL_EXIT_CODE);
    assert_eq!(emitter.stats().files, 2);
}

#[test]
fn ignore_errors_suppresses_io_error_flag() {
    // With ignore_errors=true the drain still continues but
    // io_error stays zero - matching upstream `--ignore-errors`.
    let fs = ScriptedDeleteFs::new().fail("d/bad", io::ErrorKind::Other);
    let plans = DeletePlanMap::new();
    plans.insert(plan(
        "d",
        vec![
            entry("ok", DeleteEntryKind::File),
            entry("bad", DeleteEntryKind::File),
        ],
    ));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
    cursor.observe_segment(PathBuf::from("d"), &[]);

    let policy = EmitterErrorPolicy {
        ignore_errors: true,
        continue_on_error: true,
    };
    let mut emitter = DeleteEmitter::with_policy(fs, plans, cursor, policy);
    emitter.emit_all().expect("drain succeeds");

    assert_eq!(emitter.io_error(), 0);
    assert_eq!(emitter.exit_code(), 0);
    assert_eq!(emitter.stats().files, 1);
}

#[test]
fn vanished_only_maps_to_exit_24() {
    // NotFound on a destination entry is a vanished-race; when it's
    // the sole failure, the run must report exit code 24
    // (RERR_VANISHED), not 23.
    let fs = ScriptedDeleteFs::new().fail("d/gone", io::ErrorKind::NotFound);
    let plans = DeletePlanMap::new();
    plans.insert(plan(
        "d",
        vec![
            entry("here", DeleteEntryKind::File),
            entry("gone", DeleteEntryKind::File),
        ],
    ));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
    cursor.observe_segment(PathBuf::from("d"), &[]);

    let mut emitter = DeleteEmitter::new(fs, plans, cursor);
    emitter.emit_all().expect("drain succeeds");

    assert_eq!(emitter.io_error(), IOERR_VANISHED_ONLY);
    assert_eq!(emitter.exit_code(), EMITTER_VANISHED_EXIT_CODE);
    assert_eq!(emitter.stats().files, 1);
}

#[test]
fn continue_off_aborts_on_first_nonfatal_error() {
    // continue_on_error=false stops at the first non-fatal failure
    // and surfaces the error to the caller.
    let fs = ScriptedDeleteFs::new().fail("d/two", io::ErrorKind::Other);
    let plans = DeletePlanMap::new();
    plans.insert(plan(
        "d",
        vec![
            entry("one", DeleteEntryKind::File),
            entry("two", DeleteEntryKind::File),
            entry("three", DeleteEntryKind::File),
        ],
    ));
    let mut cursor = DirTraversalCursor::new(PathBuf::from("d"));
    cursor.observe_segment(PathBuf::from("d"), &[]);

    let policy = EmitterErrorPolicy {
        ignore_errors: false,
        continue_on_error: false,
    };
    let mut emitter = DeleteEmitter::with_policy(fs, plans, cursor, policy);
    emitter
        .emit_all()
        .expect_err("non-fatal failure aborts when continue is off");

    let paths: Vec<PathBuf> = emitter
        .fs()
        .events()
        .iter()
        .map(|e| e.path.clone())
        .collect();
    assert_eq!(paths, vec![PathBuf::from("d/one")]);
    assert_eq!(emitter.io_error(), IOERR_GENERAL);
}
