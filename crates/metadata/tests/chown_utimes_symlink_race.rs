//! Regression test for the ancestor-symlink-swap TOCTOU class on the
//! receiver-side ownership and timestamp application paths.
//!
//! upstream: rsync 3.4.3+ resolves `do_lchown`/`do_utime` under the module
//! directory fd (matching upstream issue CVE-2026-29518).
//!
//! `AT_SYMLINK_NOFOLLOW` only guards the leaf component of a path-based
//! `fchownat`/`utimensat`. A symlink swapped into a receiver-created
//! *ancestor* directory is still followed, redirecting the chown/utimes to a
//! file outside the module. When the daemon runs as root this lets an
//! attacker with write access to the module chown or set-times an arbitrary
//! out-of-module path.
//!
//! `metadata::apply_file_metadata_with_options` is the production entry point
//! that exercises the path-based ownership (`set_owner_like` ->
//! `chown_path`) and timestamp (`set_timestamp_like`) helpers. Each must now
//! route through `fast_io::secure_chown_at` / `fast_io::secure_utimes_at`,
//! which walk the parent through `secure_open_dir` and anchor the syscall on
//! that dirfd so a symlink swapped into any parent component is rejected
//! (`ELOOP`/`EXDEV`/`ENOTDIR`) before the syscall fires.
//!
//! Legitimate case: application through a clean path must succeed and update
//! the destination's timestamps / leave it owned by the caller.
//!
//! Attack case: application through a parent component that is a symlink to an
//! out-of-module directory must error, and the file outside the module must
//! keep its original timestamps.

#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use filetime::{FileTime, set_file_times};
use metadata::{MetadataOptions, apply_file_metadata_with_options};
use tempfile::TempDir;

/// `tempdir()` may sit under a symlink prefix on macOS / some CI runners;
/// canonicalise so the sandbox open succeeds under `RESOLVE_NO_SYMLINKS`.
fn canonical_tempdir() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let canon = fs::canonicalize(dir.path()).expect("canonicalize");
    (dir, canon)
}

fn mtime_of(path: &Path) -> FileTime {
    FileTime::from_last_modification_time(&fs::metadata(path).expect("stat"))
}

/// A distinctive source mtime the receiver would propagate onto the
/// destination: 2001-09-09T01:46:40Z.
const SOURCE_MTIME: FileTime = FileTime::from_unix_time(1_000_000_000, 0);

fn seed_source(root: &Path) -> (PathBuf, fs::Metadata) {
    let source = root.join("source");
    fs::write(&source, b"src").expect("write source");
    set_file_times(&source, SOURCE_MTIME, SOURCE_MTIME).expect("set source times");
    let meta = fs::metadata(&source).expect("source meta");
    (source, meta)
}

