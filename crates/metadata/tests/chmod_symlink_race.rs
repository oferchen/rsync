//! Regression test for the chmod-symlink-race TOCTOU class on the
//! receiver-side permission application path.
//!
//! upstream: testsuite/chmod-symlink-race.test (rsync 3.4.3+).
//!
//! When the receiver chmods a file whose parent components are sender-
//! controllable, an attacker can swap a symlink into one of those
//! components between the receiver's check and its act, redirecting the
//! chmod to a file outside the receiver's confinement.
//!
//! `metadata::apply_file_metadata` is the production entry point that
//! exercises the path-based chmod helpers (`apply_permissions_with_chmod`,
//! `set_permissions_like`, `apply_permissions_without_chmod`). Each of
//! those must route through `fast_io::secure_chmod_at` so a symlink
//! swapped into the parent path component is rejected before the chmod
//! syscall fires.
//!
//! Legitimate case: chmod through a clean path must succeed and update
//! the destination's permission bits.
//!
//! Attack case: chmod through a parent component that is a symlink to an
//! out-of-module directory must error (typed I/O error surfaced as an
//! `ELOOP`/`EXDEV`/`ENOTDIR` from `secure_open_dir`), and the file
//! outside the module must keep its original mode.

#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;

use metadata::{MetadataOptions, apply_file_metadata};
use tempfile::TempDir;

/// `tempdir()` may sit under a symlink prefix on macOS / some CI
/// runners; canonicalise so the sandbox open succeeds under
/// `RESOLVE_NO_SYMLINKS`.
fn canonical_tempdir() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let canon = fs::canonicalize(dir.path()).expect("canonicalize");
    (dir, canon)
}

fn perms(mode: u32) -> fs::Permissions {
    <fs::Permissions as PermissionsExt>::from_mode(mode)
}

fn mode_of(path: &std::path::Path) -> u32 {
    fs::metadata(path).expect("stat").permissions().mode() & 0o7777
}

/// Legitimate path: the destination's full path resolves through
/// non-symlink components and the receiver chmod must succeed at the
/// target mode.
#[test]
fn receiver_chmod_succeeds_on_clean_path() {
    let (_keep, root) = canonical_tempdir();
    let source = root.join("source");
    let destination = root.join("dest");
    fs::write(&source, b"src").expect("write source");
    fs::set_permissions(&source, perms(0o640)).expect("source perms");
    fs::write(&destination, b"dst").expect("write dest");
    fs::set_permissions(&destination, perms(0o600)).expect("dest perms");

    let source_meta = fs::metadata(&source).expect("source meta");
    let options = MetadataOptions::default().preserve_permissions(true);

    apply_file_metadata(&destination, &source_meta, &options).expect("chmod clean path");

    assert_eq!(
        mode_of(&destination),
        0o640,
        "receiver chmod must apply the source mode through the clean parent path"
    );
}

/// Attack path: an attacker swaps a symlink into the immediate parent
/// component pointing outside the module. The receiver chmod must refuse
/// the syscall (the sandbox-anchored `secure_open_dir` rejects the
/// symlinked parent) and the outside file's mode must be unchanged.
#[test]
fn receiver_chmod_refuses_symlinked_parent_component() {
    let (_keep, root) = canonical_tempdir();

    let outside = root.join("outside");
    let module = root.join("module");
    fs::create_dir(&outside).expect("mkdir outside");
    fs::create_dir(&module).expect("mkdir module");

    let outside_target = outside.join("sentinel");
    fs::write(&outside_target, b"OUTSIDE").expect("write outside sentinel");
    fs::set_permissions(&outside_target, perms(0o600)).expect("seed outside mode");

    // module/subdir -> outside (parent-component symlink trap).
    symlink(&outside, module.join("subdir")).expect("plant symlink");

    let attack_dest = module.join("subdir").join("sentinel");

    // Source describes a permissive mode the attacker would like to land
    // on the outside file if the symlink swap goes undetected.
    let source = root.join("source");
    fs::write(&source, b"src").expect("write source");
    fs::set_permissions(&source, perms(0o666)).expect("source perms");
    let source_meta = fs::metadata(&source).expect("source meta");

    let options = MetadataOptions::default().preserve_permissions(true);

    let err = apply_file_metadata(&attack_dest, &source_meta, &options)
        .expect_err("chmod through symlinked parent must error");
    let raw = err
        .source()
        .and_then(|e| e.downcast_ref::<std::io::Error>())
        .and_then(|e| e.raw_os_error());
    // Platform-dependent: Linux + openat2 surfaces ELOOP or EXDEV;
    // O_NOFOLLOW | O_DIRECTORY on a symlinked leaf surfaces ELOOP on
    // Linux without openat2 and ENOTDIR on macOS. All three confirm the
    // parent open was refused before any chmod issued.
    assert!(
        matches!(
            raw,
            Some(libc::ELOOP) | Some(libc::EXDEV) | Some(libc::ENOTDIR)
        ),
        "expected ELOOP/EXDEV/ENOTDIR from secure_chmod_at, got {raw:?}"
    );

    assert_eq!(
        mode_of(&outside_target),
        0o600,
        "outside sentinel mode must be unchanged - chmod must not escape the module"
    );
}
