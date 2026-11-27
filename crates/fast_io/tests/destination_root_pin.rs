//! The receiver's destination is entered once, through the ownership walk, and
//! every later open of it by its spelling is answered with the descriptor the
//! walk produced - oc's form of upstream's `fchdir()` into the destination.
//!
//! upstream: `rsync-3.5.2/util1.c:1363-1393` and `:1450-1466` `change_dir()`.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};

use fast_io::confinement::{
    clear_destination_root, pin_destination_root, pinned_destination_relative,
    pinned_destination_root_for,
};

fn layout() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().expect("tempdir");
    let base = fs::canonicalize(temp.path()).expect("canonicalize");
    let dest = base.join("dest");
    fs::create_dir(&dest).expect("mkdir dest");
    fs::create_dir(base.join("outside")).expect("mkdir outside");
    (temp, base, dest)
}

/// Swaps `dest` for a symlink to `base/outside`, keeping the original
/// directory at `base/entered`.
fn swap_for_symlink(base: &Path, dest: &Path) -> PathBuf {
    let entered = base.join("entered");
    fs::rename(dest, &entered).expect("move dest aside");
    symlink(base.join("outside"), dest).expect("plant symlink");
    entered
}

fn pin(dest: &Path) {
    let anchor = fast_io::operator_open_destination(dest).expect("walk dest");
    pin_destination_root(dest, anchor).expect("pin dest");
}

/// A write anchored on the destination after it was swapped lands in the
/// directory that was entered, not where the swapped-in symlink points.
#[test]
fn a_pinned_destination_survives_a_swap_for_a_symlink() {
    let (_temp, base, dest) = layout();
    pin(&dest);
    let entered = swap_for_symlink(&base, &dest);

    drop(fast_io::confined_create_new(&dest, &dest.join("f")).expect("create beneath dest"));

    assert!(
        entered.join("f").is_file(),
        "the write left the entered directory"
    );
    assert!(
        fs::read_dir(base.join("outside"))
            .expect("outside")
            .next()
            .is_none(),
        "the write followed the swapped-in symlink"
    );
    let held = fs::File::from(fast_io::open_trusted_dir(&dest).expect("open dest"));
    let entered_meta = fs::metadata(&entered).expect("stat entered");
    let held_meta = held.metadata().expect("fstat held");
    assert_eq!(
        (held_meta.dev(), held_meta.ino()),
        (entered_meta.dev(), entered_meta.ino())
    );
}

/// A destination swapped between the walk and the pin is refused rather than
/// pinned under a name that no longer means it.
#[test]
fn a_destination_swapped_before_the_pin_is_refused() {
    let (_temp, base, dest) = layout();
    let anchor = fast_io::operator_open_destination(&dest).expect("walk dest");
    swap_for_symlink(&base, &dest);

    let error = pin_destination_root(&dest, anchor).expect_err("the swap must be caught");

    assert_eq!(error.raw_os_error(), Some(libc::ELOOP));
    assert!(pinned_destination_root_for(&dest).is_none());
}

/// The pin answers only for its own root, under either trailing-slash
/// spelling, and a cleared pin answers for nothing.
#[test]
fn the_pin_answers_only_for_its_own_root() {
    let (_temp, base, dest) = layout();
    pin(&dest);

    assert!(pinned_destination_root_for(&dest).is_some());
    assert!(pinned_destination_root_for(&base.join("dest/")).is_some());
    assert!(pinned_destination_root_for(&dest.join("sub")).is_none());
    assert!(pinned_destination_root_for(&base.join("outside")).is_none());

    clear_destination_root();
    assert!(pinned_destination_root_for(&dest).is_none());
}
/// Every path beneath the pinned destination resolves to the held descriptor
/// plus its remainder, so a consumer anchored on it keeps reaching the entered
/// directory after a swap. Paths outside the pin, or climbing out with `..`,
/// are left to the ordinary resolver.
#[test]
fn paths_beneath_the_pin_resolve_relative_to_the_held_descriptor() {
    let (_temp, base, dest) = layout();
    assert!(pinned_destination_relative(&dest.join("f")).is_none());
    pin(&dest);
    let (fd, relative) = pinned_destination_relative(&dest).expect("the root itself");
    assert_eq!(relative, Path::new("."));
    let (_, relative) = pinned_destination_relative(&base.join("dest/a/b")).expect("beneath");
    assert_eq!(relative, Path::new("a/b"));
    assert!(pinned_destination_relative(&dest.join("../outside")).is_none());
    assert!(pinned_destination_relative(&base.join("outside/f")).is_none());
    let entered = swap_for_symlink(&base, &dest);
    let held = fs::File::from(fd.try_clone().expect("dup the held descriptor"));
    let held_meta = held.metadata().expect("fstat held");
    let entered_meta = fs::metadata(&entered).expect("stat entered");
    assert_eq!(
        (held_meta.dev(), held_meta.ino()),
        (entered_meta.dev(), entered_meta.ino())
    );
    clear_destination_root();
    assert!(pinned_destination_relative(&dest.join("f")).is_none());
}

