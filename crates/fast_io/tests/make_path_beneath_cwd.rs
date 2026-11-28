//! `make_path_beneath_cwd` mirrors rsync 3.5.2's `make_path()` through
//! `do_mkdir_at()`'s confined relative arm (syscall.c:2306-2332): a relative
//! `--mkpath` destination follows an in-tree symlink and fails with `ELOOP`
//! on one that leaves the tree, before anything is created beyond it.
//!
//! Each cell changes the process cwd; nextest runs every test in its own
//! process, so no two cells share it.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use fast_io::make_path_beneath_cwd;

fn enter(dir: &Path) {
    std::env::set_current_dir(dir).expect("enter the fixture");
}

#[test]
fn creates_every_missing_component_and_accepts_existing_ones() {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::create_dir(tmp.path().join("a")).expect("mkdir a");
    enter(tmp.path());

    make_path_beneath_cwd(Path::new("a/b/c"), 0o777).expect("make_path");

    assert!(tmp.path().join("a/b/c").is_dir());
}

#[test]
fn follows_a_relative_symlink_that_stays_in_the_tree() {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::create_dir(tmp.path().join("real")).expect("mkdir real");
    symlink("real", tmp.path().join("link")).expect("symlink");
    enter(tmp.path());

    make_path_beneath_cwd(Path::new("link/new/g"), 0o777).expect("make_path");

    assert!(tmp.path().join("real/new/g").is_dir());
}

#[test]
fn refuses_an_absolute_symlink_target_and_creates_nothing_through_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("outside");
    fs::create_dir(tmp.path().join("dest")).expect("mkdir dest");
    symlink(outside.path(), tmp.path().join("dest/sub")).expect("symlink");
    enter(tmp.path());

    let error = make_path_beneath_cwd(Path::new("dest/sub/new/g"), 0o777)
        .expect_err("an absolute target leaves the tree");

    assert_eq!(error.raw_os_error(), Some(libc::ELOOP), "{error}");
    assert_eq!(fs::read_dir(outside.path()).expect("list").count(), 0);
}

#[test]
fn refuses_a_relative_symlink_that_climbs_out_of_the_tree() {
    let base = tempfile::tempdir().expect("tempdir");
    let tree = base.path().join("tree");
    fs::create_dir_all(tree.join("dest")).expect("mkdir tree/dest");
    fs::create_dir(base.path().join("outside")).expect("mkdir outside");
    symlink("../../outside", tree.join("dest/sub")).expect("symlink");
    enter(&tree);

    let error = make_path_beneath_cwd(Path::new("dest/sub/new"), 0o777)
        .expect_err("a climbing target leaves the tree");

    assert_eq!(error.raw_os_error(), Some(libc::ELOOP), "{error}");
    assert!(!base.path().join("outside/new").exists());
}

#[test]
fn an_absolute_path_is_refused_like_secure_relative_open() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let error = make_path_beneath_cwd(&tmp.path().join("new"), 0o777)
        .expect_err("the confined arm takes relative paths only");

    assert_eq!(error.raw_os_error(), Some(libc::EINVAL), "{error}");
    assert!(!tmp.path().join("new").exists());
}

/// A regular file as the last component keeps mkdir's EEXIST, as 3.5.2's
/// make_path() does: EEXIST is success only for a directory (util1.c:244-246).
#[test]
fn a_regular_file_as_the_last_component_is_an_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::create_dir(tmp.path().join("dst")).expect("mkdir dst");
    fs::write(tmp.path().join("dst/dstfile"), b"b\n").expect("write dstfile");
    let want = fs::create_dir(tmp.path().join("dst/dstfile"))
        .expect_err("mkdir of a file")
        .raw_os_error();
    enter(tmp.path());

    let error = make_path_beneath_cwd(Path::new("dst/dstfile"), 0o777)
        .expect_err("a file is not a directory");

    assert_eq!(error.raw_os_error(), want, "{error}");
    assert!(tmp.path().join("dst/dstfile").is_file());
}

/// make_path() tries the deepest directory first (util1.c:231-247), so a file
/// that is the parent of that directory fails the mkdir's parent walk with
/// `ENOTDIR`, not the shallower mkdir's `EEXIST`.
#[test]
fn a_regular_file_as_the_parent_of_the_deepest_directory_is_enotdir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::create_dir(tmp.path().join("dst")).expect("mkdir dst");
    fs::write(tmp.path().join("dst/dstfile"), b"b\n").expect("write dstfile");
    let want = fs::create_dir(tmp.path().join("dst/dstfile/x"))
        .expect_err("mkdir below a file")
        .raw_os_error();
    enter(tmp.path());

    let error = make_path_beneath_cwd(Path::new("dst/dstfile/x"), 0o777)
        .expect_err("a file is not a directory");

    assert_eq!(error.raw_os_error(), want, "{error}");
    assert!(tmp.path().join("dst/dstfile").is_file());
}

/// syscall.c:3279-3281 - the relative walk to the deepest directory's parent
/// reports a non-directory intermediate as `ELOOP`.
#[test]
fn a_regular_file_above_the_parent_of_the_deepest_directory_is_eloop() {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::create_dir(tmp.path().join("dst")).expect("mkdir dst");
    fs::write(tmp.path().join("dst/dstfile"), b"b\n").expect("write dstfile");
    let looped = tmp.path().join("loop");
    symlink(&looped, &looped).expect("plant a self loop");
    let want = fs::metadata(&looped)
        .expect_err("a self loop")
        .raw_os_error();
    fs::remove_file(&looped).expect("remove the loop");
    enter(tmp.path());

    let error = make_path_beneath_cwd(Path::new("dst/dstfile/x/y"), 0o777)
        .expect_err("a file is not a directory");

    assert_eq!(error.raw_os_error(), want, "{error}");
    assert!(tmp.path().join("dst/dstfile").is_file());
}
