//! Unit tests for [`DeleteContext`], [`EmitterTiming`], and the
//! cursor-channel drain protocol.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use protocol::flist::FileEntry;
use tempfile::TempDir;

use super::super::emitter::RecordingDeleteFs;
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
    // #506: the production emit path now dispatches through the SEC-1.q
    // dirfd anchor on unix, so `RecordingDeleteFs` records the leaf-only
    // name; the path-based fallback records the absolute path. Either way
    // the entry identified is "drop" and the stats/outcome are identical.
    assert_eq!(recorded_leaf(&events[0].path), OsStr::new("drop"));
    assert_eq!(events[0].kind, DeleteEntryKind::File);
    assert_eq!(outcome.stats.files, 1);
    assert_eq!(outcome.exit_code, 0);
}

/// Returns the leaf name of a `RecordingDeleteFs` event path.
///
/// The production emit path dispatches through the SEC-1.q dirfd anchor on
/// unix (recording the leaf only) and through the path-based methods on the
/// fallback and non-unix (recording the absolute path). `file_name()`
/// normalises both to the leaf so assertions stay mechanism-agnostic while
/// still pinning WHICH entry was deleted (Rule 9).
fn recorded_leaf(path: &Path) -> &OsStr {
    path.file_name().unwrap_or(path.as_os_str())
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
    // #506: leaf-only under the dirfd anchor, absolute under the path
    // fallback; compare on leaf names so the assertion is mechanism-agnostic.
    let leaves: Vec<OsString> = outcome
        .fs
        .events()
        .iter()
        .map(|e| recorded_leaf(&e.path).to_os_string())
        .collect();
    assert!(leaves.iter().any(|n| n == OsStr::new("x")));
    assert!(leaves.iter().any(|n| n == OsStr::new("y")));
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
    let events = outcome.fs.events();
    assert_eq!(events.len(), 1);
    // #506: leaf-only under the SEC-1.q dirfd anchor, absolute under the
    // path-based fallback; assert the identified entry and kind.
    assert_eq!(recorded_leaf(&events[0].path), OsStr::new("victim"));
    assert_eq!(events[0].kind, DeleteEntryKind::File);
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

/// DML-4 fixture: builds a delete context over `root` with `n` sibling
/// subdirectories, each holding exactly one uniquely named extra file
/// (`trashNNN`) that the drain must remove. The root keeps every
/// subdirectory (they appear in its segment), so it plans zero extras and
/// the total extras count equals `n`. Every cohort carries a single op, so
/// the parallel consumer's intra-cohort `par_iter` cannot reorder and the
/// cross-cohort order is the deterministic cursor traversal - making a
/// leaf-for-leaf comparison of the two drain paths reproducible rather than
/// racy. `n` is kept below the reorder buffer's 64-cohort cap by callers.
#[cfg(feature = "parallel-delete-consumer")]
fn build_multidir_ctx(root: &Path, n: usize) -> DeleteContext {
    let ctx = DeleteContext::new(root.to_path_buf(), EmitterTiming::Before);

    let mut root_children = Vec::with_capacity(n);
    let mut root_segment = Vec::with_capacity(n);
    for i in 0..n {
        let name = format!("sub{i:03}");
        let sub = root.join(&name);
        fs::create_dir(&sub).unwrap();
        touch(&sub, &format!("trash{i:03}"));
        root_children.push(dir_child(root.to_str().unwrap(), &name));
        root_segment.push(flist_dir(&name));
    }

    ctx.observe_directory(root.to_path_buf(), &root_children);
    ctx.begin_directory(root_segment);
    ctx.publish_plan_for(root).expect("publish root plan");

    for i in 0..n {
        let sub = root.join(format!("sub{i:03}"));
        ctx.begin_directory(make_segment(&[]));
        ctx.publish_plan_for(&sub).expect("publish sub plan");
    }

    ctx
}

/// Collects a drain outcome's recorded events as `(leaf, kind)` pairs in
/// emission order. Normalises the dirfd-anchor leaf and the path-based
/// fallback to the same leaf so the comparison is mechanism-agnostic.
#[cfg(feature = "parallel-delete-consumer")]
fn event_sequence(
    outcome: &super::DrainOutcome<RecordingDeleteFs>,
) -> Vec<(OsString, DeleteEntryKind)> {
    outcome
        .fs
        .events()
        .iter()
        .map(|e| (recorded_leaf(&e.path).to_os_string(), e.kind))
        .collect()
}

/// DML-4 (a) parity: the sequential fast path and the parallel consumer
/// must produce byte-identical delete events, stats, and exit code for the
/// same input - the correctness contract that lets the drain skip the
/// pipeline below the threshold. Drives BOTH paths directly on two
/// identical fixtures so the comparison never depends on the host's rayon
/// thread count. Each cohort holds a single, uniquely named op, so the
/// cross-cohort cursor order fully determines the event sequence and the
/// leaf-for-leaf equality is reproducible.
#[cfg(feature = "parallel-delete-consumer")]
#[test]
fn fast_and_parallel_paths_emit_identical_events() {
    // 50 single-extra dirs (51 cohorts incl. root) stays under the reorder
    // buffer's 64-cohort cap while still exercising cross-cohort ordering.
    const N: usize = 50;

    let tmp_seq = tempfile::tempdir().expect("tempdir");
    let tmp_par = tempfile::tempdir().expect("tempdir");
    let ctx_seq = build_multidir_ctx(tmp_seq.path(), N);
    let ctx_par = build_multidir_ctx(tmp_par.path(), N);
    assert_eq!(ctx_seq.plans.total_extras_count(), N);

    let parts_seq = ctx_seq.into_drain_parts().expect("seq drain parts");
    let out_seq = DeleteContext::emit_sequential_from_parts(parts_seq, RecordingDeleteFs::new())
        .expect("fast-path drain");

    let parts_par = ctx_par.into_drain_parts().expect("par drain parts");
    let out_par = DeleteContext::emit_parallel_from_parts(parts_par, RecordingDeleteFs::new())
        .expect("parallel drain");

    assert_eq!(out_seq.stats, out_par.stats, "stats must be identical");
    assert_eq!(out_seq.stats.files as usize, N, "every extra is deleted");
    assert_eq!(out_seq.exit_code, out_par.exit_code, "exit code identical");
    assert_eq!(
        event_sequence(&out_seq),
        event_sequence(&out_par),
        "fast path and parallel path must emit an identical event order",
    );
    assert_eq!(out_seq.fs.events().len(), N);
}

/// DML-4 (a) routing: `emit_one` sends a sub-threshold workload down the
/// sequential fast path and an at-or-above-threshold workload down the
/// parallel consumer, and either way deletes exactly the right entries.
/// Uses a single directory (one cohort, many ops) so the >= 64 case clears
/// the reorder-buffer cohort cap; intra-cohort `par_iter` may reorder the
/// parallel run, so the deletion SET is compared, while the routing
/// decision itself is pinned on the pure predicate.
#[cfg(feature = "parallel-delete-consumer")]
#[test]
fn emit_one_routes_small_and_large_transfers() {
    use super::core::should_use_fast_path;

    fn drain_single_dir(file_count: usize) -> super::DrainOutcome<RecordingDeleteFs> {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_path_buf();
        for i in 0..file_count {
            touch(&dir, &format!("f{i:03}"));
        }
        let ctx = DeleteContext::new(dir.clone(), EmitterTiming::During);
        ctx.observe_directory(dir.clone(), &[]);
        ctx.begin_directory(make_segment(&[]));
        ctx.publish_plan_for(&dir).expect("publish plan");
        assert_eq!(ctx.plans.total_extras_count(), file_count);
        ctx.emit_one(RecordingDeleteFs::new()).expect("drain")
    }

    // Small transfer: below the threshold -> fast path.
    assert!(should_use_fast_path(10, 2));
    let small = drain_single_dir(10);
    assert_eq!(small.stats.files, 10);
    assert_eq!(small.exit_code, 0);
    let mut small_leaves: Vec<OsString> = small
        .fs
        .events()
        .iter()
        .map(|e| recorded_leaf(&e.path).to_os_string())
        .collect();
    small_leaves.sort();
    let small_expected: Vec<OsString> = (0..10)
        .map(|i| OsString::from(format!("f{i:03}")))
        .collect();
    assert_eq!(small_leaves, small_expected);

    // Large transfer: at the threshold -> parallel consumer.
    assert!(!should_use_fast_path(64, 2));
    let large = drain_single_dir(64);
    assert_eq!(large.stats.files, 64);
    assert_eq!(large.exit_code, 0);
    let mut large_leaves: Vec<OsString> = large
        .fs
        .events()
        .iter()
        .map(|e| recorded_leaf(&e.path).to_os_string())
        .collect();
    large_leaves.sort();
    let large_expected: Vec<OsString> = (0..64)
        .map(|i| OsString::from(format!("f{i:03}")))
        .collect();
    assert_eq!(large_leaves, large_expected);
}

/// DML-4 (b): boundary parity at the threshold. 63 extras stay on the fast
/// path, 65 cross to the parallel path, and 64 (the threshold itself) is
/// the first parallel value. Thread starvation (`< 2` rayon workers) forces
/// the fast path regardless of size, matching the "no spare worker to
/// parallelise onto" clause.
#[cfg(feature = "parallel-delete-consumer")]
#[test]
fn should_use_fast_path_at_threshold_boundary() {
    use super::core::{SMALL_DIR_FAST_PATH_THRESHOLD, should_use_fast_path};

    assert_eq!(SMALL_DIR_FAST_PATH_THRESHOLD, 64);
    assert!(should_use_fast_path(63, 2), "63 < 64 -> fast path");
    assert!(!should_use_fast_path(65, 2), "65 >= 64 -> parallel path");
    assert!(
        !should_use_fast_path(64, 2),
        "64 == threshold -> parallel path"
    );
    // Single-threaded runtime cannot parallelise: fast path at any size.
    assert!(should_use_fast_path(65, 1), "one thread -> fast path");
    assert!(
        should_use_fast_path(1_000_000, 1),
        "one thread -> fast path"
    );
}

/// #506 (production wiring): the drain built by `DeleteContext` now attaches
/// a `DirSandbox` opened at the delete root, so the emitter dispatches every
/// unlink through the SEC-1.q dirfd-anchored `*_at` syscalls rather than the
/// TOCTOU-prone path-based `std::fs::remove_*`. `RecordingDeleteFs` records
/// the leaf-only name for a `*_at` dispatch and the absolute path for the
/// path-based fallback, so a leaf-only recording proves the anchor is engaged
/// on the production path this task wires. WHY it matters: without the anchor,
/// a parent path component swapped for a symlink between plan-time and
/// unlink-time could redirect the delete outside the destination tree.
#[cfg(unix)]
#[test]
fn production_drain_dispatches_through_dirfd_anchor() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("sub");
    fs::create_dir(&dir).unwrap();
    touch(&dir, "drop");

    let ctx = DeleteContext::new(dir.clone(), EmitterTiming::During);
    ctx.observe_directory(dir.clone(), &[]);
    ctx.begin_directory(make_segment(&[]));
    ctx.publish_plan_for(&dir).expect("publish plan");

    let outcome = ctx
        .emit_one(RecordingDeleteFs::new())
        .expect("drain succeeds");
    let events = outcome.fs.events();
    assert_eq!(events.len(), 1);
    // A bare leaf ("drop", no separators) can only come from the
    // dirfd-anchored `unlink_file_at`; the path-based fallback would record
    // the absolute path. This is the load-bearing proof that the production
    // drain now rides the sandbox.
    assert_eq!(events[0].path, PathBuf::from("drop"));
    assert_eq!(events[0].kind, DeleteEntryKind::File);
    assert_eq!(outcome.stats.files, 1);
}

