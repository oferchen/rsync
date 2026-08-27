//! CF-P0a / CF-P0b: the path-based [`RealDeleteFs`] methods escape the tree
//! when no confinement root has been installed.
//!
//! [`RealDeleteFs`]'s six path-based methods (`fs.rs` `unlink_file` /
//! `rmdir` / `unlink_symlink` / `unlink_device` / `unlink_special` /
//! `remove_dir_all`) now resolve through
//! [`fast_io::ConfinedFallback`], but that resolution has two halves:
//! an ownership walk and an outside-the-root test. `Activation::root()`
//! is `None` until a session is installed, so the root half cannot fire,
//! and the ownership half trusts a symlink owned by our own euid. A
//! local `--delete` therefore still walks straight through a planted
//! symlink.
//!
//! These tests assert that escape rather than hiding it. Each plants a
//! symlink mid-path and then asserts that the entry the method actually
//! destroyed lives OUTSIDE the deletion tree. They are deliberately NOT
//! flipped to a refusal: the routing alone does not close the local
//! path, and a test asserting otherwise would be describing a session
//! install that has not happened. Installing one on the local path is
//! task 1009's decision, not this module's.
//!
//! [`the_delete_routing_refuses_only_once_a_session_root_is_installed`]
//! is the other half of the pair - the same fixture with a root
//! installed, where the routing does refuse. See
//! [`super::daemon_escape`] for the daemon shape, where a module root
//! IS installed in production.
//!
//! Every escape test is paired with a non-vacuity companion that runs
//! the same method against a genuine in-tree directory. Without the
//! companion an escape assertion would also pass if the fixture were
//! simply unable to remove anything at all.
//!
//! upstream: `rsync-3.5.0/syscall.c:2891` `ds_descend()` - upstream
//! resolves a deletion path per component, and at `syscall.c:2953`
//! refuses an absolute symlink target rather than walking through it.

