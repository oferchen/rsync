//! Unit tests for [`DeleteContext`], [`EmitterTiming`], and the
//! cursor-channel drain protocol.

use std::ffi::OsStr;
use std::fs;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use protocol::flist::FileEntry;
use tempfile::TempDir;

use super::super::emitter::{DeleteEvent, RecordingDeleteFs};
#[cfg(not(feature = "parallel-delete-consumer"))]
use super::super::error::DeleteError;
use super::super::plan::DeleteEntryKind;
use super::super::plan_map::DeletePlanMap;
use super::{DeleteContext, EmitterTiming};

fn touch(dir: &Path, name: &str) {
    File::create(dir.join(name)).expect("create file");
}

fn flist_file(name: &str) -> FileEntry {
    FileEntry::new_file(PathBuf::from(name), 0, 0o644)
}

fn flist_dir(name: &str) -> FileEntry {
    FileEntry::new_directory(PathBuf::from(name), 0o755)
}

fn make_segment(names: &[&str]) -> Vec<FileEntry> {
    names.iter().map(|n| flist_file(n)).collect()
}

fn dir_child(parent: &str, name: &str) -> FileEntry {
    let path = if parent.is_empty() {
        PathBuf::from(name)
    } else {
        PathBuf::from(parent).join(name)
    };
    FileEntry::new_directory(path, 0o755)
}

#[test]
fn disabled_context_publishes_nothing() {
    let dir = TempDir::new().unwrap();
    touch(dir.path(), "extra");
    let map = Arc::new(DeletePlanMap::new());
    let ctx =
        DeleteContext::with_shared_plan_map(Arc::clone(&map), dir.path().to_path_buf(), false);

    ctx.observe_segment_for_delete(Path::new(""), &[flist_file("kept")])
        .expect("disabled is a no-op");

    assert!(map.is_empty());
    let cursor_lock = ctx.cursor_snapshot();
    let mut cursor = cursor_lock.lock().unwrap();
    // Even with no observations, the root is still emitted, and the
    // second call drains the now-empty stack and reports exhaustion.
    assert_eq!(cursor.next_ready(), Some(PathBuf::new()));
    assert_eq!(cursor.next_ready(), None);
    assert!(cursor.is_exhausted());
}

#[test]
fn enabled_context_publishes_sorted_plan_and_records_children() {
    let dir = TempDir::new().unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    for n in ["a", "b", "c", "d"] {
        touch(&sub, n);
    }

    let map = Arc::new(DeletePlanMap::new());
    let ctx = DeleteContext::with_shared_plan_map(Arc::clone(&map), dir.path().to_path_buf(), true);

    let root_segment = vec![flist_dir("sub")];
    ctx.observe_segment_for_delete(Path::new(""), &root_segment)
        .expect("root observe ok");

    let segment = vec![flist_file("a"), flist_file("c"), flist_dir("nested")];
    ctx.observe_segment_for_delete(Path::new("sub"), &segment)
        .expect("observe ok");

    assert!(map.contains(Path::new("sub")));
    let plan = map.take(Path::new("sub")).expect("plan present");
    assert!(plan.is_sorted());
    let names: Vec<&OsStr> = plan.extras.iter().map(|e| e.name.as_os_str()).collect();
    assert_eq!(names, vec![OsStr::new("d"), OsStr::new("b")]);

    let cursor_lock = ctx.cursor_snapshot();
    let mut cursor = cursor_lock.lock().unwrap();
    let seq: Vec<PathBuf> = std::iter::from_fn(|| cursor.next_ready()).collect();
    assert!(seq.contains(&PathBuf::from("sub/nested")));
}

