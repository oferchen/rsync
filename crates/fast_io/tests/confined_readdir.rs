//! `read_dir_confined` must anchor the directory before enumerating it.
//!
//! The scan that discovers entries is its own confinement decision. Resolving
//! the directory and then enumerating the same path by name still loses to a
//! parent flipped between the two, so the names must come from a descriptor
//! the confined walk produced.
//!
//! upstream: `rsync-3.5.0/flist.c` `send_directory()` enumerates the
//! descriptor its confined open produced; `syscall.c:2891` `ds_descend()` is
//! the walk being anchored on.

#![cfg(unix)]

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;

use tempfile::TempDir;

fn names(entries: Vec<OsString>) -> BTreeSet<String> {
    entries
        .into_iter()
        .map(|name| name.to_string_lossy().into_owned())
        .collect()
}

/// Non-vacuity companion: an ordinary directory enumerates its real contents.
/// Without this, every refusal test below would also pass if the function
/// simply errored - or returned nothing - for all inputs.
#[test]
fn read_dir_confined_lists_a_plain_directory() {
    let root = TempDir::new().expect("tempdir");
    fs::create_dir(root.path().join("sub")).expect("mkdir sub");
    fs::write(root.path().join("sub/a"), b"a").expect("write a");
    fs::write(root.path().join("sub/b"), b"b").expect("write b");
    fs::create_dir(root.path().join("sub/c")).expect("mkdir c");

    let entries = fast_io::read_dir_confined(root.path(), std::path::Path::new("sub"))
        .expect("a plain directory must enumerate");

    assert_eq!(
        names(entries),
        ["a", "b", "c"].map(String::from).into_iter().collect(),
        "the scan must report exactly the directory's own entries"
    );
}

/// `.` and `..` are omitted: callers build child paths from these names, and
/// upstream's scan skips them too.
#[test]
fn read_dir_confined_omits_dot_and_dotdot() {
    let root = TempDir::new().expect("tempdir");
    fs::create_dir(root.path().join("sub")).expect("mkdir sub");
    fs::write(root.path().join("sub/only"), b"x").expect("write");

    let entries =
        fast_io::read_dir_confined(root.path(), std::path::Path::new("sub")).expect("enumerate");

    assert_eq!(
        names(entries),
        ["only"].map(String::from).into_iter().collect(),
        "`.` and `..` must not appear as entries"
    );
}

/// A relative, in-tree symlinked parent is FOLLOWED, not refused - upstream's
/// walk descends it (`syscall.c:2961`). Pinning this stops a future
/// "refuse every symlink" simplification from breaking an ordinary recursive
/// copy whose source contains a directory symlink.
#[test]
fn read_dir_confined_follows_a_relative_in_tree_symlink() {
    let root = TempDir::new().expect("tempdir");
    fs::create_dir(root.path().join("real")).expect("mkdir real");
    fs::write(root.path().join("real/inside"), b"x").expect("write");
    std::os::unix::fs::symlink("real", root.path().join("link")).expect("plant symlink");

    let entries = fast_io::read_dir_confined(root.path(), std::path::Path::new("link"))
        .expect("a relative in-tree symlink must not be refused");

    assert_eq!(
        names(entries),
        ["inside"].map(String::from).into_iter().collect(),
        "the scan must descend an in-tree symlink and list its target"
    );
}

/// THE ESCAPE: the directory being scanned is reached through a parent
/// symlinked outside the root. The walk must refuse, and - the part that
/// matters - the outside names must never be reported, because the caller
/// turns them into paths it then copies.
#[test]
fn read_dir_confined_refuses_a_parent_symlinked_outside() {
    let base = TempDir::new().expect("tempdir");
    let root = base.path().join("source");
    let outside = base.path().join("outside");
    fs::create_dir(&root).expect("mkdir source");
    fs::create_dir(&outside).expect("mkdir outside");
    fs::write(outside.join("SECRET-do-not-list"), b"x").expect("write secret");
    std::os::unix::fs::symlink(&outside, root.join("sub")).expect("plant symlink");

    let error = fast_io::read_dir_confined(&root, std::path::Path::new("sub/deeper"))
        .expect_err("a parent symlinked outside the root must be refused");

    let _ = error;
}

/// The leaf itself is an absolute symlink out of the tree. This is the shape
/// `sender-scan-dir-escape` plants: the scan is asked for a directory whose
/// final component leaves the root, and returning its names leaks them into
/// the file list.
#[test]
fn read_dir_confined_never_reports_names_from_outside_the_root() {
    let base = TempDir::new().expect("tempdir");
    let root = base.path().join("source");
    let outside = base.path().join("outside");
    fs::create_dir(&root).expect("mkdir source");
    fs::create_dir(&outside).expect("mkdir outside");
    fs::write(outside.join("SECRET-do-not-list"), b"x").expect("write secret");
    std::os::unix::fs::symlink(&outside, root.join("sub")).expect("plant symlink");

    match fast_io::read_dir_confined(&root, std::path::Path::new("sub")) {
        Err(_) => {}
        Ok(entries) => {
            let listed = names(entries);
            assert!(
                !listed.contains("SECRET-do-not-list"),
                "the scan leaked a name from outside the transfer root: {listed:?}"
            );
        }
    }
}

/// `..` above the anchor is refused up front, matching the portable
/// front-door check the confined open already applies.
#[test]
fn read_dir_confined_refuses_dotdot_above_the_anchor() {
    let base = TempDir::new().expect("tempdir");
    let root = base.path().join("source");
    fs::create_dir(&root).expect("mkdir source");

    let error = fast_io::read_dir_confined(&root, std::path::Path::new("../outside"))
        .expect_err("`..` above the anchor must be refused");

    assert_eq!(
        error.raw_os_error(),
        Some(libc::EINVAL),
        "the front-door check reports EINVAL, like the confined open"
    );
}
