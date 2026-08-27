//! CF-P2a: the daemon-shaped delete escape, and the arm it actually takes.
//!
//! The sibling [`super::unconfined_escape`] module pins the shape where
//! no [`fast_io::DirSandbox`] is attached at all. This module pins the
//! other, more consequential route to the same path-based methods: a
//! sandbox IS attached, and the per-plan `open_dir_at` walk *refuses*
//! the plan directory because one of its components is a planted
//! symlink. `DeleteEmitter::open_plan_dirfd` discards that refusal with
//! `.ok()`, leaves `parent_fd` as `None`, and every entry in the plan
//! then dispatches through the path-based methods - which resolve the
//! very symlink the sandbox open just rejected.
//!
//! That is the defect stated concretely: the drop to an unconfined
//! syscall is keyed on a runtime error, and the error it keys on is the
//! confinement working.
//!
//! upstream: `rsync-3.5.0/syscall.c:658` `do_unlink_at()` has three
//! arms. Arm 1 is the policy gate off. Arm 2 is the gate on with the
//! walk succeeding. Arm 3 is the gate on with the walk failing, and
//! arm 3 is an error - never a plain path syscall.
//!
//! Every test here installs (or deliberately does not install) the
//! process-global confinement session, so each one must run in its own
//! process. `cargo nextest` runs one process per test; do not run this
//! module under `cargo test`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{fs, io};

use fast_io::DirSandbox;
use tempfile::TempDir;

use super::super::super::{
    DeleteEntry, DeleteEntryKind, DeletePlan, DeletePlanMap, DirTraversalCursor,
};
use super::super::{DeleteEmitter, RealDeleteFs, open_dir_at};

/// A served module root with one real sub-directory and one symlinked
/// component pointing at a sibling tree the delete must never reach.
///
/// `module` stands in for the daemon's module root - the directory the
/// [`DirSandbox`] is opened on and the root a `ModuleState` names.
/// `module/hop` is the planted symlink; `module/real` is the control.
struct DaemonFixture {
    _base: TempDir,
    module: PathBuf,
    outside: PathBuf,
}

impl DaemonFixture {
    fn new() -> Self {
        let base = TempDir::new().expect("tempdir");
        let module = base.path().join("module");
        let outside = base.path().join("outside");
        fs::create_dir(&module).expect("mkdir module");
        fs::create_dir(&outside).expect("mkdir outside");
        fs::create_dir(module.join("real")).expect("mkdir real");

        std::os::unix::fs::symlink(&outside, module.join("hop")).expect("plant symlink");
        assert!(
            module
                .join("hop")
                .symlink_metadata()
                .expect("stat hop")
                .file_type()
                .is_symlink(),
            "fixture is inert unless module/hop really is a symlink",
        );

        Self {
            _base: base,
            module,
            outside,
        }
    }

    fn sandbox(&self) -> Arc<DirSandbox> {
        Arc::new(DirSandbox::open_root(&self.module).expect("open module sandbox"))
    }

    /// Runs one delete of `victim` under `plan_dir`, with a sandbox
    /// attached exactly as the production delete context attaches one.
    fn drain(&self, plan_dir: &Path) -> (DeleteEmitter<RealDeleteFs>, io::Result<()>) {
        let plans = DeletePlanMap::new();
        plans.insert(DeletePlan::from_extras(
            plan_dir.to_path_buf(),
            vec![DeleteEntry::new("victim".into(), DeleteEntryKind::File)],
        ));
        let cursor = DirTraversalCursor::new(plan_dir.to_path_buf());
        let mut emitter = DeleteEmitter::new(RealDeleteFs, plans, cursor)
            .with_sandbox_rooted(self.sandbox(), self.module.clone());
        let outcome = emitter.emit_all();
        (emitter, outcome)
    }
}

/// Installs the daemon session the production daemon installs, naming
/// `module` as the served module root.
fn install_module_session(module: &Path) {
    fast_io::confinement::install_daemon_session(fast_io::confinement::ModuleState {
        root: Some(module.to_path_buf()),
        chrooted: false,
        selected: true,
        insecure_links: fast_io::confinement::ModuleInsecureLinks::default(),
    });
}

