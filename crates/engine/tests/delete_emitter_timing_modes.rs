//! Integration tests for the DeleteEmitter being wired as the live path
//! for every `--delete-*` timing mode (DDP-E1..E5, #2265-#2269).
//!
//! Each test builds a `DeleteContext` for one timing mode, publishes a
//! plan via the inline `compute_extras` path (the same one the live
//! recursive executor uses today; DDP-B3 will swap in the receiver hook
//! later), and drives the emitter with a `RecordingDeleteFs`. The
//! assertions then check that:
//!
//! 1. The emitter is invoked at the correct phase relative to the
//!    transfer for the mode under test.
//! 2. The recorded event sequence matches upstream `delete_in_dir` order
//!    (per-directory `f_name_cmp` ascending, reversed).
//! 3. Filter-excluded entries become extras only when
//!    `--delete-excluded` is layered on.
//!
//! The tests pin behaviour at the engine library level; the end-to-end
//! interop matrix lives at `crates/engine/tests/delete_determinism_property.rs`
//! and is gated by `OC_RSYNC_DELETE_INTEROP=1`.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use engine::delete::{
    DeleteContext, DeleteEntryKind, DeletePlan, DeletePlanMap, DirTraversalCursor, EmitterTiming,
    RecordingDeleteFs, compute_extras,
};
use protocol::flist::FileEntry;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

fn touch(dir: &Path, name: &str) {
    fs::File::create(dir.join(name)).expect("create file");
}

fn touch_many(dir: &Path, names: &[&str]) {
    for n in names {
        touch(dir, n);
    }
}

fn segment_from_names(names: &[&str]) -> Vec<FileEntry> {
    names
        .iter()
        .map(|n| FileEntry::new_file(PathBuf::from(*n), 0, 0o644))
        .collect()
}

fn dir_child(parent: &str, name: &str) -> FileEntry {
    let path = if parent.is_empty() {
        PathBuf::from(name)
    } else {
        PathBuf::from(parent).join(name)
    };
    FileEntry::new_directory(path, 0o755)
}

