//! CF-P0c: the `*_via_sandbox_or_fallback` tails escape the tree.
//!
//! Both helpers take a `sandbox: Option<&DirSandbox>`. It is `None` for
//! every caller that has not yet been plumbed with a sandbox, plus every
//! local-copy path, and then the confined branch is skipped entirely and
//! the call lands on a bare `std::fs` entry point:
//!
//! - `renameat_via_sandbox_or_fallback` falls through to
//!   `std::fs::rename(old_link_path, new_link_path)`.
//! - `openat_via_sandbox_or_fallback` falls through to
//!   `open_path_with_flags(link_path, ..)`, which rebuilds the open from
//!   `std::fs::OpenOptions`.
//!
//! Both resolve the whole path through the kernel with symlink following
//! enabled, so a planted component redirects them out of the tree. The
//! open fallback carries a second, independent defect: `OpenOptions` has
//! no knob for `O_NOFOLLOW`, so that bit is silently discarded and even
//! the terminal component is followed. `open.rs` states this ("Flags
//! outside that set are silently dropped on the fallback path"); these
//! tests make it observable.
//!
//! This is a RED baseline, not a regression guard. Each escape test is
//! paired with a companion proving the fixture is capable of ordinary,
//! non-escaping behaviour, so no assertion here can pass vacuously.
//!
//! upstream: `rsync-3.5.0/syscall.c:1918-1923` `do_rename_at()` and
//! `:2896-2961` `ds_descend()` - upstream confines each endpoint per
//! component rather than handing a full path to the kernel.

#![cfg(unix)]

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use fast_io::dir_sandbox::DirSandbox;
use tempfile::TempDir;

/// The two-tree fixture every test in this module shares.
///
/// `root` is the tree the caller believes it is confined to; `outside`
/// is a sibling nothing may reach. `root/hop` is a symlink to `outside`,
/// standing in for a component flipped under the caller's feet.
struct Fixture {
    _base: TempDir,
    root: PathBuf,
    outside: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let base = TempDir::new().expect("tempdir");
        let root = base.path().join("root");
        let outside = base.path().join("outside");
        fs::create_dir(&root).expect("mkdir root");
        fs::create_dir(&outside).expect("mkdir outside");
        plant_symlink(&outside, &root.join("hop"));
        fs::create_dir(root.join("real")).expect("mkdir real");
        Self {
            _base: base,
            root,
            outside,
        }
    }
}

/// Plant a symlink at `link` pointing at `target`, asserting the result
/// really is one.
///
/// The assertion is load-bearing: a mutation sweep that replaced the
/// planted symlinks with regular files left
/// [`open_fallback_honours_o_nofollow_at_the_leaf`] still passing, because
/// reading the expected bytes back does not by itself say the leaf was
/// followed. Pinning the fixture shape is what makes that test
/// falsifiable.
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

fn read_to_string(file: &mut fs::File) -> String {
    let mut body = String::new();
    file.read_to_string(&mut body).expect("read opened file");
    body
}

/// `renameat_via_sandbox_or_fallback(None, ..)` reaches `std::fs::rename`,
/// which walks `root/hop` into the sibling tree and publishes the staged
/// file there.
#[test]
fn rename_fallback_escapes_through_a_symlinked_destination_parent() {
    let fixture = Fixture::new();
    let staged = fixture.root.join(".tmp");
    fs::write(&staged, b"payload").expect("stage the temp file");
    let committed = fixture.root.join("hop").join("committed");

    fast_io::renameat_via_sandbox_or_fallback(
        None,
        &fixture.root,
        Path::new(".tmp"),
        &staged,
        &fixture.root,
        Path::new("hop/committed"),
        &committed,
        true,
    )
    .expect("the unconfined rename follows the planted symlink");

    let escaped = fixture.outside.join("committed");
    assert_eq!(
        fs::read_to_string(&escaped).ok().as_deref(),
        Some("payload"),
        "no escape: {} does not hold the staged payload",
        escaped.display()
    );
    assert!(
        !fixture.root.join("real").join("committed").exists(),
        "fixture broken: the payload also appeared inside the tree"
    );
}

/// Non-vacuity companion: with no symlink in the destination path the
/// same fallback performs an ordinary in-tree commit.
#[test]
fn rename_fallback_without_a_symlink_stays_in_the_tree() {
    let fixture = Fixture::new();
    let staged = fixture.root.join(".tmp");
    fs::write(&staged, b"payload").expect("stage the temp file");
    let committed = fixture.root.join("real").join("committed");

    fast_io::renameat_via_sandbox_or_fallback(
        None,
        &fixture.root,
        Path::new(".tmp"),
        &staged,
        &fixture.root,
        Path::new("real/committed"),
        &committed,
        true,
    )
    .expect("an ordinary in-tree commit");

    assert_eq!(
        fs::read_to_string(&committed).ok().as_deref(),
        Some("payload"),
        "the in-tree commit should have landed"
    );
    assert!(
        !fixture.outside.join("committed").exists(),
        "nothing outside the tree may be touched"
    );
}

/// `openat_via_sandbox_or_fallback(None, ..)` reaches
/// `open_path_with_flags`, which walks `root/hop` into the sibling tree
/// and hands back a handle on the out-of-tree inode.
#[test]
fn open_fallback_escapes_through_a_symlinked_parent() {
    let fixture = Fixture::new();
    fs::write(fixture.outside.join("secret"), b"out-of-tree").expect("plant the secret");
    fs::write(fixture.root.join("real").join("secret"), b"in-tree").expect("plant the decoy");
    let link_path = fixture.root.join("hop").join("secret");

    let mut opened = fast_io::openat_via_sandbox_or_fallback(
        None,
        &fixture.root,
        Path::new("hop/secret"),
        &link_path,
        libc::O_RDONLY,
        0,
    )
    .expect("the unconfined open follows the planted symlink");

    assert_eq!(
        read_to_string(&mut opened),
        "out-of-tree",
        "no escape: the open resolved inside the tree"
    );
}

