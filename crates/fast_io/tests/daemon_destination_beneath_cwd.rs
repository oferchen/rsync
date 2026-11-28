//! `open_dir_beneath_cwd` and `create_dir_beneath_cwd` are how a daemon
//! serving an unchrooted module enters and makes the client's destination
//! beneath the module root, its cwd.
//!
//! upstream: util1.c:1430-1446 change_dir() - `secure_relative_dirfd()` then
//! `fchdir()`: an in-tree relative symlink is followed and one that leaves the
//! module is refused with `ELOOP`, whoever owns it. A plain `do_mkdir()`
//! (main.c:817) follows such a link; `create_dir_beneath_cwd` refuses it
//! first, the deliberate deviation that keeps the directory from being made
//! outside the module.
//!
//! Each cell changes the process cwd; nextest runs every test in its own
//! process, so no two cells share it.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use fast_io::{create_dir_beneath_cwd, open_dir_beneath_cwd};

fn enter(dir: &Path) {
    std::env::set_current_dir(dir).expect("enter the fixture");
}

/// A module holding `inner/`, `rel -> inner` and `esc -> <outside>`.
fn module() -> (tempfile::TempDir, tempfile::TempDir) {
    let module = tempfile::tempdir().expect("module");
    let outside = tempfile::tempdir().expect("outside");
    fs::create_dir(module.path().join("inner")).expect("mkdir inner");
    symlink("inner", module.path().join("rel")).expect("relative link");
    symlink(outside.path(), module.path().join("esc")).expect("escaping link");
    enter(module.path());
    (module, outside)
}

fn same_directory(fd: &std::os::fd::OwnedFd, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let opened = rustix::fs::fstat(fd).expect("fstat");
    let named = fs::metadata(path).expect("stat");
    opened.st_ino == named.ino() && opened.st_dev == named.dev()
}

#[test]
fn opens_the_cwd_itself_and_follows_an_in_tree_link() {
    let (module, _outside) = module();

    let root = open_dir_beneath_cwd(Path::new(".")).expect("open the cwd");
    assert!(same_directory(&root, module.path()));

    let inner = open_dir_beneath_cwd(Path::new("./rel/")).expect("follow rel");
    assert!(same_directory(&inner, &module.path().join("inner")));
}

#[test]
fn refuses_a_link_that_leaves_the_tree_with_eloop() {
    let (_module, _outside) = module();

    let error = open_dir_beneath_cwd(Path::new("esc")).expect_err("esc leaves the module");
    assert_eq!(error.raw_os_error(), Some(libc::ELOOP), "{error}");
}

#[test]
fn refuses_an_absolute_path_and_a_dotdot_component_with_einval() {
    let (module, _outside) = module();

    for path in [
        module.path().join("inner"),
        Path::new("inner/..").to_path_buf(),
    ] {
        let error = open_dir_beneath_cwd(&path).expect_err("not a plain relative path");
        assert_eq!(
            error.raw_os_error(),
            Some(libc::EINVAL),
            "{}: {error}",
            path.display()
        );
    }
}

#[test]
fn makes_a_directory_through_an_in_tree_link() {
    let (module, _outside) = module();

    create_dir_beneath_cwd(Path::new("rel/new"), 0o777).expect("mkdir through rel");
    assert!(module.path().join("inner/new").is_dir());
}

#[test]
fn refuses_a_parent_that_leaves_the_tree_before_making_anything() {
    let (_module, outside) = module();

    let error =
        create_dir_beneath_cwd(Path::new("esc/new"), 0o777).expect_err("esc leaves the module");
    assert_eq!(error.raw_os_error(), Some(libc::ELOOP), "{error}");
    assert_eq!(fs::read_dir(outside.path()).expect("list").count(), 0);
}

/// One `do_mkdir()`: a missing parent is `mkdir()`'s own `ENOENT`, and an
/// existing name its `EEXIST` - nothing is made above it.
#[test]
fn keeps_mkdir_errors_for_a_missing_parent_and_an_existing_name() {
    let (module, _outside) = module();

    let missing =
        create_dir_beneath_cwd(Path::new("absent/new"), 0o777).expect_err("the parent is missing");
    assert_eq!(missing.raw_os_error(), Some(libc::ENOENT), "{missing}");
    assert!(!module.path().join("absent").exists());

    let existing =
        create_dir_beneath_cwd(Path::new("inner"), 0o777).expect_err("inner exists already");
    assert_eq!(existing.raw_os_error(), Some(libc::EEXIST), "{existing}");
}