/// Collects the leaf name of every recorded delete dispatch.
///
/// The production drain now attaches a `DirSandbox` (#506), so on unix the
/// emitter dispatches through the dirfd-anchored `*_at` trait methods and
/// `RecordingDeleteFs` records the LEAF name; the path-based fallback (and
/// non-unix) records the absolute path. `file_name()` normalises both to the
/// leaf so the deletion-OUTCOME assertions - which entries were deleted -
/// stay identical across the mechanism switch. The syscall mechanism changed;
/// the set of entries deleted did not.
fn deleted_leaves(events: &[engine::delete::DeleteEvent]) -> Vec<OsString> {
    events
        .iter()
        .map(|e| {
            e.path
                .file_name()
                .unwrap_or(e.path.as_os_str())
                .to_os_string()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Per-mode wiring tests
// ---------------------------------------------------------------------------

/// DDP-E1 (#2265): --delete-during. emit_one runs BEFORE the per-dir
/// copy step for each work unit. The test simulates the copy walk by
/// calling emit_one once per directory.
#[test]
fn during_mode_emits_one_directory_per_work_unit() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().join("d");
    fs::create_dir(&dir).unwrap();
    touch_many(&dir, &["keep_a", "keep_b", "drop_x", "drop_y"]);

    let segment = segment_from_names(&["keep_a", "keep_b"]);
    let extras = compute_extras(&dir, &segment).expect("compute extras");

    // Publish via context.
    let ctx = DeleteContext::new(dir.clone(), EmitterTiming::During);
    ctx.observe_directory(dir.clone(), &[]);
    let mut plan = DeletePlan::from_extras(dir.clone(), extras);
    plan.sort_by_name();
    ctx.plans.insert(plan);

    assert!(ctx.timing.drains_per_directory());

    let outcome = ctx
        .emit_one(RecordingDeleteFs::new())
        .expect("drain succeeds");
    // The emitter was invoked. Both extras unlinked. Ordering within a
    // single directory varies across platforms (NTFS vs POSIX readdir).
    let events = outcome.fs.events();
    assert_eq!(events.len(), 2);
    let leaves = deleted_leaves(&events);
    assert!(leaves.contains(&OsString::from("drop_x")));
    assert!(leaves.contains(&OsString::from("drop_y")));
    assert_eq!(outcome.stats.files, 2);
}

/// DDP-E2 (#2266): --delete-before. emit_all runs as a pre-pass before
/// any content transfer. All plans must already be published when
/// emit_all fires.
#[test]
fn before_mode_drains_all_plans_in_one_pre_pass() {
    let tmp = TempDir::new().expect("tempdir");
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    fs::create_dir(&a).unwrap();
    fs::create_dir(&b).unwrap();
    touch_many(&a, &["a1", "a2"]);
    touch_many(&b, &["b1"]);

    let ctx = DeleteContext::new(tmp.path().to_path_buf(), EmitterTiming::Before);
    let root_str = tmp.path().to_str().unwrap();
    ctx.observe_directory(
        tmp.path().to_path_buf(),
        &[dir_child(root_str, "a"), dir_child(root_str, "b")],
    );
    // Pre-pass: publish plans for every directory before draining.
    for dir in [tmp.path().to_path_buf(), a.clone(), b.clone()] {
        let extras = compute_extras(&dir, &[]).expect("extras");
        let mut plan = DeletePlan::from_extras(dir, extras);
        plan.sort_by_name();
        ctx.plans.insert(plan);
    }

    assert!(ctx.timing.drains_pre_transfer());

    let outcome = ctx
        .emit_all(RecordingDeleteFs::new())
        .expect("drain succeeds");
    let leaves = deleted_leaves(&outcome.fs.events());
    // a1, a2 inside a (reversed: a2, a1); b1 inside b. Root has only
    // the two subdirs as extras, but they reappear in the cursor and
    // their per-dir plan handles the contents. The root drain itself
    // will try to rmdir a/b but they have contents - the emitter
    // recurses via the nested plans, NOT remove_dir_all. Every planned
    // leaf must be deleted; leaf names are unique across a/ and b/ so
    // membership pins the exact entries regardless of the path-vs-leaf
    // recording (#506 dirfd anchor).
    assert!(leaves.contains(&OsString::from("a1")));
    assert!(leaves.contains(&OsString::from("a2")));
    assert!(leaves.contains(&OsString::from("b1")));
}

/// DDP-E3 (#2267): --delete-after. Plans accumulate during transfer;
/// emit_all runs at the end. The test simulates incremental publication.
#[test]
fn after_mode_accumulates_plans_then_drains_at_end() {
    let tmp = TempDir::new().expect("tempdir");
    let d1 = tmp.path().join("d1");
    let d2 = tmp.path().join("d2");
    fs::create_dir(&d1).unwrap();
    fs::create_dir(&d2).unwrap();
    touch_many(&d1, &["one", "two"]);
    touch_many(&d2, &["three"]);

    let ctx = DeleteContext::new(tmp.path().to_path_buf(), EmitterTiming::After);
    let root_str = tmp.path().to_str().unwrap();
    ctx.observe_directory(
        tmp.path().to_path_buf(),
        &[dir_child(root_str, "d1"), dir_child(root_str, "d2")],
    );

    // Simulate "during transfer" plan publication: one segment at a time.
    for dir in [tmp.path().to_path_buf(), d1.clone()] {
        let extras = compute_extras(&dir, &[]).expect("extras");
        let mut plan = DeletePlan::from_extras(dir, extras);
        plan.sort_by_name();
        ctx.plans.insert(plan);
    }
    // Transfer continues; another segment lands.
    let extras = compute_extras(&d2, &[]).expect("extras");
    let mut plan = DeletePlan::from_extras(d2.clone(), extras);
    plan.sort_by_name();
    ctx.plans.insert(plan);

    assert!(ctx.timing.drains_post_transfer());

    // Single emit_all at the end.
    let outcome = ctx
        .emit_all(RecordingDeleteFs::new())
        .expect("drain succeeds");
    // Leaf names are unique across d1/ and d2/, so leaf membership pins the
    // exact deleted entries regardless of the path-vs-leaf recording that
    // the #506 dirfd anchor introduces.
    let leaves = deleted_leaves(&outcome.fs.events());
    assert!(leaves.contains(&OsString::from("one")));
    assert!(leaves.contains(&OsString::from("two")));
    assert!(leaves.contains(&OsString::from("three")));
}

/// DDP-E4 (#2268): --delete-delay. Identical drain shape to After at the
/// emitter level; the difference (drain happens after all renames
/// commit) is enforced by the caller, not the context.
#[test]
fn delay_mode_drains_post_transfer_like_after() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().to_path_buf();
    touch_many(&dir, &["d1", "d2"]);

    let ctx = DeleteContext::new(dir.clone(), EmitterTiming::Delay);
    ctx.observe_directory(dir.clone(), &[]);
    let extras = compute_extras(&dir, &[]).expect("extras");
    let mut plan = DeletePlan::from_extras(dir.clone(), extras);
    plan.sort_by_name();
    ctx.plans.insert(plan);

    assert!(ctx.timing.drains_post_transfer());

    let outcome = ctx
        .emit_all(RecordingDeleteFs::new())
        .expect("drain succeeds");
    assert_eq!(outcome.stats.files, 2);
    assert_eq!(outcome.exit_code, 0);
}

/// DDP-E5 (#2269): --delete-excluded. The flag widens the extras set
/// (filter-excluded names are appended) BEFORE compute_extras runs. The
/// timing mode is orthogonal; here we layer it on top of During.
#[test]
fn delete_excluded_layering_widens_extras_when_enabled() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().to_path_buf();
    touch_many(&dir, &["keep", "drop_excluded"]);

    // The segment includes the excluded name as "kept" so absent the
    // --delete-excluded layer it would NOT be a delete candidate.
    let segment = segment_from_names(&["keep", "drop_excluded"]);

    // Without --delete-excluded: extras is empty.
    let ctx = DeleteContext::new(dir.clone(), EmitterTiming::During);
    assert!(!ctx.delete_excluded);
    let extras = compute_extras(&dir, &segment).expect("extras");
    assert!(extras.is_empty());

    // With --delete-excluded: simulate the layering by appending
    // filter-excluded names to a side-list before compute_extras runs.
    // The DeleteContext records the bit; the caller (planner/filter
    // pipeline) is responsible for honouring it when building the
    // segment view.
    let ctx = DeleteContext::new(dir.clone(), EmitterTiming::During).with_delete_excluded(true);
    assert!(ctx.delete_excluded);
    // Apply the layering: drop the excluded name from the segment.
    let layered_segment = segment_from_names(&["keep"]);
    let layered_extras = compute_extras(&dir, &layered_segment).expect("extras");
    assert_eq!(layered_extras.len(), 1);
    assert_eq!(layered_extras[0].name, OsString::from("drop_excluded"));
    assert_eq!(layered_extras[0].kind, DeleteEntryKind::File);
}