/// Clears the process-wide pin when a test ends, even on a failed assertion.
struct PinGuard;

impl Drop for PinGuard {
    fn drop(&mut self) {
        clear_destination_root();
    }
}

/// A basis named beneath the pinned destination opens from the entered
/// directory after a swap, never from where the swapped-in symlink points:
/// upstream opens it by a name relative to the cwd `change_dir()` entered
/// (`rsync-3.5.2/receiver.c:1086-1093`). This is the `--partial-dir`,
/// `--fuzzy` and backup basis open of the network receiver.
#[test]
fn a_basis_beneath_the_pin_opens_from_the_entered_directory() {
    use std::io::Read as _;
    let (_temp, base, dest) = layout();
    fs::write(dest.join("basis"), b"entered").expect("basis");
    fs::write(base.join("outside/basis"), b"outside").expect("decoy");
    pin(&dest);
    let _guard = PinGuard;
    swap_for_symlink(&base, &dest);

    let mut read = String::new();
    fast_io::open_basis_nofollow(&dest.join("basis"))
        .expect("open the basis")
        .read_to_string(&mut read)
        .expect("read the basis");

    assert_eq!(
        read, "entered",
        "the basis was read through the swapped-in symlink"
    );
}

/// `open_read` resolves a destination name from the pin, so the local copy's
/// basis and pre-image reads stay in the entered directory after a swap.
#[test]
fn open_read_resolves_from_the_entered_directory() {
    let (_temp, base, dest) = layout();
    fs::write(dest.join("f"), b"entered").expect("file");
    fs::write(base.join("outside/f"), b"outside").expect("decoy");
    pin(&dest);
    let _guard = PinGuard;
    swap_for_symlink(&base, &dest);

    let read =
        std::io::read_to_string(fast_io::destination_at::open_read(&dest.join("f")).expect("open"))
            .expect("read");

    assert_eq!(read, "entered");
}

/// `open_read` refuses a symlink leaf, as upstream's basis and pre-image
/// opens carry `O_NOFOLLOW` (`rsync-3.5.2/syscall.c:4084`,
/// `rsync-3.5.2/util1.c:411-412`): a planted leaf link cannot hand the reader
/// a file outside the entered directory.
#[test]
fn open_read_refuses_a_symlink_leaf() {
    let (_temp, base, dest) = layout();
    fs::write(base.join("outside/secret"), b"outside").expect("decoy");
    symlink(base.join("outside/secret"), dest.join("f")).expect("leaf link");
    pin(&dest);
    let _guard = PinGuard;

    let error = fast_io::destination_at::open_read(&dest.join("f"))
        .expect_err("a symlink leaf must not be followed");

    assert_eq!(error.raw_os_error(), Some(libc::ELOOP));
}

/// `read_dir` lists the entered directory after a swap, with each entry's own
/// `lstat`, as upstream's `--fuzzy` dirlist does
/// (`rsync-3.5.2/generator.c:2230`).
#[test]
fn read_dir_lists_the_entered_directory() {
    let (_temp, base, dest) = layout();
    fs::write(dest.join("kept"), b"12345").expect("file");
    fs::write(base.join("outside/decoy"), b"x").expect("decoy");
    pin(&dest);
    let _guard = PinGuard;
    swap_for_symlink(&base, &dest);

    let listed = fast_io::destination_at::read_dir(&dest).expect("list");

    let names: Vec<_> = listed.iter().map(|(name, _)| name.clone()).collect();
    assert_eq!(names, vec![std::ffi::OsString::from("kept")]);
    assert!(listed[0].1.is_file());
    assert_eq!(listed[0].1.len(), 5);
}