use std::fs;
use std::io;
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
fn unlink_file_escapes_when_no_confinement_root_is_installed() {
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
fn rmdir_escapes_when_no_confinement_root_is_installed() {
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
fn unlink_symlink_escapes_when_no_confinement_root_is_installed() {
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
fn unlink_device_escapes_when_no_confinement_root_is_installed() {
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
fn unlink_special_escapes_when_no_confinement_root_is_installed() {
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
fn remove_dir_all_escapes_when_no_confinement_root_is_installed() {
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

/// The A/B that says WHY the escape above still happens after
/// `RealDeleteFs` was routed onto `ConfinedFallback`.
///
/// `Activation::outside_root` is `self.root().is_some_and(..)`, so with no
/// session installed it is unconditionally false and the root half of the
/// ownership walk never fires. What remains is the ownership half, which
/// trusts a symlink owned by our own euid and follows it - and the fixture
/// plants exactly such a symlink. So the routed delete path resolves through
/// arm 2 and still lands outside the tree.
///
/// This is the PRECONDITION, stated rather than hidden: the routing is a
/// prerequisite that stays inert until a session is installed. Installing one
/// is task 1009's job, deliberately NOT done here - a production delete path
/// that installed its own session would be a policy decision, not a routing
/// one.
#[test]
fn the_delete_routing_refuses_only_once_a_session_root_is_installed() {
    let fixture = Fixture::new();
    let victim = fixture.plant_file(&fixture.outside, "victim");

    install_confinement_root(&fixture.tree);

    let refused = RealDeleteFs.unlink_file(&fixture.through_symlink("victim"));

    assert_refused(&refused, victim.exists(), "unlink_file");
}

/// Install a confinement session rooted at `root`.
///
/// Extracted so the six session-installed cells cannot drift apart in how they
/// activate confinement. The activation IS the fixture for those cells, and one
/// that built a subtly different shape would measure a different thing while
/// reading identically.
///
/// `install_session` writes process-global state, so these cells rely on
/// nextest's process-per-test execution to stay independent.
fn install_confinement_root(root: &Path) {
    use fast_io::confinement::{
        Activation, DaemonState, LocalInsecureLinks, Role, install_session,
    };

    install_session(&Activation {
        role: Role::Receiver,
        daemon: DaemonState::NotDaemon,
        insecure_links: LocalInsecureLinks::default(),
        confine_root: Some(root.to_path_buf()),
    });
}

/// Assert a routed method refused the escape and left the victim in place.
///
/// `survived` is supplied by the caller rather than computed from a path here:
/// the symlink cell must probe with `symlink_metadata`, because an `exists()`
/// on a link pointing at `/dev/null` follows it and answers about the target
/// instead of about the link.
fn assert_refused(result: &io::Result<()>, survived: bool, what: &str) {
    assert!(
        result.is_err(),
        "with a confinement root installed the routed {what} must refuse the escape"
    );
    assert!(
        survived,
        "the out-of-tree victim must survive a refused {what}"
    );
}

/// `rmdir` refuses the escape once a session root is installed.
#[test]
fn rmdir_refuses_the_escape_once_a_session_root_is_installed() {
    let fixture = Fixture::new();
    let victim = fixture.outside.join("victim");
    fs::create_dir(&victim).expect("plant victim dir");

    install_confinement_root(&fixture.tree);

    let refused = RealDeleteFs.rmdir(&fixture.through_symlink("victim"));

    assert_refused(&refused, victim.is_dir(), "rmdir");
}

/// `unlink_symlink` refuses the escape once a session root is installed.
#[test]
fn unlink_symlink_refuses_the_escape_once_a_session_root_is_installed() {
    let fixture = Fixture::new();
    let victim = fixture.outside.join("victim");
    plant_symlink(Path::new("/dev/null"), &victim);

    install_confinement_root(&fixture.tree);

    let refused = RealDeleteFs.unlink_symlink(&fixture.through_symlink("victim"));

    assert_refused(
        &refused,
        victim.symlink_metadata().is_ok(),
        "unlink_symlink",
    );
}

/// `unlink_device` refuses the escape once a session root is installed.
///
/// The victim is a regular file for the reason given on the escape cell:
/// `RealDeleteFs` routes every file-like kind to the same unlink, so the
/// inode type is irrelevant to what is being measured.
#[test]
fn unlink_device_refuses_the_escape_once_a_session_root_is_installed() {
    let fixture = Fixture::new();
    let victim = fixture.plant_file(&fixture.outside, "victim");

    install_confinement_root(&fixture.tree);

    let refused = RealDeleteFs.unlink_device(&fixture.through_symlink("victim"));

    assert_refused(&refused, victim.exists(), "unlink_device");
}

/// `unlink_special` refuses the escape once a session root is installed.
#[test]
fn unlink_special_refuses_the_escape_once_a_session_root_is_installed() {
    let fixture = Fixture::new();
    let victim = fixture.outside.join("victim");
    mkfifo(&victim);

    install_confinement_root(&fixture.tree);

    let refused = RealDeleteFs.unlink_special(&fixture.through_symlink("victim"));

    assert_refused(
        &refused,
        victim.symlink_metadata().is_ok(),
        "unlink_special",
    );
}

/// `remove_dir_all` refuses the escape once a session root is installed.
///
/// The sharpest of the five: unrefused, this one destroys a whole out-of-tree
/// subtree rather than a single entry, so the payload is asserted separately
/// from the directory.
#[test]
fn remove_dir_all_refuses_the_escape_once_a_session_root_is_installed() {
    let fixture = Fixture::new();
    let victim = fixture.outside.join("victim");
    fs::create_dir(&victim).expect("plant victim dir");
    fs::create_dir(victim.join("nested")).expect("plant nested dir");
    let payload = victim.join("nested").join("payload");
    fs::write(&payload, b"irreplaceable").expect("plant payload");

    install_confinement_root(&fixture.tree);

    let refused = RealDeleteFs.remove_dir_all(&fixture.through_symlink("victim"));

    assert_refused(&refused, victim.is_dir(), "remove_dir_all");
    assert!(
        payload.exists(),
        "the out-of-tree payload must survive a refused remove_dir_all"
    );
}

/// Non-vacuity companion for the four cells routing through `unlink_at` with
/// [`UnlinkFlags::File`] - `unlink_file`, `unlink_symlink`, `unlink_device`
/// and `unlink_special`, which share one method body.
///
/// Without it a mutation that made the resolver refuse EVERY path would
/// satisfy all four refusal assertions. Three companions cover the three
/// distinct confined success paths (`unlinkat` file, `unlinkat` dir, and the
/// recursive peel); one per refusal cell would restate the same evidence.
#[test]
fn an_in_tree_unlink_still_succeeds_with_a_session_root_installed() {
    let fixture = Fixture::new();
    let bystander = fixture.plant_file(&fixture.outside, "victim");
    let target = fixture.plant_file(&fixture.tree.join("real"), "victim");

    install_confinement_root(&fixture.tree);

    RealDeleteFs
        .unlink_file(&fixture.in_tree("victim"))
        .expect("a confined in-tree unlink must still succeed");

    assert!(
        !target.exists(),
        "the in-tree file should have been removed"
    );
    assert!(
        bystander.exists(),
        "nothing outside the tree may be touched"
    );
}

/// Non-vacuity companion for the confined `unlinkat` directory path.
#[test]
fn an_in_tree_rmdir_still_succeeds_with_a_session_root_installed() {
    let fixture = Fixture::new();
    let target = fixture.tree.join("real").join("victim");
    fs::create_dir(&target).expect("plant target dir");

    install_confinement_root(&fixture.tree);

    RealDeleteFs
        .rmdir(&fixture.in_tree("victim"))
        .expect("a confined in-tree rmdir must still succeed");

    assert!(!target.exists(), "the in-tree dir should have been removed");
}

/// Non-vacuity companion for the confined recursive peel.
#[test]
fn an_in_tree_remove_dir_all_still_succeeds_with_a_session_root_installed() {
    let fixture = Fixture::new();
    let target = fixture.tree.join("real").join("victim");
    fs::create_dir(&target).expect("plant target dir");
    fs::write(target.join("payload"), b"expendable").expect("plant payload");

    install_confinement_root(&fixture.tree);

    RealDeleteFs
        .remove_dir_all(&fixture.in_tree("victim"))
        .expect("a confined in-tree recursive delete must still succeed");

    assert!(!target.exists(), "the in-tree dir should have been removed");
}
