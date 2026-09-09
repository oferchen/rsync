//! A daemon session's module root bounds the per-directory merge read.
//!
//! # Why this cell exists
//!
//! Each half of this composition is already pinned on its own - `fast_io`'s
//! `confine_root_merge_alias.rs` pins the resolver against a lexically-invisible
//! escape, and its `operator_open_module_confinement.rs` pins a real daemon
//! session - but nothing pinned the two TOGETHER through the public filter API.
//! The join is what a daemon `filter = : NAME` newly depends on, and it runs
//! through three separate decisions:
//!
//! 1. `install_daemon_session` takes the confinement root from
//!    `ModuleState.root`. It is NOT the CLI `--confine-root` slot, which stays
//!    `None` on this arm - reading that field and concluding the daemon is
//!    unconfined is the available misreading.
//! 2. `FilterChain::enter_directory` opens the merge file through
//!    `merge_open::read_to_string`, which on unix is
//!    `fast_io::operator_read_to_string_confined` - the ownership walk AND
//!    `abspath_outside_confinement`.
//! 3. A refused merge file is SKIPPED, not fatal, so the observable is that the
//!    escaping file's rules never took effect - not an error return.
//!
//! This is the cell that would catch a future change to the daemon arm of
//! `Activation::root()`: were it to stop taking the module root, the escaping
//! merge file would be read and `bait.txt` would start being denied.
//!
//! # The companion control is load-bearing
//!
//! A resolver that refused EVERY merge file would satisfy the escape assertion
//! by doing nothing at all. [`an_in_tree_merge_file_is_still_read`] keeps an
//! ordinary in-module `.rsync-filter` green through the same session, so the
//! pair distinguishes "refused the escape" from "refused everything".
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/exclude.c:1668-1684` `parse_filter_file()` - wraps the
//!   merge-file open in `operator_path_resolve = 1`, scoped by
//!   `if (!daemon_config_filter_file)`: a PER-DIRECTORY merge is confined, while
//!   the daemon's own `filter` / `include from` / `exclude from` parameters are
//!   deliberately EXEMPT. The two directions are opposite on purpose.
//! - `rsync-3.5.0/syscall.c:308-310` - a daemon seeds `abspath` from
//!   `module_dir`, which is what `ModuleState.root` carries here.
//! - `rsync-3.5.0/exclude.c:1682` - `parse_filter_file()` runs without
//!   `XFLG_FATAL_ERRORS`, so an unopenable merge file is skipped.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use fast_io::confinement::{ModuleInsecureLinks, ModuleState};
use filters::{DirMergeConfig, FilterChain};
use tempfile::TempDir;

/// The confinement root is process-global, mirroring upstream's `module_dir`,
/// so the cells must not interleave.
static SESSION: Mutex<()> = Mutex::new(());

/// Serves `module` as a non-chrooted daemon module with confinement engaged.
fn serve_module(module: &Path) -> MutexGuard<'static, ()> {
    let guard = SESSION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    fast_io::confinement::install_daemon_session(ModuleState {
        root: Some(module.to_path_buf()),
        chrooted: false,
        selected: true,
        insecure_links: ModuleInsecureLinks::from_module_config(false),
    });
    guard
}

/// A module holding `sub/`, plus an out-of-module file of filter rules.
struct Fixture {
    _tmp: TempDir,
    module: PathBuf,
    sub: PathBuf,
    outside_rules: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = TempDir::new().expect("tempdir");
    // The module is a level BELOW the tempdir root, so `outside_rules` is a
    // genuine sibling outside the confinement root rather than an ancestor.
    let module = tmp.path().join("module");
    let sub = module.join("sub");
    fs::create_dir_all(&sub).expect("create module tree");
    let outside_rules = tmp.path().join("outside-rules");
    fs::write(&outside_rules, b"- bait.txt\n").expect("write outside rules");
    Fixture {
        _tmp: tmp,
        module,
        sub,
        outside_rules,
    }
}

/// A chain carrying exactly one per-directory merge config, as a daemon
/// `filter = : .rsync-filter` produces.
fn dir_merge_chain() -> FilterChain {
    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));
    assert!(
        chain.has_per_dir_merge(),
        "the fixture must actually register a per-directory merge"
    );
    chain
}

/// THE PIN. `sub/.rsync-filter` is a symlink out of the module. The ownership
/// walk follows it - it is owned by our own euid, which upstream treats as
/// trusted - so the refusal has to come from the module root. If it does not,
/// the out-of-module file becomes filter-rule text and `bait.txt` is denied.
#[test]
fn a_merge_file_symlinked_out_of_the_module_is_not_read() {
    let fixture = fixture();
    symlink(&fixture.outside_rules, fixture.sub.join(".rsync-filter"))
        .expect("plant the escaping merge file");

    let _session = serve_module(&fixture.module);
    let mut chain = dir_merge_chain();
    // A refused merge file is skipped rather than fatal (exclude.c:1682), so
    // this must not error - the divergence shows up in the match below.
    let guard = chain
        .enter_directory(&fixture.sub)
        .expect("a refused merge file is skipped, not fatal");

    assert!(
        chain.allows(Path::new("bait.txt"), false),
        "the escaping merge file's rules must never take effect; reading it \
         would let a peer-named path outside the module become filter rules"
    );

    chain.leave_directory(guard);
}

/// THE INSTRUMENT CHECK. The identical symlink, read under a session whose root
/// is widened to the tempdir so the target is INSIDE it, must be READ.
///
/// Without this, the escape cell could be satisfied by a resolver that simply
/// never follows a symlinked merge file, or by one that fails the open for any
/// unrelated reason - both of which would keep `bait.txt` allowed while proving
/// nothing about the confinement ROOT. Because the only difference between this
/// cell and the escape cell is `ModuleState.root`, a pass here plus a pass there
/// localises the refusal to the root check.
#[test]
fn the_same_symlink_is_read_when_the_root_is_widened_to_contain_it() {
    let fixture = fixture();
    symlink(&fixture.outside_rules, fixture.sub.join(".rsync-filter"))
        .expect("plant the escaping merge file");

    // Root at the tempdir, one level ABOVE the module, so `outside-rules` is
    // inside the confinement root.
    let widened = fixture
        .module
        .parent()
        .expect("module has a parent")
        .to_path_buf();
    let _session = serve_module(&widened);
    let mut chain = dir_merge_chain();
    let guard = chain.enter_directory(&fixture.sub).expect("enter sub");

    assert!(
        !chain.allows(Path::new("bait.txt"), false),
        "the symlinked merge file IS followed and read when its target lies \
         inside the confinement root - so the escape cell's refusal is the \
         root check, not a blanket refusal to follow symlinks"
    );

    chain.leave_directory(guard);
}

/// THE COMPANION. The same session, the same chain, an ordinary in-module merge
/// file - which must still be read, or the cell above proves nothing.
#[test]
fn an_in_tree_merge_file_is_still_read() {
    let fixture = fixture();
    fs::write(fixture.sub.join(".rsync-filter"), b"- bait.txt\n").expect("write in-tree rules");

    let _session = serve_module(&fixture.module);
    let mut chain = dir_merge_chain();
    let guard = chain.enter_directory(&fixture.sub).expect("enter sub");
    assert_eq!(chain.scope_depth(), 1, "the merge file must have been read");

    assert!(
        !chain.allows(Path::new("bait.txt"), false),
        "an in-module merge file must still be read; an over-refusing resolver \
         would satisfy the escape cell while breaking every ordinary module"
    );

    chain.leave_directory(guard);
}
