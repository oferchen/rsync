//! Regression coverage for a `--relative` source operand that carries a
//! trailing DOTDIR marker (`sym-to-dir/`, `sym-to-dir/.`).
//!
//! Upstream keeps TWO facts about such an operand apart, and collapsing them
//! into one decision is what this pins:
//!
//! 1. The NAME it stats is the operand with the marker stripped
//!    (`flist.c:2652-2657` - `fn[--len] = '\0'` before `name_type` is set).
//!    Stating the raw `operand/` form instead makes the kernel resolve the
//!    trailing slash, which silently turns the `lstat` into a `stat` that also
//!    demands a directory: a symlink to a FILE came back `ENOTDIR` and a
//!    dangling symlink came back `ENOENT`, so both operands aborted the
//!    transfer at exit 23 instead of being sent.
//! 2. The marker itself survives as `name_type` and feeds the stat's follow
//!    decision (`flist.c:2697` - `copy_dirlinks || name_type != NORMAL_NAME`).
//!    `link_stat()` (`flist.c:286-301`) is then a two-step: `lstat` first, and
//!    the `stat` result replaces it ONLY when the target is a directory.
//!
//! So `sym-to-dir/` is the directory it points at, while `sym-to-file/`,
//! `dangling/`, and a plain-file operand keep their own `lstat` identity. The
//! assertions are on WHAT LANDS AT THE DESTINATION, since both halves of the
//! mechanism are observable only there.
//!
//! # Upstream Reference
//!
//! - `flist.c:115-118` - `NORMAL_NAME` / `SLASH_ENDING_NAME` / `DOTDIR_NAME`.
//! - `flist.c:286-301` - `link_stat()`, the lstat-then-dir-only-upgrade pair.
//! - `flist.c:2652-2657` - the marker is stripped from the name it stats.
//! - `flist.c:2697` - `link_stat(fbuf, &st, copy_dirlinks || name_type != NORMAL_NAME)`.

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use tempfile::tempdir;

/// Builds `from/{realdir/{f.txt,sub/g.txt},realfile.txt}` plus the three
/// symlinks the follow decision has to tell apart, and returns the `/./`-
/// anchored `-R` operand for `leaf` (marker appended verbatim) followed by the
/// destination root.
fn fixture(leaf_with_marker: &str) -> (Vec<OsString>, PathBuf, tempfile::TempDir) {
    let temp = tempdir().expect("tempdir");
    let from = temp.path().join("from");
    let to = temp.path().join("to");
    fs::create_dir_all(from.join("realdir/sub")).expect("create source tree");
    fs::create_dir_all(&to).expect("create destination root");
    fs::write(from.join("realdir/f.txt"), b"hello").expect("write f.txt");
    fs::write(from.join("realdir/sub/g.txt"), b"deep").expect("write g.txt");
    fs::write(from.join("realfile.txt"), b"plain").expect("write realfile.txt");
    symlink("realdir", from.join("sym-to-dir")).expect("symlink to dir");
    symlink("realfile.txt", from.join("sym-to-file")).expect("symlink to file");
    symlink("nowhere", from.join("dangling")).expect("dangling symlink");

    // `from/./<leaf-with-marker>`: the dot anchor makes the relative chain
    // exactly the leaf, so the entry lands at `to/<leaf>`.
    let mut operand = OsString::from(from.as_os_str());
    operand.push("/./");
    operand.push(leaf_with_marker);

    (vec![operand, to.clone().into_os_string()], to, temp)
}

fn base_options() -> LocalCopyOptions {
    LocalCopyOptions::default()
        .recursive(true)
        .relative_paths(true)
        .links(true)
}

fn run(leaf_with_marker: &str, options: LocalCopyOptions) -> (PathBuf, tempfile::TempDir) {
    let (operands, to, temp) = fixture(leaf_with_marker);
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_report(LocalCopyExecution::Apply, options)
        .expect("a marked operand must not abort the transfer");
    (to, temp)
}

fn symlink_target(path: &Path) -> PathBuf {
    fs::read_link(path).expect("destination entry must be a symlink")
}

/// A symlink to a DIRECTORY with a trailing slash is the directory it points
/// at: the marker's follow yields a directory, so the `stat` result wins and
/// the whole subtree is walked. Upstream `rsync -aR from/./sym-to-dir/ to/`.
#[test]
fn marked_symlink_to_dir_transfers_the_directory_it_points_at() {
    let (to, _temp) = run("sym-to-dir/", base_options());

    let landed = to.join("sym-to-dir");
    assert!(
        fs::symlink_metadata(&landed)
            .expect("sym-to-dir must land")
            .is_dir(),
        "the marker's follow yielded a directory, so it is sent as a real directory"
    );
    assert_eq!(
        fs::read(landed.join("f.txt")).expect("f.txt must land"),
        b"hello",
        "the directory's contents are walked through the symlink"
    );
    assert_eq!(
        fs::read(landed.join("sub/g.txt")).expect("sub/g.txt must land"),
        b"deep"
    );
}