#[test]
fn accumulates_plans_across_segments() {
    let root = TempDir::new().unwrap();
    for sub in ["s1", "s2", "s3"] {
        let p = root.path().join(sub);
        fs::create_dir(&p).unwrap();
        touch(&p, "keeper");
        touch(&p, "trash");
    }

    let map = Arc::new(DeletePlanMap::new());
    let ctx =
        DeleteContext::with_shared_plan_map(Arc::clone(&map), root.path().to_path_buf(), true);

    for sub in ["s1", "s2", "s3"] {
        let entries = vec![flist_file("keeper")];
        ctx.observe_segment_for_delete(Path::new(sub), &entries)
            .expect("observe ok");
    }

    assert_eq!(map.len(), 3);
    for sub in ["s1", "s2", "s3"] {
        let plan = map.take(Path::new(sub)).expect("plan present");
        assert_eq!(plan.extras.len(), 1);
        assert_eq!(plan.extras[0].name, std::ffi::OsString::from("trash"));
    }
}

#[test]
fn missing_destination_dir_surfaces_io_error() {
    let root = TempDir::new().unwrap();
    let map = Arc::new(DeletePlanMap::new());
    let ctx =
        DeleteContext::with_shared_plan_map(Arc::clone(&map), root.path().to_path_buf(), true);

    let err = ctx
        .observe_segment_for_delete(Path::new("does-not-exist"), &[])
        .expect_err("missing dir is an error");
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
    assert!(map.is_empty());
}

#[test]
fn with_cursor_root_uses_provided_root() {
    let root = TempDir::new().unwrap();
    let map = Arc::new(DeletePlanMap::new());
    let ctx = DeleteContext::with_cursor_root(
        Arc::clone(&map),
        root.path().to_path_buf(),
        PathBuf::from("from_here"),
        true,
    );

    let cursor_lock = ctx.cursor_snapshot();
    let mut cursor = cursor_lock.lock().unwrap();
    assert_eq!(cursor.next_ready(), Some(PathBuf::from("from_here")));
}

#[test]
fn empty_segment_still_publishes_plan_for_dest_only_entries() {
    let root = TempDir::new().unwrap();
    touch(root.path(), "ghost1");
    touch(root.path(), "ghost2");

    let map = Arc::new(DeletePlanMap::new());
    let ctx =
        DeleteContext::with_shared_plan_map(Arc::clone(&map), root.path().to_path_buf(), true);
    ctx.observe_segment_for_delete(Path::new(""), &[])
        .expect("observe ok");

    let plan = map.take(Path::new("")).expect("plan present");
    assert_eq!(plan.extras.len(), 2);
    assert!(plan.is_sorted());
}

#[test]
fn timing_predicates_partition_modes() {
    assert!(EmitterTiming::Before.drains_pre_transfer());
    assert!(EmitterTiming::During.drains_per_directory());
    assert!(EmitterTiming::After.drains_post_transfer());
    assert!(EmitterTiming::Delay.drains_post_transfer());
    assert!(!EmitterTiming::Before.drains_per_directory());
    assert!(!EmitterTiming::During.drains_post_transfer());
}

#[test]
fn round_trip_with_local_copy_delete_timing() {
    for mode in [
        EmitterTiming::Before,
        EmitterTiming::During,
        EmitterTiming::After,
        EmitterTiming::Delay,
    ] {
        let lc: crate::local_copy::DeleteTiming = mode.into();
        let back: EmitterTiming = lc.into();
        assert_eq!(mode, back);
    }
}

#[test]
fn during_mode_drains_one_directory_through_emitter() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("sub");
    fs::create_dir(&dir).unwrap();
    for n in ["keep", "drop"] {
        touch(&dir, n);
    }

    let ctx = DeleteContext::new(dir.clone(), EmitterTiming::During);
    ctx.observe_directory(dir.clone(), &[]);
    ctx.begin_directory(make_segment(&["keep"]));
    ctx.publish_plan_for(&dir).expect("publish plan");

    let outcome = ctx
        .emit_one(RecordingDeleteFs::new())
        .expect("drain succeeds");
    let events = outcome.fs.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].path, dir.join("drop"));
    assert_eq!(events[0].kind, DeleteEntryKind::File);
    assert_eq!(outcome.stats.files, 1);
    assert_eq!(outcome.exit_code, 0);
}

