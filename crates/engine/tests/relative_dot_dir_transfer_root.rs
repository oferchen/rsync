//! `--relative` operands decide whether the destination transfer root `.` is
//! stamped from a source directory.
//!
//! Upstream splits a relative operand at its first `/./`. Only when the rest
//! begins with `./` does the sender inject the synthetic transfer-root `.`
//! entry (`implied_dot_dir`), carrying the anchor directory's attributes onto
//! the destination root. An operand such as `src/./f` re-roots the relative
//! path but adds no `.` entry, so the destination root is left alone - a
//! receiver that does not own it (an unprivileged user in a user namespace)
//! must not fail with `EPERM` trying to stamp it.
//!
//! # Upstream Reference
//!
//! - `flist.c:2862-2881` - the `/./` split and the `implied_dot_dir` test.
//! - `flist.c:2929-2932` - the synthetic `.` sent with `FLAG_IMPLIED_DIR`.
//! - `flist.c:2913-2917` - an empty remainder becomes the real `.` entry
//!   (`DOTDIR_NAME`), which is how `src/./` still stamps the root.
#![cfg(unix)]
use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use filetime::{FileTime, set_file_mtime};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tempfile::{TempDir, tempdir};
/// Source anchor and destination root start on distinct, well-separated
/// mtimes, so a stamp from the anchor is unmistakable.
struct Fixture {
    _temp: TempDir,
    src: PathBuf,
    dest: PathBuf,
    src_mtime: SystemTime,
}
fn fixture() -> Fixture {
    let temp = tempdir().expect("tempdir");
    let src = temp.path().join("src");
    let dest = temp.path().join("dest");
    fs::create_dir_all(src.join("sub")).expect("create source tree");
    fs::create_dir_all(&dest).expect("create destination root");
    fs::write(src.join("f"), b"payload").expect("write f");
    fs::write(src.join("sub").join("g"), b"payload").expect("write g");
    let src_mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(978_307_200);
    set_file_mtime(&src, FileTime::from_system_time(src_mtime)).expect("backdate src");
    let dest_mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1_115_251_200);
    set_file_mtime(&dest, FileTime::from_system_time(dest_mtime)).expect("backdate dest");
    Fixture {
        _temp: temp,
        src,
        dest,
        src_mtime,
    }
}
fn copy_relative(operand: OsString, dest: &Path) {
    let mut dest_operand = dest.as_os_str().to_owned();
    dest_operand.push("/");
    let plan = LocalCopyPlan::from_operands(&[operand, dest_operand]).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .relative_paths(true)
        .times(true)
        .permissions(true);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("relative copy succeeds");
}
fn mtime(path: &Path) -> SystemTime {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .unwrap_or_else(|error| panic!("mtime {}: {error}", path.display()))
}
fn operand(base: &Path, suffix: &str) -> OsString {
    let mut operand = base.as_os_str().to_owned();
    operand.push(suffix);
    operand
}
/// `src/./f` re-roots the path at `src` but injects no `.`: the destination
/// root must not take the source anchor's mtime.
#[test]
fn mid_path_marker_on_a_file_leaves_the_destination_root_unstamped() {
    let fx = fixture();
    copy_relative(operand(&fx.src, "/./f"), &fx.dest);
    assert!(fx.dest.join("f").is_file(), "f must be copied");
    assert_ne!(
        mtime(&fx.dest),
        fx.src_mtime,
        "src/./f must not stamp the destination root from src"
    );
}
/// The same holds for a directory operand: `src/./sub` stamps `dest/sub`, never
/// the destination root itself.
#[test]
fn mid_path_marker_on_a_directory_leaves_the_destination_root_unstamped() {
    let fx = fixture();
    copy_relative(operand(&fx.src, "/./sub"), &fx.dest);
    assert!(
        fx.dest.join("sub").join("g").is_file(),
        "sub/g must be copied"
    );
    assert_ne!(
        mtime(&fx.dest),
        fx.src_mtime,
        "src/./sub must not stamp the destination root from src"
    );
}
/// A remainder that itself begins with `./` (`src/././f`) is upstream's
/// `implied_dot_dir` case: the destination root takes the anchor's mtime.
#[test]
fn remainder_beginning_with_dot_slash_stamps_the_destination_root() {
    let fx = fixture();
    copy_relative(operand(&fx.src, "/././f"), &fx.dest);
    assert!(fx.dest.join("f").is_file(), "f must be copied");
    assert_eq!(
        mtime(&fx.dest),
        fx.src_mtime,
        "src/././f must stamp the destination root from src"
    );
}
/// An empty remainder (`src/./`) is the real `.` entry for `src`, so the
/// destination root takes the anchor's mtime.
#[test]
fn empty_remainder_stamps_the_destination_root_from_the_anchor() {
    let fx = fixture();
    copy_relative(operand(&fx.src, "/./"), &fx.dest);
    assert!(fx.dest.join("f").is_file(), "contents must be copied");
    assert_eq!(
        mtime(&fx.dest),
        fx.src_mtime,
        "src/./ must stamp the destination root from src"
    );
}