/// The instrumentation the whole module rests on: which arm does the
/// emitter take?
///
/// `open_plan_dirfd` is `Some(fd)` exactly when `open_dir_at` succeeds.
/// This asserts both directions on one fixture, so neither answer can be
/// an artefact of a tree that simply cannot be opened.
#[test]
fn the_planted_symlink_makes_the_sandbox_open_refuse_and_a_real_dir_makes_it_succeed() {
    let fixture = DaemonFixture::new();
    let sandbox = fixture.sandbox();

    let refused = open_dir_at(sandbox.root_dirfd(), Path::new("hop"));
    let err = refused.expect_err("O_NOFOLLOW must refuse the planted symlink component");
    // `open_dir_at` passes `O_DIRECTORY | O_NOFOLLOW`. Linux reports the
    // O_DIRECTORY violation first and yields ENOTDIR; a platform that
    // checks O_NOFOLLOW first yields ELOOP. Accept either, and note what
    // that ambiguity costs: ENOTDIR is exactly the errno a plain
    // non-directory component would produce, so the caller cannot tell a
    // confinement refusal from an ordinary shape mismatch. That is
    // upstream's objection to keying the fallback on errno at all.
    assert!(
        matches!(err.raw_os_error(), Some(libc::ENOTDIR | libc::ELOOP)),
        "the refusal must be the O_DIRECTORY/O_NOFOLLOW refusal, not an unrelated errno: {err}",
    );

    open_dir_at(sandbox.root_dirfd(), Path::new("real"))
        .expect("a genuine in-tree directory must still open through the sandbox");
}

/// The escape, on the daemon shape. A sandbox is attached; the sandbox
/// open refuses; the emitter falls through to the path-based method and
/// destroys a file outside the module root.
#[test]
fn a_refused_sandbox_open_drops_the_delete_onto_the_unconfined_path() {
    let fixture = DaemonFixture::new();
    let victim = fixture.outside.join("victim");
    fs::write(&victim, b"do-not-touch").expect("plant victim");

    let (emitter, outcome) = fixture.drain(&fixture.module.join("hop"));
    outcome.expect("the default error policy never aborts the drain");

    assert!(
        !victim.exists(),
        "CF-P2a RED baseline: the delete escaped the module root through the planted symlink",
    );
    assert_eq!(
        emitter.stats().files,
        1,
        "the escape is a counted, successful unlink - not a swallowed failure",
    );
}

/// The same fixture with the module session installed. The routed
/// path-based method now refuses, and the file outside the module root
/// survives.
#[test]
fn the_module_session_makes_the_refused_sandbox_open_fail_closed() {
    let fixture = DaemonFixture::new();
    let victim = fixture.outside.join("victim");
    fs::write(&victim, b"do-not-touch").expect("plant victim");
    install_module_session(&fixture.module);

    let (emitter, outcome) = fixture.drain(&fixture.module.join("hop"));
    outcome.expect("a refusal is a per-entry io error, not a fatal abort");

    assert!(
        victim.exists(),
        "the delete must not reach outside the module root once confinement is live",
    );
    assert_eq!(emitter.stats().files, 0, "nothing was unlinked",);
    assert_ne!(
        emitter.io_error(),
        0,
        "the refusal surfaces as a non-fatal io error, upstream's third arm",
    );
}

/// Availability control for the same session: an ordinary in-tree
/// delete under an installed module root must still work. Without this
/// the refusal test above would also pass if confinement simply broke
/// every delete.
#[test]
fn an_in_tree_delete_still_succeeds_under_the_module_session() {
    let fixture = DaemonFixture::new();
    let victim = fixture.module.join("real").join("victim");
    fs::write(&victim, b"delete-me").expect("plant victim");
    install_module_session(&fixture.module);

    let (emitter, outcome) = fixture.drain(&fixture.module.join("real"));
    outcome.expect("drain succeeds");

    assert!(!victim.exists(), "the in-tree delete must still land");
    assert_eq!(emitter.stats().files, 1);
}
