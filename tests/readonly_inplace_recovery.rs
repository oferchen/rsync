//! A read-only destination must still be updated under `--inplace`.
//!
//! upstream 3.5.0 opens the in-place output through a three-arm chain
//! (`receiver.c:1210-1224`): the primary `O_WRONLY|O_CREAT`, Linux's
//! `protected_regular` retry, and finally `open_readonly_inplace()`
//! (`receiver.c:200-287`), which grants owner-write only for the duration of
//! the open and restores the prior mode before returning.
//!
//! oc had the first two arms at both of its in-place open sites and neither had
//! the third, so `--inplace` onto a 0444 file failed the whole transfer with
//! `Permission denied (13)` and left the file untouched.
//!
//! These tests drive the LOCAL-COPY site
//! (`engine/.../copy/transfer/write_strategy.rs`). The receiver site has its own
//! unit tests in `crates/transfer/src/disk_commit/process/file_ops/tests.rs`;
//! the two are separate `OpenOptions` chains and a fix to one does not reach the
//! other.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn oc_rsync_binary() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    if built.is_file() {
        return built;
    }
    PathBuf::from("oc-rsync")
}

fn mode_of(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

/// Seeds a source and a destination whose contents differ, then sets the
/// destination's mode. Both files are given distinct lengths so the quick check
/// cannot legitimately skip the transfer.
fn seed(root: &Path, dest_mode: u32) -> (PathBuf, PathBuf) {
    let source = root.join("source");
    let dest = root.join("dest");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&dest).unwrap();
    fs::write(source.join("f"), b"replacement data\n").unwrap();
    fs::write(dest.join("f"), b"old data\n").unwrap();
    fs::set_permissions(dest.join("f"), fs::Permissions::from_mode(dest_mode)).unwrap();
    (source, dest)
}

fn run_inplace(source: &Path, dest: &Path) -> std::process::Output {
    Command::new(oc_rsync_binary())
        .arg("--inplace")
        .arg("-r")
        .arg(format!("{}/", source.display()))
        .arg(format!("{}/", dest.display()))
        .output()
        .expect("run oc-rsync")
}

/// The fix. Before the third arm existed this exited 23 with
/// `failed to copy file ...: Permission denied (13)` and left `old data`.
#[test]
fn a_read_only_destination_is_updated_in_place_and_keeps_its_mode() {
    let root = tempfile::tempdir().unwrap();
    let (source, dest) = seed(root.path(), 0o444);

    let output = run_inplace(&source, &dest);

    assert!(
        output.status.success(),
        "read-only --inplace update failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(dest.join("f")).unwrap(), b"replacement data\n");
    assert_eq!(
        mode_of(&dest.join("f")),
        0o444,
        "the recovery must restore the prior mode"
    );
}

/// Non-vacuity companion: the same fixture with an already-writable destination
/// never reaches the recovery arm at all. Without it, a harness that could not
/// perform *any* in-place update would still make the test above look like a
/// mode-restore assertion rather than the recovery it is.
#[test]
fn a_writable_destination_takes_the_same_path_without_the_recovery() {
    let root = tempfile::tempdir().unwrap();
    let (source, dest) = seed(root.path(), 0o644);

    let output = run_inplace(&source, &dest);

    assert!(output.status.success());
    assert_eq!(fs::read(dest.join("f")).unwrap(), b"replacement data\n");
    assert_eq!(mode_of(&dest.join("f")), 0o644);
}
