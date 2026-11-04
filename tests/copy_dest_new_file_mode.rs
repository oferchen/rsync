//! A `--copy-dest` copy takes a new file's mode, not the basis file's.
//!
//! upstream: generator.c:917-953 `copy_altdest_file()` creates the copy with
//! `file->mode`, which recv_generator() has already set through
//! `dest_mode(file->mode, ..., exists = 0)` when `--perms` is off
//! (rsync.c:464-487): the source mode washed by the umask. The basis file's
//! own permission bits never reach the destination.
//!
//! oc copied the basis with its mode and then applied metadata as if the copy
//! were an existing destination, so a `0600` alt file landed as `0600` where
//! 3.5.1 lands `0644` (push and pull; the local copy already matched).
//!
//! `--link-dest` is the control: the destination IS the basis inode, so it
//! keeps the basis mode on both implementations and the reference tree must
//! not be chmod'ed.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use filetime::FileTime;

const PAYLOAD: &[u8] = b"copy-dest mode payload, identical in source and alt dir\n";

/// `CARGO_BIN_EXE_oc-rsync` is a compile-time variable; reading it at run
/// time would fall through to whatever stale binary is on disk.
fn oc_rsync_binary() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    assert!(
        built.is_file(),
        "oc-rsync binary missing at {}; refusing to fall back to a stale build",
        built.display()
    );
    built
}

/// `src/f` at 0644 and an identical `alt/f` at 0600 with the same mtime, an
/// empty `dst/`, and an rsh shim. Returns the shim.
fn seed(root: &Path) -> PathBuf {
    for dir in ["src", "dst", "alt"] {
        fs::create_dir_all(root.join(dir)).expect("mkdir");
    }
    let mtime = FileTime::from_unix_time(1_577_836_800, 0);
    for (file, mode) in [(root.join("src/f"), 0o644), (root.join("alt/f"), 0o600)] {
        fs::write(&file, PAYLOAD).expect("write payload");
        fs::set_permissions(&file, fs::Permissions::from_mode(mode)).expect("chmod");
        filetime::set_file_mtime(&file, mtime).expect("set mtime");
    }
    let shim = root.join("rsh.sh");
    fs::write(&shim, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh shim");
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).expect("chmod shim");
    shim
}

/// Runs one transfer of `src/f` into `dst/` and returns `dst/f`'s mode.
fn transfer(root: &Path, alt_option: &str, pull: bool) -> u32 {
    let shim = seed(root);
    let binary = oc_rsync_binary();
    let (source, dest) = if pull {
        (
            format!("localhost:{}", root.join("src/f").display()),
            format!("{}/", root.join("dst").display()),
        )
    } else {
        (
            root.join("src/f").display().to_string(),
            format!("localhost:{}/", root.join("dst").display()),
        )
    };
    let output = Command::new(&binary)
        .args(["-t", alt_option, "-e"])
        .arg(&shim)
        .arg(format!("--rsync-path={}", binary.display()))
        .arg(source)
        .arg(dest)
        .output()
        .expect("spawn oc-rsync");
    assert!(
        output.status.success(),
        "exit {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::metadata(root.join("dst/f"))
        .expect("dst/f exists")
        .permissions()
        .mode()
        & 0o7777
}

/// The umask-washed source mode upstream's `dest_mode(..., exists = 0)` gives.
fn new_file_mode() -> u32 {
    let probe = tempfile::tempdir().expect("tempdir");
    let path = probe.path().join("umask-probe");
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .expect("create umask probe");
    // A plain create asks for 0666, so the bits the umask cleared are
    // exactly 0666 minus what landed.
    let umask = 0o666
        & !(fs::metadata(&path)
            .expect("stat probe")
            .permissions()
            .mode()
            & 0o777);
    0o644 & !umask
}

#[test]
fn copy_dest_push_lands_the_new_file_mode() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        transfer(tmp.path(), "--copy-dest=../alt", false),
        new_file_mode(),
        "3.5.1 creates the copy with dest_mode(..., exists = 0), not alt/f's 0600"
    );
}

#[test]
fn copy_dest_pull_lands_the_new_file_mode() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        transfer(tmp.path(), "--copy-dest=../alt", true),
        new_file_mode(),
        "3.5.1 creates the copy with dest_mode(..., exists = 0), not alt/f's 0600"
    );
}

#[test]
fn link_dest_pull_keeps_the_shared_basis_mode() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        transfer(tmp.path(), "--link-dest=../alt", true),
        0o600,
        "a hard link shares alt/f's inode; chmod'ing it would rewrite the reference tree"
    );
    assert_eq!(
        fs::metadata(tmp.path().join("alt/f"))
            .expect("alt/f")
            .permissions()
            .mode()
            & 0o7777,
        0o600
    );
}