#[test]
fn before_mode_drains_pre_walk_pass_across_dirs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    fs::create_dir(&a).unwrap();
    fs::create_dir(&b).unwrap();
    touch(&a, "x");
    touch(&b, "y");

    let ctx = DeleteContext::new(tmp.path().to_path_buf(), EmitterTiming::Before);
    ctx.observe_directory(
        tmp.path().to_path_buf(),
        &[
            dir_child(tmp.path().to_str().unwrap(), "a"),
            dir_child(tmp.path().to_str().unwrap(), "b"),
        ],
    );
    ctx.begin_directory(make_segment(&[]));
    ctx.publish_plan_for(tmp.path()).unwrap();
    ctx.begin_directory(make_segment(&[]));
    ctx.publish_plan_for(&a).unwrap();
    ctx.begin_directory(make_segment(&[]));
    ctx.publish_plan_for(&b).unwrap();

    let outcome = ctx
        .emit_all(RecordingDeleteFs::new())
        .expect("drain succeeds");
    let names: Vec<PathBuf> = outcome.fs.events().iter().map(|e| e.path.clone()).collect();
    assert!(names.iter().any(|p| p == &a.join("x")));
    assert!(names.iter().any(|p| p == &b.join("y")));
    assert_eq!(outcome.stats.files, 2);
}

#[test]
fn after_mode_accumulates_then_drains() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().to_path_buf();
    touch(&dir, "later1");
    touch(&dir, "later2");

    let ctx = DeleteContext::new(dir.clone(), EmitterTiming::After);
    ctx.observe_directory(dir.clone(), &[]);
    ctx.begin_directory(make_segment(&[]));
    ctx.publish_plan_for(&dir).unwrap();

    let outcome = ctx
        .emit_all(RecordingDeleteFs::new())
        .expect("drain succeeds");
    assert_eq!(outcome.stats.files, 2);
    assert_eq!(outcome.exit_code, 0);
}

#[test]
fn delay_mode_uses_same_drain_path_as_after() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().to_path_buf();
    touch(&dir, "delayed");

    let ctx = DeleteContext::new(dir.clone(), EmitterTiming::Delay);
    ctx.observe_directory(dir.clone(), &[]);
    ctx.begin_directory(make_segment(&[]));
    ctx.publish_plan_for(&dir).unwrap();

    let outcome = ctx
        .emit_all(RecordingDeleteFs::new())
        .expect("drain succeeds");
    assert_eq!(outcome.stats.files, 1);
}

#[test]
fn delete_excluded_layering_bit_round_trips() {
    let ctx =
        DeleteContext::new(PathBuf::from("/"), EmitterTiming::During).with_delete_excluded(true);
    assert!(ctx.delete_excluded);
    let ctx = DeleteContext::new(PathBuf::from("/"), EmitterTiming::During);
    assert!(!ctx.delete_excluded);
}

#[cfg(not(feature = "parallel-delete-consumer"))]
#[test]
fn into_emitter_reports_plan_map_still_shared_with_strong_count() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plans = Arc::new(DeletePlanMap::new());
    let ctx =
        DeleteContext::with_shared_plan_map(Arc::clone(&plans), tmp.path().to_path_buf(), true);
    // The caller still holds `plans`; the drain must surface the
    // residual strong-count via the typed error.
    let err = ctx
        .into_emitter(RecordingDeleteFs::new())
        .expect_err("plan map is still shared");
    match err {
        DeleteError::PlanMapStillShared { strong_count } => {
            assert!(
                strong_count >= 2,
                "expected strong_count >= 2, got {strong_count}"
            );
        }
        other => panic!("expected PlanMapStillShared, got {other:?}"),
    }
    drop(plans);
}

#[test]
fn emit_one_propagates_typed_error_through_io_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plans = Arc::new(DeletePlanMap::new());
    let ctx =
        DeleteContext::with_shared_plan_map(Arc::clone(&plans), tmp.path().to_path_buf(), true);
    let err = ctx
        .emit_one(RecordingDeleteFs::new())
        .expect_err("plan map still shared surfaces as io::Error");
    let msg = err.to_string();
    assert!(msg.contains("DeletePlanMap"));
    assert!(msg.contains("strong_count="));
    drop(plans);
}

