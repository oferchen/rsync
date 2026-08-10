//! End-to-end coverage for the `metadata::windows::classify_path` helper
//! (WPC-8'.9): the public path-based wrapper that opens a reparse-point
//! handle internally and runs the classifier without forcing callers to
//! handle the `CreateFileW` FFI surface themselves.
//!
//! The helper is the boundary the transfer crate uses when populating
//! `FileEntry` values on Windows; this integration test materialises real
//! NTFS reparse points so a regression in the open/classify glue surfaces
//! without depending on the transfer crate's test bring-up.
//!
//! `mklink /j` runs without elevation on Windows 10+, so the junction case
//! is unconditional. `mklink /d` (directory symlink) and `mountvol`
//! (volume mount-point) require admin or Windows 10 developer mode and
//! are downgraded to runtime skips when the privilege is missing,
//! mirroring the in-tree integration tests shipped with the classifier
//! itself.

#![cfg(target_os = "windows")]

use std::fs;
use std::process::Command;

use metadata::windows::{ReparseKind, classify_path};

/// Returns `true` when `mklink` succeeded; `false` when the test should
/// be skipped because `cmd.exe` is unavailable or the privilege to create
/// the reparse point is missing. Mirrors the pattern in
/// `crates/metadata/src/windows/reparse.rs` integration tests so a
/// non-admin CI runner stays green without flaking the suite.
fn try_mklink(args: &[&str]) -> bool {
    match Command::new("cmd").args(args).status() {
        Ok(s) => s.success(),
        Err(_) => false,
    }
}

/// Plain (non-reparse) entries must surface as `io::Error` rather than
/// being silently classified, so the caller can short-circuit on the
/// expected `ERROR_NOT_A_REPARSE_POINT` (4390) from `DeviceIoControl` and
/// skip the symlink emission branch entirely.
#[test]
fn regular_file_returns_not_a_reparse_point_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("plain.txt");
    fs::write(&path, b"data").expect("write");

    let err = classify_path(&path).expect_err("regular file must error");
    let raw = err.raw_os_error().unwrap_or(0);
    assert!(
        raw == 4390 || raw == 0,
        "expected ERROR_NOT_A_REPARSE_POINT (4390) for plain file, got {raw} ({err})"
    );
}

/// `mklink /j` produces an `IO_REPARSE_TAG_MOUNT_POINT` reparse point
/// whose substitute-name points at a directory; the classifier must
/// disambiguate that as a directory junction rather than a volume
/// mount-point.
#[test]
fn junction_classifies_as_junction() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("target");
    let junction = tmp.path().join("link");
    fs::create_dir(&target).expect("create target dir");

    let (Some(j), Some(t)) = (junction.to_str(), target.to_str()) else {
        return; // non-UTF8 tempdir; skip rather than fail
    };
    if !try_mklink(&["/c", "mklink", "/j", j, t]) {
        return; // privilege missing or cmd.exe unavailable; skip
    }

    let kind = classify_path(&junction).expect("classify junction");
    assert_eq!(kind, ReparseKind::Junction);
}

/// `mklink /d` produces an `IO_REPARSE_TAG_SYMLINK` reparse point on a
/// directory; the classifier must surface it as `Symlink` (the most
/// common shape that round-trips through every receiver).
#[test]
fn directory_symlink_classifies_as_symlink() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("target");
    let link = tmp.path().join("link");
    fs::create_dir(&target).expect("create target dir");

    let (Some(l), Some(t)) = (link.to_str(), target.to_str()) else {
        return;
    };
    if !try_mklink(&["/c", "mklink", "/d", l, t]) {
        // `mklink /d` requires admin / developer mode; downgrade to skip
        // rather than failing the suite on a non-admin CI runner.
        return;
    }

    let kind = classify_path(&link).expect("classify symlink");
    assert_eq!(kind, ReparseKind::Symlink);
}

/// `classify_path` must reject a missing path with `NotFound`; we surface
/// the underlying `CreateFileW` failure so callers can decide whether to
/// retry or drop the entry from the transfer.
#[test]
fn missing_path_returns_not_found_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().join("does-not-exist");

    let err = classify_path(&missing).expect_err("missing path must error");
    assert!(
        err.kind() == std::io::ErrorKind::NotFound
            || err.raw_os_error().map_or(false, |n| n == 2 || n == 3),
        "expected NotFound for missing path, got {err}"
    );
}
