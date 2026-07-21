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
//! The permission apply path is disabled in every case
//! (`preserve_permissions(false)`) so these tests isolate the ownership and
//! timestamp cutovers: with `-p` active the already-hardened
//! `secure_chmod_at` would refuse the symlinked parent first and mask the
//! chown/utimes behaviour under test.
//!
//! Legitimate case: application through a clean path must succeed and update
//! the destination.
//!
//! Attack case: application through a parent component that is a symlink to an
//! out-of-module directory must error, and the file outside the module must
//! be untouched.

#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
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

/// A uid the caller cannot own without privilege, so an escaped chown is
/// observable (the write lands, or fails with `EPERM` rather than the
/// `ELOOP` a refused parent open produces).
const FOREIGN_UID: u32 = 4_242_424;

fn seed_source(root: &Path) -> fs::Metadata {
    let source = root.join("source");
    fs::write(&source, b"src").expect("write source");
    set_file_times(&source, SOURCE_MTIME, SOURCE_MTIME).expect("set source times");
    fs::metadata(&source).expect("source meta")
}

/// Downcast an `apply_file_metadata_with_options` error to its underlying
/// `errno`, if any.
fn errno_of(err: &(dyn Error + 'static)) -> Option<i32> {
    err.source()
        .and_then(|e| e.downcast_ref::<std::io::Error>())
        .and_then(|e| e.raw_os_error())
}

/// Assert an apply error is a refused-parent-open, not any other failure.
fn assert_refused(err: &metadata::MetadataError, helper: &str) {
    let raw = errno_of(err);
    // Platform-dependent: Linux + openat2 surfaces ELOOP or EXDEV;
    // O_NOFOLLOW | O_DIRECTORY on a symlinked component surfaces ELOOP on
    // Linux without openat2 and ENOTDIR on macOS. All three confirm the
    // parent open was refused before the syscall issued.
    assert!(
        matches!(
            raw,
            Some(libc::ELOOP) | Some(libc::EXDEV) | Some(libc::ENOTDIR)
        ),
        "expected ELOOP/EXDEV/ENOTDIR from {helper}, got {raw:?}"
    );
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
// Timestamps (utimensat) - `--times`, `--no-perms`
// -------------------------------------------------------------------------

/// Legitimate path: `-t` through non-symlink components must set the
/// destination's mtime to the source value.
#[test]
fn receiver_utimes_succeeds_on_clean_path() {
    let (_keep, root) = canonical_tempdir();
    let source_meta = seed_source(&root);

    let destination = root.join("dest");
    fs::write(&destination, b"dst").expect("write dest");
    // Backdate the dest so the quick-check does not skip the utimes.
    let old = FileTime::from_unix_time(100, 0);
    set_file_times(&destination, old, old).expect("backdate dest");

    let options = MetadataOptions::new()
        .preserve_permissions(false)
        .preserve_times(true);
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
/// `SOURCE_MTIME`, so the apply returns `Ok` and `expect_err` panics. Post-fix
/// `secure_utimes_at` rejects the symlinked parent before any `utimensat`
/// fires.
#[test]
fn receiver_utimes_refuses_symlinked_parent_component() {
    let (_keep, root) = canonical_tempdir();
    let source_meta = seed_source(&root);
    let (attack_dest, outside_target) = plant_ancestor_symlink_trap(&root);
    let sentinel_mtime_before = mtime_of(&outside_target);

    let options = MetadataOptions::new()
        .preserve_permissions(false)
        .preserve_times(true);
    let err = apply_file_metadata_with_options(&attack_dest, &source_meta, &options)
        .expect_err("utimes through symlinked parent must error");
    assert_refused(&err, "secure_utimes_at");

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
// Ownership (fchownat) - `--chown`, `--no-perms`, `--no-times`
// -------------------------------------------------------------------------

/// Legitimate path: an explicit `--chown` to the caller's own uid/gid through
/// a clean path must succeed (no privilege required) and leave the
/// destination owned by the caller.
#[test]
fn receiver_chown_succeeds_on_clean_path() {
    let (_keep, root) = canonical_tempdir();
    let source_meta = seed_source(&root);

    let destination = root.join("dest");
    fs::write(&destination, b"dst").expect("write dest");
    let dest_meta = fs::symlink_metadata(&destination).expect("dest meta");
    let (my_uid, my_gid) = (dest_meta.uid(), dest_meta.gid());

    let options = MetadataOptions::new()
        .preserve_permissions(false)
        .preserve_times(false)
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
/// parent) before any `fchownat` fires, and the outside file's owner must be
/// unchanged.
///
/// The `--chown` targets a foreign uid the caller cannot own. Pre-fix the
/// path-based `fchownat(AT_FDCWD, ...)` follows the symlinked parent: as root
/// it reowns `outside/sentinel` (apply returns `Ok`, `expect_err` panics); as
/// a normal user it fails with `EPERM` (not the expected `ELOOP`), so the
/// errno assertion fails. Post-fix `secure_chown_at` refuses the symlinked
/// parent before any `fchownat` fires, regardless of privilege.
#[test]
fn receiver_chown_refuses_symlinked_parent_component() {
    let (_keep, root) = canonical_tempdir();
    let source_meta = seed_source(&root);
    let (attack_dest, outside_target) = plant_ancestor_symlink_trap(&root);
    let owner_before = fs::symlink_metadata(&outside_target)
        .expect("sentinel meta")
        .uid();

    let options = MetadataOptions::new()
        .preserve_permissions(false)
        .preserve_times(false)
        .with_owner_override(Some(FOREIGN_UID));
    let err = apply_file_metadata_with_options(&attack_dest, &source_meta, &options)
        .expect_err("chown through symlinked parent must error");
    assert_refused(&err, "secure_chown_at");

    let owner_after = fs::symlink_metadata(&outside_target)
        .expect("sentinel meta")
        .uid();
    assert_eq!(
        owner_after, owner_before,
        "outside sentinel owner must be unchanged - chown must not escape the module"
    );
    assert_ne!(
        owner_after, FOREIGN_UID,
        "the escaped foreign uid must never land on the outside sentinel"
    );
}