/// `sym-to-dir/.` is the same DOTDIR marker in its explicit spelling and must
/// resolve identically.
#[test]
fn marked_symlink_to_dir_dot_form_transfers_the_directory() {
    let (to, _temp) = run("sym-to-dir/.", base_options());

    let landed = to.join("sym-to-dir");
    assert!(
        fs::symlink_metadata(&landed)
            .expect("sym-to-dir must land")
            .is_dir()
    );
    assert_eq!(
        fs::read(landed.join("f.txt")).expect("f.txt must land"),
        b"hello"
    );
}

/// A symlink to a FILE with the same trailing slash keeps its own identity:
/// `link_stat()` attempts the follow and DISCARDS it because the target is not
/// a directory, so the symlink itself is transferred. Before the fix the raw
/// `sym-to-file/` stat returned `ENOTDIR` and the run aborted at exit 23 with
/// nothing transferred.
#[test]
fn marked_symlink_to_file_stays_a_symlink() {
    let (to, _temp) = run("sym-to-file/", base_options());

    let landed = to.join("sym-to-file");
    assert!(
        fs::symlink_metadata(&landed)
            .expect("sym-to-file must land")
            .file_type()
            .is_symlink(),
        "the follow is discarded when the target is not a directory"
    );
    assert_eq!(symlink_target(&landed), Path::new("realfile.txt"));
}

/// A DANGLING symlink with a trailing slash is transferred as the symlink it
/// is: the `lstat` succeeds, and the follow that fails changes nothing. Before
/// the fix the raw `dangling/` stat returned `ENOENT` and the run aborted at
/// exit 23 as a `link_stat` failure.
#[test]
fn marked_dangling_symlink_stays_a_symlink() {
    let (to, _temp) = run("dangling/", base_options());

    let landed = to.join("dangling");
    assert!(
        fs::symlink_metadata(&landed)
            .expect("dangling must land")
            .file_type()
            .is_symlink()
    );
    assert_eq!(symlink_target(&landed), Path::new("nowhere"));
}

/// A plain REGULAR FILE with a trailing slash is transferred as the file: the
/// marker never made it a directory, so neither the stat nor the destination
/// layout may treat it as one.
#[test]
fn marked_regular_file_transfers_as_a_file() {
    let (to, _temp) = run("realfile.txt/", base_options());

    let landed = to.join("realfile.txt");
    assert!(
        fs::symlink_metadata(&landed)
            .expect("realfile.txt must land")
            .is_file()
    );
    assert_eq!(fs::read(&landed).expect("contents"), b"plain");
}

/// `--copy-dirlinks` is the OTHER disjunct of the same follow decision and
/// carries the same dir-only rule: it must not turn a symlink-to-file into its
/// target. Pins that the two disjuncts were not collapsed into one clause.
#[test]
fn copy_dirlinks_keeps_the_dir_only_rule_for_a_marked_operand() {
    let (to, _temp) = run("sym-to-file/", base_options().copy_dirlinks(true));

    let landed = to.join("sym-to-file");
    assert!(
        fs::symlink_metadata(&landed)
            .expect("sym-to-file must land")
            .file_type()
            .is_symlink(),
        "--copy-dirlinks follows only symlinks to directories"
    );
}

/// `--copy-links` DOES follow a marked symlink to a file, all the way to the
/// regular file - the negative control for the two tests above, so a fix that
/// simply never follows cannot pass the set.
#[test]
fn copy_links_follows_a_marked_symlink_to_a_file() {
    let (to, _temp) = run("sym-to-file/", base_options().copy_links(true));

    let landed = to.join("sym-to-file");
    assert!(
        fs::symlink_metadata(&landed)
            .expect("sym-to-file must land")
            .is_file(),
        "--copy-links resolves the symlink to its target"
    );
    assert_eq!(fs::read(&landed).expect("contents"), b"plain");
}

/// Without the marker the same symlink operand stays `NORMAL_NAME`: no follow
/// is attempted at all. The negative control for the follow itself - a fix
/// that followed unconditionally would turn this into a directory.
#[test]
fn unmarked_symlink_to_dir_stays_a_symlink() {
    let (to, _temp) = run("sym-to-dir", base_options());

    let landed = to.join("sym-to-dir");
    assert!(
        fs::symlink_metadata(&landed)
            .expect("sym-to-dir must land")
            .file_type()
            .is_symlink(),
        "an operand without the marker is NORMAL_NAME and is never followed"
    );
    assert_eq!(symlink_target(&landed), Path::new("realdir"));
}
