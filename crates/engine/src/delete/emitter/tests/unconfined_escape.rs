//! CF-P0a / CF-P0b: the path-based [`RealDeleteFs`] methods escape the tree.
//!
//! [`RealDeleteFs`]'s six path-based methods are bare `std::fs` calls
//! (`fs.rs` `unlink_file` / `rmdir` / `unlink_symlink` / `unlink_device`
//! / `unlink_special` / `remove_dir_all`). They are what the emitter
//! issues whenever no [`fast_io::DirSandbox`] has been wired to it, so
//! every intermediate component of the deletion path is resolved by the
//! kernel with symlink following fully enabled.
//!
//! These tests are a RED baseline, not a regression guard: each one
//! plants a symlink mid-path and then asserts that the entry the method
//! actually destroyed lives OUTSIDE the deletion tree. They document the
//! defect the confined-fallback work has to close, so that work cannot
//! land vacuously.
//!
//! Every escape test is paired with a non-vacuity companion that runs
//! the same method against a genuine in-tree directory. Without the
//! companion an escape assertion would also pass if the fixture were
//! simply unable to remove anything at all.
//!
//! upstream: `rsync-3.5.0/syscall.c:2896-2961` `ds_descend()` - upstream
//! resolves a deletion path per component and refuses an absolute
//! symlink target rather than walking through it.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::super::{DeleteFs, RealDeleteFs};

/// The two-tree fixture every test in this module shares.
///
/// `tree` is the directory the emitter believes it is deleting inside.
/// `outside` is a sibling that no deletion may ever touch. `tree/hop` is
/// a symlink to `outside`, standing in for a component an attacker
/// flipped between the emitter's decision to delete and the syscall.
struct Fixture {
    _base: TempDir,
    tree: PathBuf,
    outside: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let base = TempDir::new().expect("tempdir");
        let tree = base.path().join("tree");
        let outside = base.path().join("outside");
        fs::create_dir(&tree).expect("mkdir tree");
        fs::create_dir(&outside).expect("mkdir outside");
        plant_symlink(&outside, &tree.join("hop"));
        fs::create_dir(tree.join("real")).expect("mkdir real");
        Self {
            _base: base,
            tree,
            outside,
        }
    }

    /// Path the emitter would issue: it traverses the planted symlink.
    fn through_symlink(&self, name: &str) -> PathBuf {
        self.tree.join("hop").join(name)
    }

    /// Same shape without a symlink anywhere in it - the control.
    fn in_tree(&self, name: &str) -> PathBuf {
        self.tree.join("real").join(name)
    }

    fn plant_file(&self, dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, b"victim").expect("plant file");
        path
    }
}

/// Plant a symlink at `link` pointing at `target`, asserting the result
/// really is one.
///
/// The assertion pins the fixture shape rather than trusting it: a
/// mutation that degrades the planted link to an ordinary file must make
/// the escape tests fail, not quietly change what they measure.
fn plant_symlink(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).expect("plant symlink");
    assert!(
        link.symlink_metadata()
            .expect("stat the planted link")
            .file_type()
            .is_symlink(),
        "fixture broken: {} is not a symlink",
        link.display()
    );
}

/// Assert `victim` is gone while its parent survives, i.e. the call
/// removed exactly the out-of-tree entry rather than failing outright.
fn assert_escaped(fixture: &Fixture, victim: &Path) {
    assert!(
        !victim.exists(),
        "no escape: {} survived the unconfined delete",
        victim.display()
    );
    assert!(
        fixture.outside.is_dir(),
        "fixture broken: the out-of-tree directory itself vanished"
    );
}

/// `unlink_file` walks `tree/hop` into `outside` and removes the file
/// there. The emitter asked to delete something under `tree`.
#[test]
fn unlink_file_escapes_through_a_symlinked_parent() {
    let fixture = Fixture::new();
    let victim = fixture.plant_file(&fixture.outside, "victim");

    RealDeleteFs
        .unlink_file(&fixture.through_symlink("victim"))
        .expect("the unconfined unlink follows the planted symlink");

    assert_escaped(&fixture, &victim);
}

/// Non-vacuity companion: the same call against a real in-tree parent
/// removes the in-tree file and leaves the out-of-tree one alone.
#[test]
fn unlink_file_without_a_symlink_stays_in_the_tree() {
    let fixture = Fixture::new();
    let bystander = fixture.plant_file(&fixture.outside, "victim");
    let target = fixture.plant_file(&fixture.tree.join("real"), "victim");

    RealDeleteFs
        .unlink_file(&fixture.in_tree("victim"))
        .expect("an ordinary in-tree unlink");

    assert!(
        !target.exists(),
        "the in-tree file should have been removed"
    );
    assert!(
        bystander.exists(),
        "nothing outside the tree may be touched"
    );
}

/// `rmdir` follows the same planted component and removes the empty
/// directory in the out-of-tree sibling.
#[test]
fn rmdir_escapes_through_a_symlinked_parent() {
    let fixture = Fixture::new();
    let victim = fixture.outside.join("victim");
    fs::create_dir(&victim).expect("plant victim dir");

    RealDeleteFs
        .rmdir(&fixture.through_symlink("victim"))
        .expect("the unconfined rmdir follows the planted symlink");

    assert_escaped(&fixture, &victim);
}