/// A path-only consumer gets a spelling that resolves through the pin: on
/// Linux the descriptor's `/proc/self/fd` link, which reads the entered
/// directory after a swap.
#[cfg(target_os = "linux")]
#[test]
fn with_anchored_path_spells_the_pin_through_proc() {
    let (_temp, base, dest) = layout();
    fs::write(dest.join("f"), b"entered").expect("file");
    fs::write(base.join("outside/f"), b"outside").expect("decoy");
    pin(&dest);
    let _guard = PinGuard;
    swap_for_symlink(&base, &dest);

    let read = fast_io::destination_at::with_anchored_path(&dest.join("f"), |anchored| {
        let anchored = anchored.expect("Linux spells the pin through /proc");
        assert!(
            anchored.starts_with("/proc/self/fd"),
            "{}",
            anchored.display()
        );
        fs::read_to_string(anchored)
    })
    .expect("anchored read");

    assert_eq!(read, "entered");
}

/// Where the pin has no path spelling, the consumer is told so and gets no
/// path, rather than the name the pin exists to stop following; it then reads
/// through a descriptor opened beneath the pin.
#[cfg(all(unix, not(target_os = "linux")))]
#[test]
fn with_anchored_path_offers_no_spelling_without_proc() {
    let (_temp, _base, dest) = layout();
    pin(&dest);
    let _guard = PinGuard;

    let seen = fast_io::destination_at::with_anchored_path(&dest.join("f"), |spelling| {
        spelling.map(Path::to_path_buf)
    });

    assert_eq!(seen, None);
}

/// A path outside the pin is handed to the consumer unchanged.
#[test]
fn with_anchored_path_passes_an_unpinned_path_through() {
    let (_temp, base, dest) = layout();
    pin(&dest);
    let _guard = PinGuard;
    let outside = base.join("outside");

    let seen =
        fast_io::destination_at::with_anchored_path(&outside, |path| path.map(Path::to_path_buf));

    assert_eq!(seen, Some(outside));
}

/// What a swap left in `base/outside`; empty when nothing followed it.
fn outside_entries(base: &Path) -> usize {
    fs::read_dir(base.join("outside")).expect("outside").count()
}

/// A single-file destination's confinement root is the file itself, so its
/// temp file and final name are not beneath it. They are entries of the
/// parent the receiver entered and pinned (upstream `change_dir#3`,
/// `rsync-3.5.2/main.c:865-868`), and resolve from it: after that parent is
/// swapped for the operator's own symlink - which the ownership walk would
/// follow - the commit still lands in the parent that was entered.
#[test]
fn a_single_file_commit_lands_in_the_pinned_parent_after_a_swap() {
    let (_temp, base, dest) = layout();
    let file = dest.join("f");
    let staged = dest.join(".f.tmp");
    pin(&dest);
    let entered = swap_for_symlink(&base, &dest);

    drop(fast_io::confined_create_new(&file, &staged).expect("stage beside the file"));
    fast_io::confined_rename(&file, &staged, &file, true).expect("commit the file");

    assert!(
        entered.join("f").is_file(),
        "the commit left the entered parent"
    );
    assert_eq!(
        outside_entries(&base),
        0,
        "the commit followed the swapped-in symlink"
    );
}

/// The direct-write arm creates the final name itself; it resolves from the
/// pinned parent the same way.
#[test]
fn a_single_file_created_in_place_lands_in_the_pinned_parent_after_a_swap() {
    let (_temp, base, dest) = layout();
    let file = dest.join("f");
    pin(&dest);
    let entered = swap_for_symlink(&base, &dest);

    drop(fast_io::confined_create_new(&file, &file).expect("create the file"));

    assert!(
        entered.join("f").is_file(),
        "the write left the entered parent"
    );
    assert_eq!(
        outside_entries(&base),
        0,
        "the write followed the swapped-in symlink"
    );
}

/// CONTROL for the two cells above: with nothing pinned, the same calls
/// resolve the swapped name afresh and follow the operator's own symlink, so
/// the cells above fail if the pinned parent is not consulted.
#[test]
fn an_unpinned_single_file_commit_follows_a_swapped_in_symlink() {
    let (_temp, base, dest) = layout();
    let file = dest.join("f");
    let staged = dest.join(".f.tmp");
    clear_destination_root();
    let entered = swap_for_symlink(&base, &dest);

    drop(fast_io::confined_create_new(&file, &staged).expect("stage beside the file"));
    fast_io::confined_rename(&file, &staged, &file, true).expect("commit the file");

    assert!(base.join("outside/f").is_file());
    assert!(!entered.join("f").exists());
}
