//! `-a` expands in command-line order: a later individual flag overrides it.
//!
//! upstream: options.c:1546 `case 'a'` assigns `preserve_* = 1` inline during the
//! left-to-right argv scan, so `-a --no-perms` clears permission preservation
//! while `--no-perms -a` re-enables it. These tests exercise the observable
//! outcome (the destination's permission bits after a real local transfer)
//! rather than the parsed flag, so they fail if the ordering semantics regress.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).expect("stat dest").permissions().mode() & 0o777
}

/// Runs a local transfer and asserts it succeeded.
fn transfer(args: &[&str]) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = cli::run(args.iter().copied(), &mut stdout, &mut stderr);
    assert_eq!(
        code,
        0,
        "transfer {args:?} failed: {}",
        String::from_utf8_lossy(&stderr)
    );
}

/// Creates a fresh src (mode 0700) and pre-existing dest (mode 0600) with
/// differing contents so the quick-check never skips the transfer.
fn setup(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let src = dir.join("src");
    let dst = dir.join("dst");
    fs::write(&src, b"source-contents").expect("write src");
    fs::write(&dst, b"old").expect("write dst");
    fs::set_permissions(&src, fs::Permissions::from_mode(0o700)).expect("chmod src");
    fs::set_permissions(&dst, fs::Permissions::from_mode(0o600)).expect("chmod dst");
    (src, dst)
}

#[test]
fn no_perms_before_archive_preserves_permissions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (src, dst) = setup(dir.path());
    // `--no-perms -a`: the trailing -a re-enables perms, so the destination
    // takes the source's 0700.
    transfer(&[
        "oc-rsync",
        "--no-perms",
        "-a",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert_eq!(
        mode_of(&dst),
        0o700,
        "-a after --no-perms must re-enable permission preservation"
    );
}

#[test]
fn no_perms_after_archive_drops_permissions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (src, dst) = setup(dir.path());
    // `-a --no-perms`: the trailing --no-perms wins, so the destination keeps
    // its pre-existing 0600.
    transfer(&[
        "oc-rsync",
        "-a",
        "--no-perms",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert_eq!(
        mode_of(&dst),
        0o600,
        "--no-perms after -a must disable permission preservation"
    );
}