/// Non-vacuity companion for [`rmdir_escapes_through_a_symlinked_parent`].
#[test]
fn rmdir_without_a_symlink_stays_in_the_tree() {
    let fixture = Fixture::new();
    let bystander = fixture.outside.join("victim");
    fs::create_dir(&bystander).expect("plant bystander dir");
    let target = fixture.tree.join("real").join("victim");
    fs::create_dir(&target).expect("plant target dir");

    RealDeleteFs
        .rmdir(&fixture.in_tree("victim"))
        .expect("an ordinary in-tree rmdir");

    assert!(!target.exists(), "the in-tree dir should have been removed");
    assert!(
        bystander.exists(),
        "nothing outside the tree may be touched"
    );
}

/// `unlink_symlink` removes the out-of-tree symlink itself. The leaf is
/// not followed - the escape is entirely in the walk to the parent.
#[test]
fn unlink_symlink_escapes_through_a_symlinked_parent() {
    let fixture = Fixture::new();
    let victim = fixture.outside.join("victim");
    plant_symlink(Path::new("/dev/null"), &victim);

    RealDeleteFs
        .unlink_symlink(&fixture.through_symlink("victim"))
        .expect("the unconfined unlink follows the planted symlink");

    assert!(
        victim.symlink_metadata().is_err(),
        "no escape: the out-of-tree symlink survived"
    );
    assert!(fixture.outside.is_dir(), "fixture broken");
}

/// `unlink_device` escapes the same way. `RealDeleteFs` routes every
/// file-like kind to `fs::remove_file`, so the victim's inode type is
/// irrelevant to the defect: what escapes is the path walk. The fixture
/// uses a regular file because `mknod(2)` needs privileges the test
/// suite does not have.
#[test]
fn unlink_device_escapes_through_a_symlinked_parent() {
    let fixture = Fixture::new();
    let victim = fixture.plant_file(&fixture.outside, "victim");

    RealDeleteFs
        .unlink_device(&fixture.through_symlink("victim"))
        .expect("the unconfined unlink follows the planted symlink");

    assert_escaped(&fixture, &victim);
}

/// `unlink_special` escapes the same way, here against a real FIFO so
/// at least one non-regular kind is exercised end to end.
#[test]
fn unlink_special_escapes_through_a_symlinked_parent() {
    let fixture = Fixture::new();
    let victim = fixture.outside.join("victim");
    mkfifo(&victim);

    RealDeleteFs
        .unlink_special(&fixture.through_symlink("victim"))
        .expect("the unconfined unlink follows the planted symlink");

    assert_escaped(&fixture, &victim);
}

/// The sharpest of the six: `remove_dir_all` walks the planted symlink
/// and then recursively destroys a whole out-of-tree subtree, contents
/// and all. The sibling doc at
/// `fast_io::dir_sandbox::at_syscalls::unlink` already concedes that
/// this recursive fallback "is vulnerable to the symlink-swap class the
/// carrier closes"; this is that concession made observable.
#[test]
fn remove_dir_all_escapes_through_a_symlinked_parent() {
    let fixture = Fixture::new();
    let victim = fixture.outside.join("victim");
    fs::create_dir(&victim).expect("plant victim dir");
    fs::create_dir(victim.join("nested")).expect("plant nested dir");
    let payload = victim.join("nested").join("payload");
    fs::write(&payload, b"irreplaceable").expect("plant payload");

    RealDeleteFs
        .remove_dir_all(&fixture.through_symlink("victim"))
        .expect("the unconfined recursive delete follows the planted symlink");

    assert!(
        !payload.exists(),
        "no escape: the out-of-tree payload survived"
    );
    assert_escaped(&fixture, &victim);
}

/// Non-vacuity companion for
/// [`remove_dir_all_escapes_through_a_symlinked_parent`].
#[test]
fn remove_dir_all_without_a_symlink_stays_in_the_tree() {
    let fixture = Fixture::new();
    let bystander = fixture.outside.join("victim");
    fs::create_dir(&bystander).expect("plant bystander dir");
    fs::write(bystander.join("payload"), b"irreplaceable").expect("plant payload");
    let target = fixture.tree.join("real").join("victim");
    fs::create_dir(&target).expect("plant target dir");
    fs::write(target.join("payload"), b"expendable").expect("plant payload");

    RealDeleteFs
        .remove_dir_all(&fixture.in_tree("victim"))
        .expect("an ordinary in-tree recursive delete");

    assert!(!target.exists(), "the in-tree dir should have been removed");
    assert!(
        bystander.join("payload").exists(),
        "nothing outside the tree may be touched"
    );
}

/// Create a FIFO at `path`.
///
/// `engine` denies unsafe code, so this shells out to `mkfifo(1)` rather
/// than calling `libc::mkfifo`. The node only has to exist; the test
/// cares about the path walk, not about opening it.
fn mkfifo(path: &Path) {
    let status = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("spawn mkfifo");
    assert!(status.success(), "mkfifo failed for {}", path.display());
    assert!(
        path.symlink_metadata().is_ok(),
        "mkfifo produced no node at {}",
        path.display()
    );
}