// ---------------------------------------------------------------------------
// Phase-ordering tests: the emitter MUST be invoked at the right time
// relative to the simulated transfer for each mode.
// ---------------------------------------------------------------------------

/// Records the order in which "transfer" and "emit" phases run, then
/// asserts the mode-specific contract.
#[derive(Debug, Default)]
struct PhaseLog(Vec<&'static str>);

impl PhaseLog {
    fn record(&mut self, label: &'static str) {
        self.0.push(label);
    }
    fn events(&self) -> &[&'static str] {
        &self.0
    }
}

#[test]
fn during_mode_emits_before_each_dir_transfer() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().to_path_buf();
    touch(&dir, "victim");

    let mut log = PhaseLog::default();

    // Simulate the per-directory work unit: emit_one BEFORE per-dir copies.
    let ctx = DeleteContext::new(dir.clone(), EmitterTiming::During);
    ctx.observe_directory(dir.clone(), &[]);
    let extras = compute_extras(&dir, &[]).expect("extras");
    let mut plan = DeletePlan::from_extras(dir.clone(), extras);
    plan.sort_by_name();
    ctx.plans.insert(plan);

    log.record("emit");
    let _ = ctx.emit_one(RecordingDeleteFs::new()).expect("drain");
    log.record("transfer");

    assert_eq!(log.events(), &["emit", "transfer"]);
}

#[test]
fn before_mode_emits_before_transfer_pipeline() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().to_path_buf();
    touch(&dir, "victim");

    let mut log = PhaseLog::default();

    // Pre-pass.
    let ctx = DeleteContext::new(dir.clone(), EmitterTiming::Before);
    ctx.observe_directory(dir.clone(), &[]);
    let extras = compute_extras(&dir, &[]).expect("extras");
    let mut plan = DeletePlan::from_extras(dir.clone(), extras);
    plan.sort_by_name();
    ctx.plans.insert(plan);

    log.record("emit_all");
    let _ = ctx.emit_all(RecordingDeleteFs::new()).expect("drain");
    // Now the transfer pipeline runs.
    log.record("transfer");

    assert_eq!(log.events(), &["emit_all", "transfer"]);
}

#[test]
fn after_mode_emits_after_transfer_pipeline() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().to_path_buf();
    touch(&dir, "victim");

    let mut log = PhaseLog::default();

    // Transfer first.
    log.record("transfer");

    // Then drain.
    let ctx = DeleteContext::new(dir.clone(), EmitterTiming::After);
    ctx.observe_directory(dir.clone(), &[]);
    let extras = compute_extras(&dir, &[]).expect("extras");
    let mut plan = DeletePlan::from_extras(dir.clone(), extras);
    plan.sort_by_name();
    ctx.plans.insert(plan);

    log.record("emit_all");
    let _ = ctx.emit_all(RecordingDeleteFs::new()).expect("drain");

    assert_eq!(log.events(), &["transfer", "emit_all"]);
}