/// Downcast an `apply_file_metadata_with_options` error to its underlying
/// `errno`, if any.
fn errno_of(err: &(dyn Error + 'static)) -> Option<i32> {
    err.source()
        .and_then(|e| e.downcast_ref::<std::io::Error>())
        .and_then(|e| e.raw_os_error())
}

/// Plant `module/subdir -> outside` and return the trap destination
/// `module/subdir/sentinel` plus the out-of-module sentinel file, seeded with
/// a mtime distinct from `SOURCE_MTIME` so an escaped write is observable.
fn plant_ancestor_symlink_trap(root: &Path) -> (PathBuf, PathBuf) {
    let outside = root.join("outside");
    let module = root.join("module");
    fs::create_dir(&outside).expect("mkdir outside");
    fs::create_dir(&module).expect("mkdir module");

    let outside_target = outside.join("sentinel");
    fs::write(&outside_target, b"OUTSIDE").expect("write outside sentinel");
    // A mtime far from SOURCE_MTIME: 1993-03-01T00:00:00Z.
    let witness = FileTime::from_unix_time(730_000_000, 0);
    set_file_times(&outside_target, witness, witness).expect("seed outside mtime");

    // module/subdir -> outside (parent-component symlink trap).
    symlink(&outside, module.join("subdir")).expect("plant symlink");

    let attack_dest = module.join("subdir").join("sentinel");
    (attack_dest, outside_target)
}

// -------------------------------------------------------------------------
// Timestamps (utimensat) - `--times`
// -------------------------------------------------------------------------

/// Legitimate path: `-t` through non-symlink components must set the
/// destination's mtime to the source value.
#[test]
fn receiver_utimes_succeeds_on_clean_path() {
    let (_keep, root) = canonical_tempdir();
    let (_source, source_meta) = seed_source(&root);

    let destination = root.join("dest");
    fs::write(&destination, b"dst").expect("write dest");
    // Backdate the dest so the quick-check does not skip the utimes.
    let old = FileTime::from_unix_time(100, 0);
    set_file_times(&destination, old, old).expect("backdate dest");

    let options = MetadataOptions::default().preserve_times(true);
    apply_file_metadata_with_options(&destination, &source_meta, &options)
        .expect("utimes clean path");

    assert_eq!(
        mtime_of(&destination),
        SOURCE_MTIME,
        "receiver must apply the source mtime through the clean parent path"
    );
}

/// Attack path: an attacker swaps a symlink into the immediate parent
/// component pointing outside the module. The receiver utimes must refuse the
/// syscall and the outside file's mtime must be unchanged.
///
/// Pre-fix this test fails: the path-based `utimensat(AT_FDCWD, ...)` follows
/// the symlinked parent and rewrites `outside/sentinel`'s mtime to
/// `SOURCE_MTIME`. Post-fix `secure_utimes_at` rejects the symlinked parent
/// before any `utimensat` fires.
#[test]
fn receiver_utimes_refuses_symlinked_parent_component() {
    let (_keep, root) = canonical_tempdir();
    let (_source, source_meta) = seed_source(&root);
    let (attack_dest, outside_target) = plant_ancestor_symlink_trap(&root);
    let sentinel_mtime_before = mtime_of(&outside_target);

    let options = MetadataOptions::default().preserve_times(true);
    let err = apply_file_metadata_with_options(&attack_dest, &source_meta, &options)
        .expect_err("utimes through symlinked parent must error");
    let raw = errno_of(err.as_ref());
    assert!(
        matches!(
            raw,
            Some(libc::ELOOP) | Some(libc::EXDEV) | Some(libc::ENOTDIR)
        ),
        "expected ELOOP/EXDEV/ENOTDIR from secure_utimes_at, got {raw:?}"
    );

    assert_eq!(
        mtime_of(&outside_target),
        sentinel_mtime_before,
        "outside sentinel mtime must be unchanged - utimes must not escape the module"
    );
    assert_ne!(
        mtime_of(&outside_target),
        SOURCE_MTIME,
        "the escaped source mtime must never land on the outside sentinel"
    );
}

// -------------------------------------------------------------------------
// Ownership (fchownat) - `--chown`
// -------------------------------------------------------------------------

/// Legitimate path: an explicit `--chown` to the caller's own uid/gid through
/// a clean path must succeed (no privilege required) and leave the
/// destination owned by the caller.
#[test]
fn receiver_chown_succeeds_on_clean_path() {
    let (_keep, root) = canonical_tempdir();
    let (_source, source_meta) = seed_source(&root);

    let destination = root.join("dest");
    fs::write(&destination, b"dst").expect("write dest");
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o644)).expect("dest perms");
    let dest_meta = fs::symlink_metadata(&destination).expect("dest meta");
    let (my_uid, my_gid) = (dest_meta.uid(), dest_meta.gid());

    let options = MetadataOptions::default()
        .with_owner_override(Some(my_uid))
        .with_group_override(Some(my_gid));
    apply_file_metadata_with_options(&destination, &source_meta, &options)
        .expect("chown-to-self clean path");

    let after = fs::symlink_metadata(&destination).expect("dest meta after");
    assert_eq!(
        (after.uid(), after.gid()),
        (my_uid, my_gid),
        "chown-to-self through a clean path must leave the caller as owner"
    );
}

/// Attack path: an attacker swaps a symlink into the immediate parent
/// component pointing outside the module. The receiver chown must refuse the
/// syscall (the sandbox-anchored `secure_open_dir` rejects the symlinked
/// parent) before any `fchownat` fires.
///
/// The `--chown` targets the caller's own uid/gid so no privilege is needed;
/// `preserve_times` seeds an mtime witness so an escaped metadata write of any
/// kind is observable on the outside sentinel.
#[test]
fn receiver_chown_refuses_symlinked_parent_component() {
    let (_keep, root) = canonical_tempdir();
    let (_source, source_meta) = seed_source(&root);
    let (attack_dest, outside_target) = plant_ancestor_symlink_trap(&root);
    let sentinel = fs::symlink_metadata(&outside_target).expect("sentinel meta");
    let sentinel_mtime_before = mtime_of(&outside_target);

    let options = MetadataOptions::default()
        .with_owner_override(Some(sentinel.uid()))
        .with_group_override(Some(sentinel.gid()))
        .preserve_times(true);
    let err = apply_file_metadata_with_options(&attack_dest, &source_meta, &options)
        .expect_err("chown through symlinked parent must error");
    let raw = errno_of(err.as_ref());
    assert!(
        matches!(
            raw,
            Some(libc::ELOOP) | Some(libc::EXDEV) | Some(libc::ENOTDIR)
        ),
        "expected ELOOP/EXDEV/ENOTDIR from secure_chown_at, got {raw:?}"
    );

    assert_eq!(
        mtime_of(&outside_target),
        sentinel_mtime_before,
        "outside sentinel must be untouched - no metadata write may escape the module"
    );
}