/// #506 (TOCTOU regression): the dirfd anchor the production drain attaches
/// refuses to delete through a parent component that was swapped for a
/// symlink pointing outside the destination tree. Proven deterministically
/// by program order (no threads, no sleeps): the emitter opens the delete
/// root's dirfd when the drain starts, so a later ancestor-symlink swap
/// cannot redirect the anchored `unlinkat`. Uses the real `RealDeleteFs` so
/// the live `unlinkat(2)` runs. The out-of-tree sentinel must survive; only
/// the in-tree leaf may be removed.
///
/// Gated off the parallel feature because it drives the sequential
/// `into_emitter`/`emit_all` API directly to open the dirfd before the swap;
/// the parallel consumer's equivalent anchor is covered by the DFD tests in
/// `parallel_consumer.rs`.
#[cfg(all(unix, not(feature = "parallel-delete-consumer")))]
#[test]
fn production_drain_refuses_ancestor_symlink_escape() {
    use crate::delete::RealDeleteFs;
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().expect("tempdir");
    // Delete root is `root`; the plan targets `root/victim`.
    let root = tmp.path().join("root");
    fs::create_dir(&root).unwrap();
    let victim = root.join("victim");
    touch(&root, "victim");

    // Sentinel OUTSIDE the delete tree; a redirected unlink would try to
    // remove `outside/victim`. It must be untouched.
    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let sentinel = outside.join("victim");
    File::create(&sentinel).expect("write sentinel");

    let ctx = DeleteContext::new(root.clone(), EmitterTiming::During);
    ctx.observe_directory(root.clone(), &[]);
    ctx.begin_directory(make_segment(&[]));
    ctx.publish_plan_for(&root).expect("publish plan");

    // Build the emitter (opens the root dirfd), THEN swap the delete root
    // for a symlink pointing at `outside`. The dirfd captured at build time
    // pins the real inode; the path `root/victim` now names `outside/victim`
    // for any path-based resolver, but the anchored unlink is immune.
    let emitter = ctx
        .into_emitter(RealDeleteFs)
        .expect("emitter built with sandbox");
    assert!(
        emitter.sandbox().is_some(),
        "the production drain must attach a DirSandbox at the delete root",
    );

    fs::rename(&root, tmp.path().join("root.bak")).expect("move real root aside");
    symlink(&outside, &root).expect("plant root symlink");

    let mut emitter = emitter;
    emitter.emit_all().expect("drain returns Ok");

    // The out-of-tree sentinel survives: the anchored unlink refused the
    // symlink redirect.
    assert!(
        sentinel.exists(),
        "outside sentinel must survive; the dirfd anchor refused the ancestor-symlink redirect",
    );
    // The real in-tree victim (now under root.bak, since the anchor points
    // at the original inode) was removed.
    assert!(
        !tmp.path().join("root.bak").join("victim").exists(),
        "the anchored unlink must remove the real in-tree victim",
    );
    let _ = victim; // original in-tree path, documented for clarity
}