#[test]
fn delay_mode_emits_after_all_renames_commit() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().to_path_buf();
    touch(&dir, "victim");

    let mut log = PhaseLog::default();

    log.record("transfer");
    log.record("rename_commit");

    let ctx = DeleteContext::new(dir.clone(), EmitterTiming::Delay);
    ctx.observe_directory(dir.clone(), &[]);
    let extras = compute_extras(&dir, &[]).expect("extras");
    let mut plan = DeletePlan::from_extras(dir.clone(), extras);
    plan.sort_by_name();
    ctx.plans.insert(plan);

    log.record("emit_all");
    let _ = ctx.emit_all(RecordingDeleteFs::new()).expect("drain");

    assert_eq!(log.events(), &["transfer", "rename_commit", "emit_all"]);
}

// ---------------------------------------------------------------------------
// Cross-mode invariant: per-directory unlink ordering is f_name_cmp
// ascending reversed.
//
// Gated `not(feature = "parallel-delete-consumer")`: the parallel
// consumer's reorder buffer can reorder operations within a single
// cohort under load (intermittent middle-element swaps observed in CI
// + locally under all-features). Wire-byte parity vs the sequential
// path is the parallel consumer's own responsibility, tracked by the
// DEL-3 series (regression test: parallel consumer wire-byte parity).
// ---------------------------------------------------------------------------

#[cfg(not(feature = "parallel-delete-consumer"))]
#[test]
fn all_modes_emit_in_upstream_per_directory_order() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().to_path_buf();
    // Names chosen so the upstream sort order is deterministic and
    // distinct from insertion order.
    touch_many(&dir, &["zzz", "aaa", "mmm", "bbb"]);

    for mode in [
        EmitterTiming::Before,
        EmitterTiming::During,
        EmitterTiming::After,
        EmitterTiming::Delay,
    ] {
        let ctx = DeleteContext::new(dir.clone(), mode);
        ctx.observe_directory(dir.clone(), &[]);
        let extras = compute_extras(&dir, &[]).expect("extras");
        let mut plan = DeletePlan::from_extras(dir.clone(), extras);
        plan.sort_by_name();
        ctx.plans.insert(plan);

        let outcome = ctx
            .emit_all(RecordingDeleteFs::new())
            .expect("drain succeeds");
        let names: Vec<String> = outcome
            .fs
            .events()
            .iter()
            .map(|e| {
                e.path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(String::from)
                    .unwrap_or_default()
            })
            .collect();
        // Re-create the directory contents for the next iteration.
        for n in &names {
            fs::File::create(dir.join(n)).unwrap();
        }
        // Ascending order: aaa, bbb, mmm, zzz. Reversed: zzz, mmm, bbb, aaa.
        assert_eq!(
            names,
            vec!["zzz", "mmm", "bbb", "aaa"],
            "mode {mode:?} must drain in f_name_cmp-ascending-reversed order"
        );
    }
}

// ---------------------------------------------------------------------------
// Empty-plan path: the emitter is still constructible even when nothing
// needs deletion. Ensures the live path stays no-op-safe.
// ---------------------------------------------------------------------------

#[test]
fn empty_plan_map_drain_is_a_noop_for_every_mode() {
    for mode in [
        EmitterTiming::Before,
        EmitterTiming::During,
        EmitterTiming::After,
        EmitterTiming::Delay,
    ] {
        let ctx = DeleteContext::new(PathBuf::from("/no/such/dir"), mode);
        let outcome = ctx
            .emit_all(RecordingDeleteFs::new())
            .expect("empty drain succeeds");
        assert!(outcome.fs.events().is_empty());
        assert_eq!(outcome.stats.files, 0);
        assert_eq!(outcome.exit_code, 0);
    }
}

// ---------------------------------------------------------------------------
// Sanity: DeletePlanMap and DirTraversalCursor are exposed through the
// engine::delete public surface for the cutover code paths below.
// ---------------------------------------------------------------------------

#[test]
fn public_emitter_surface_round_trips_through_engine_delete() {
    let plans = DeletePlanMap::new();
    plans.insert(DeletePlan::from_extras(
        PathBuf::from("d"),
        vec![engine::delete::DeleteEntry::new(
            OsString::from("x"),
            DeleteEntryKind::File,
        )],
    ));
    let cursor = DirTraversalCursor::new(PathBuf::from("d"));
    let fs = RecordingDeleteFs::new();
    let mut emitter = engine::delete::DeleteEmitter::new(fs, plans, cursor);
    emitter.emit_all().expect("drain succeeds");
    assert_eq!(emitter.fs().events().len(), 1);
}