#[test]
fn record_drain_outcome_carries_recorded_events() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().to_path_buf();
    touch(&dir, "victim");
    let ctx = DeleteContext::new(dir.clone(), EmitterTiming::During);
    ctx.observe_directory(dir.clone(), &[]);
    ctx.begin_directory(make_segment(&[]));
    ctx.publish_plan_for(&dir).unwrap();
    let outcome = ctx.emit_one(RecordingDeleteFs::new()).unwrap();
    assert_eq!(
        outcome.fs.events(),
        vec![DeleteEvent {
            path: dir.join("victim"),
            kind: DeleteEntryKind::File,
        }]
    );
}

/// ATU-4 (#2381): channel-shutdown drain terminates as soon as every
/// worker drops its `Arc<DeleteContext>` clone. We spawn N producer
/// threads, each enqueueing one cursor observation through the
/// shared context, join them all (so every Arc clone is gone), and
/// then run the drain on the owned context. The drain must complete
/// without blocking and must surface every observation in the final
/// cursor.
#[test]
fn channel_drain_completes_when_workers_drop_normally() {
    use std::thread;

    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().to_path_buf();
    for i in 0..8 {
        touch(&dir, &format!("worker{i}"));
    }
    let ctx = Arc::new(DeleteContext::new(dir.clone(), EmitterTiming::During));
    ctx.observe_directory(dir.clone(), &[]);

    let mut handles = Vec::new();
    for i in 0..8 {
        let ctx = Arc::clone(&ctx);
        let parent = dir.clone();
        handles.push(thread::spawn(move || {
            ctx.observe_directory(parent, &[dir_child("", &format!("child{i}"))]);
        }));
    }
    for h in handles {
        h.join().expect("worker joined");
    }

    // All Arc clones are gone; we now own the context uniquely.
    let owned = Arc::try_unwrap(ctx).expect("workers released their clones");
    owned.begin_directory(make_segment(&[]));
    owned.publish_plan_for(&dir).expect("publish plan");

    let outcome = owned
        .emit_one(RecordingDeleteFs::new())
        .expect("drain completes without blocking");
    assert_eq!(
        outcome.stats.files, 8,
        "every worker file is deleted by the drain"
    );
}

/// ATU-4 (#2381): even if a worker panics mid-send, the channel-
/// shutdown semantics still let the drain finish. Each producer's
/// `Sender` clone is dropped by the panic-unwind, the `recv` loop
/// terminates, and the drain returns successfully with whatever
/// observations made it through before the panic.
#[test]
fn channel_drain_completes_when_worker_panics_mid_send() {
    use std::panic;
    use std::thread;

    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().to_path_buf();
    touch(&dir, "survivor");
    let ctx = Arc::new(DeleteContext::new(dir.clone(), EmitterTiming::During));
    ctx.observe_directory(dir.clone(), &[]);

    // Worker A sends one observation, then panics. Drop runs as the
    // thread unwinds, so its borrow on the shared context goes away
    // cleanly.
    let ctx_a = Arc::clone(&ctx);
    let dir_a = dir.clone();
    let panic_handle = thread::spawn(move || {
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            ctx_a.observe_directory(dir_a, &[]);
            panic!("simulated worker failure mid-send");
        }));
    });
    // Worker B completes normally; the drain must still pick up its
    // observation.
    let ctx_b = Arc::clone(&ctx);
    let dir_b = dir.clone();
    let normal_handle = thread::spawn(move || {
        ctx_b.observe_directory(dir_b, &[]);
    });

    panic_handle.join().expect("panic worker joined");
    normal_handle.join().expect("normal worker joined");

    let owned = Arc::try_unwrap(ctx).expect("workers released their clones");
    owned.begin_directory(make_segment(&[]));
    owned.publish_plan_for(&dir).expect("publish plan");

    // Drain returns without blocking - the channel closes because
    // every sender clone (including the panicked worker's) was
    // dropped during stack unwinding.
    let outcome = owned
        .emit_one(RecordingDeleteFs::new())
        .expect("drain completes after worker panic");
    assert_eq!(
        outcome.stats.files, 1,
        "the survivor file is deleted exactly once"
    );
}