/// Non-vacuity companion: the same fallback against a real in-tree
/// parent opens the in-tree file.
#[test]
fn open_fallback_without_a_symlink_stays_in_the_tree() {
    let fixture = Fixture::new();
    fs::write(fixture.outside.join("secret"), b"out-of-tree").expect("plant the secret");
    fs::write(fixture.root.join("real").join("secret"), b"in-tree").expect("plant the decoy");
    let link_path = fixture.root.join("real").join("secret");

    let mut opened = fast_io::openat_via_sandbox_or_fallback(
        None,
        &fixture.root,
        Path::new("real/secret"),
        &link_path,
        libc::O_RDONLY,
        0,
    )
    .expect("an ordinary in-tree open");

    assert_eq!(read_to_string(&mut opened), "in-tree");
}

/// The second, independent defect, now CLOSED: the fallback rebuilt the
/// open from `OpenOptions`, which cannot express `O_NOFOLLOW`, so the
/// caller's refusal to follow the TERMINAL component was discarded and
/// the symlinked leaf resolved out of the tree. The fallback now honours
/// the whole flag word and refuses with `ELOOP`.
///
/// Paired with [`the_sandbox_branch_honours_o_nofollow_on_the_same_leaf`],
/// which asserts the SAME refusal through the confined branch. The pair
/// began as a DIVERGENCE pin - the two branches disagreed, and that
/// disagreement is what identified the dropped flag. It is now a PARITY
/// pin: the fallback must not drift away from the confined branch again.
#[test]
fn open_fallback_honours_o_nofollow_at_the_leaf() {
    let fixture = Fixture::new();
    fs::write(fixture.outside.join("secret"), b"out-of-tree").expect("plant the secret");
    let leaf = fixture.root.join("leaflink");
    plant_symlink(&fixture.outside.join("secret"), &leaf);
    let err = fast_io::openat_via_sandbox_or_fallback(
        None,
        &fixture.root,
        Path::new("leaflink"),
        &leaf,
        libc::O_RDONLY | libc::O_NOFOLLOW,
        0,
    )
    .expect_err("the fallback must refuse the symlinked leaf under O_NOFOLLOW");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ELOOP),
        "expected O_NOFOLLOW to refuse the leaf, got {err}"
    );
}

/// Non-vacuity companion for [`open_fallback_honours_o_nofollow_at_the_leaf`].
///
/// The sandbox branch already had one; the fallback branch did not, because
/// while its pin asserted a SUCCESS the success was itself the evidence.
/// Now that it asserts a refusal, the pin would pass just as well if the
/// fallback refused every input - so a regular in-tree leaf under the same
/// `O_NOFOLLOW` must still open.
#[test]
fn open_fallback_opens_a_regular_leaf_under_o_nofollow() {
    let fixture = Fixture::new();
    let leaf = fixture.root.join("real").join("secret");
    fs::write(&leaf, b"in-tree").expect("plant the in-tree leaf");
    let mut opened = fast_io::openat_via_sandbox_or_fallback(
        None,
        &fixture.root,
        Path::new("real/secret"),
        &leaf,
        libc::O_RDONLY | libc::O_NOFOLLOW,
        0,
    )
    .expect("a regular in-tree leaf still opens under O_NOFOLLOW");
    assert_eq!(read_to_string(&mut opened), "in-tree");
}

/// Paired control for [`open_fallback_honours_o_nofollow_at_the_leaf`]:
/// the SAME leaf, the SAME flags, routed through a real `DirSandbox` so
/// the confined `openat` branch runs instead. Both branches must now
/// refuse with `ELOOP`. This cell is what proved the kernel was not
/// simply ignoring the flag: it refused here even while the fallback
/// still followed.
#[test]
fn the_sandbox_branch_honours_o_nofollow_on_the_same_leaf() {
    let fixture = Fixture::new();
    fs::write(fixture.outside.join("secret"), b"out-of-tree").expect("plant the secret");
    let leaf = fixture.root.join("leaflink");
    plant_symlink(&fixture.outside.join("secret"), &leaf);
    let sandbox = DirSandbox::open_root(&fixture.root).expect("open the sandbox root");

    let err = fast_io::openat_via_sandbox_or_fallback(
        Some(&sandbox),
        &fixture.root,
        Path::new("leaflink"),
        &leaf,
        libc::O_RDONLY | libc::O_NOFOLLOW,
        0,
    )
    .expect_err("the confined openat must refuse the symlinked leaf");

    assert_eq!(
        err.raw_os_error(),
        Some(libc::ELOOP),
        "expected O_NOFOLLOW to refuse the leaf, got {err}"
    );
}

/// Non-vacuity companion for the control: the sandbox branch is not
/// simply failing for every input - a regular in-tree leaf opens fine
/// under the same `O_NOFOLLOW`.
#[test]
fn the_sandbox_branch_opens_a_regular_leaf_under_o_nofollow() {
    let fixture = Fixture::new();
    let leaf = fixture.root.join("plain");
    fs::write(&leaf, b"in-tree").expect("plant the regular leaf");
    let sandbox = DirSandbox::open_root(&fixture.root).expect("open the sandbox root");

    let mut opened = fast_io::openat_via_sandbox_or_fallback(
        Some(&sandbox),
        &fixture.root,
        Path::new("plain"),
        &leaf,
        libc::O_RDONLY | libc::O_NOFOLLOW,
        0,
    )
    .expect("a regular leaf is not a symlink, so O_NOFOLLOW permits it");

    assert_eq!(read_to_string(&mut opened), "in-tree");
}
