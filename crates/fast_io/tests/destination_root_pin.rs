//! The receiver's destination is entered once, through the ownership walk, and
//! every later open of it by its spelling is answered with the descriptor the
//! walk produced - oc's form of upstream's `fchdir()` into the destination.
//!
//! upstream: `rsync-3.5.1/util1.c:1363-1393` and `:1450-1466` `change_dir()`.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};

use fast_io::confinement::{
    clear_destination_root, pin_destination_root, pinned_destination_root_for,
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
