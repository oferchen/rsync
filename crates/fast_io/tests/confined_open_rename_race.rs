//! A confined open through an in-tree `..` must not fail because some other
//! process renamed a file.
//!
//! `openat2(RESOLVE_BENEATH)` returns `EAGAIN` when a rename or mount anywhere
//! on the system races its resolution of a `..` (openat2(2)): the kernel can
//! no longer prove the `..` stayed beneath the anchor, so it refuses to answer.
//! That is not a verdict on the path. On a busy host it made the daemon sender
//! fail in-module `../` targets intermittently - the upstream testsuite's
//! daemon-copylinks-parent-escape, daemon-copylinks-parent-target-regression
//! and daemon-scan-cwd-desync cells failed 30-38 runs in 50 under a rename
//! load, and never against upstream, whose resolver is the per-component walk
//! (`syscall.c:3032-3115`) with no such race.
//!
//! Each cell below runs a cross-directory rename storm - cross-directory
//! renames take the global `rename_lock` the scoped lookup samples - while it
//! opens the same in-module target thousands of times - through the sender's
//! confined open, a sandbox descent, and a nested destination parent anchor.
//! Without the fallback to the walk, a fraction of those opens fail with
//! `EAGAIN`.

#![cfg(target_os = "linux")]

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use fast_io::{
    DirSandbox, LeafPolicy, LstatOutcome, lstat_via_sandbox_or_fallback, open_source_confined,
};
use tempfile::TempDir;

const OPENS: usize = 20_000;
const STORM_THREADS: usize = 2;

/// Renames a file between two directories until `stop` is set.
struct RenameStorm {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    _dir: TempDir,
}

impl RenameStorm {
    fn start() -> Self {
        let dir = TempDir::new().expect("storm dir");
        let stop = Arc::new(AtomicBool::new(false));
        let threads = (0..STORM_THREADS)
            .map(|i| {
                let here = dir.path().join(format!("a{i}"));
                let there = dir.path().join(format!("b{i}"));
                fs::create_dir(&here).expect("mkdir");
                fs::create_dir(&there).expect("mkdir");
                let (from, to) = (here.join("f"), there.join("f"));
                fs::write(&from, b"").expect("write");
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        fs::rename(&from, &to).expect("rename");
                        fs::rename(&to, &from).expect("rename back");
                    }
                })
            })
            .collect();
        Self {
            stop,
            threads,
            _dir: dir,
        }
    }
}

impl Drop for RenameStorm {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// The module tree of daemon-copylinks-parent-escape: in-module targets
/// reached through a `..`.
fn module_tree() -> TempDir {
    let root = TempDir::new().expect("module root");
    let m = root.path();
    fs::create_dir_all(m.join("sub/x")).expect("mkdir");
    fs::create_dir_all(m.join("sub/y")).expect("mkdir");
    fs::create_dir(m.join("targetdir")).expect("mkdir");
    fs::write(m.join("target.txt"), b"in-module file\n").expect("write");
    fs::write(m.join("targetdir/f.txt"), b"in-module dirfile\n").expect("write");
    symlink("../target.txt", m.join("sub/in_file")).expect("symlink");
    symlink("../targetdir", m.join("sub/in_dir")).expect("symlink");
    symlink("x/../y", m.join("sub/xy")).expect("symlink");
    fs::write(m.join("sub/y/leaf"), b"").expect("write");
    root
}

fn open_repeatedly(root: &Path, relative: &str, leaf: LeafPolicy) {
    let _storm = RenameStorm::start();
    for attempt in 0..OPENS {
        if let Err(err) = open_source_confined(root, Path::new(relative), leaf, false) {
            panic!("open {attempt} of {relative} failed under a rename load: {err}");
        }
    }
}

/// `--copy-links` on an in-module file symlink: the leaf's own target climbs.
#[test]
fn a_followed_leaf_symlink_through_dotdot_survives_a_rename_load() {
    let root = module_tree();
    open_repeatedly(root.path(), "sub/in_file", LeafPolicy::FollowConfined);
}

/// A plain open whose parent is an in-module directory symlink that climbs,
/// the shape of daemon-scan-cwd-desync (`x/jump -> ../y`).
#[test]
fn a_symlinked_parent_through_dotdot_survives_a_rename_load() {
    let root = module_tree();
    open_repeatedly(root.path(), "sub/in_dir/f.txt", LeafPolicy::Nofollow);
}

/// The destination-side descent follows an in-tree directory symlink the same
/// way; its `..` must not turn a rename elsewhere into a refusal either.
#[test]
fn a_sandbox_descent_through_dotdot_survives_a_rename_load() {
    let root = module_tree();
    let _storm = RenameStorm::start();
    for attempt in 0..OPENS {
        let mut sandbox = DirSandbox::open_root(&root.path().join("sub")).expect("open root");
        if let Err(err) = sandbox.enter(OsStr::new("xy")) {
            panic!("descent {attempt} through sub/xy failed under a rename load: {err}");
        }
    }
}

/// A multi-component destination op anchors its parent beneath the sandbox
/// root, through the same in-tree `..`. The anchor must resolve it rather than
/// refuse it, and must not degrade to a path-based op either.
#[test]
fn a_nested_parent_anchor_through_dotdot_survives_a_rename_load() {
    let root = module_tree();
    let dest = root.path().join("sub");
    let sandbox = DirSandbox::open_root(&dest).expect("open root");
    let relative = Path::new("xy/leaf");
    let full = dest.join(relative);
    let _storm = RenameStorm::start();
    for attempt in 0..OPENS {
        match lstat_via_sandbox_or_fallback(Some(&sandbox), &dest, relative, &full) {
            Ok(LstatOutcome::At(_)) => {}
            Ok(LstatOutcome::Std(_)) => {
                panic!("lstat {attempt} of xy/leaf degraded to a path-based op")
            }
            Err(err) => panic!("lstat {attempt} of xy/leaf failed under a rename load: {err}"),
        }
    }
}
