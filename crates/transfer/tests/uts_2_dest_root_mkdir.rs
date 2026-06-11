//! Integration coverage for the receiver-side pre-flight destination root
//! mkdir.
//!
//! Mirrors upstream `main.c:778-792` (`get_local_name()`): when the receiver
//! is about to handle more than one file, or the destination operand carries
//! a trailing slash, the root must exist before per-entry mkdir dispatch.
//! Local-mode receivers got this for free through the file-list-driven
//! implicit mkdir, but the `--server` path (remote shell, daemon) skipped it
//! entirely - the alt-dest upstream interop test failed because the
//! non-existent dest was never created.
//!
//! These tests exercise the `ensure_dest_root_exists` helper across the
//! decision-table cells upstream `main.c` cares about so the gating stays
//! locked in if the call site moves.

use std::fs;

use tempfile::tempdir;

use transfer::receiver::ensure_dest_root_exists;

/// Multi-file transfer into a non-existent destination must create the root.
///
/// Mirrors `main.c:778` (`file_total > 1`).
#[test]
fn creates_dest_root_for_multi_file_transfer() {
    let tmp = tempdir().expect("tempdir");
    let dest = tmp.path().join("new_dest");
    assert!(!dest.exists());

    let created = ensure_dest_root_exists(&dest, 4, false, false).expect("ensure ok");

    assert!(created, "should report newly created");
    assert!(dest.is_dir(), "destination root must exist as a directory");
}

/// Trailing-slash on the operand creates the dest even for a single file,
/// matching `main.c:778` (`trailing_slash`).
#[test]
fn creates_dest_root_for_trailing_slash_single_file() {
    let tmp = tempdir().expect("tempdir");
    let dest = tmp.path().join("with_slash");
    assert!(!dest.exists());

    let created = ensure_dest_root_exists(&dest, 1, true, false).expect("ensure ok");

    assert!(created, "trailing slash forces creation");
    assert!(dest.is_dir());
}

/// Single-file transfer without trailing slash must not pre-create the root -
/// upstream falls through to mode 2 which writes the file directly. Creating
/// the parent here would diverge from upstream and trash existing siblings.
#[test]
fn skips_create_for_single_file_without_trailing_slash() {
    let tmp = tempdir().expect("tempdir");
    let dest = tmp.path().join("single_file_dest");
    assert!(!dest.exists());

    let created =
        ensure_dest_root_exists(&dest, 1, false, false).expect("single-file path must not error");

    assert!(!created, "single-file transfer must not create the root");
    assert!(
        !dest.exists(),
        "no directory should be created for single-file mode 2"
    );
}

/// Pre-existing directory is treated as a no-op, matching upstream's
/// `statret == 0 && S_ISDIR(...)` branch which simply enters the directory.
#[test]
fn pre_existing_directory_is_noop() {
    let tmp = tempdir().expect("tempdir");
    let dest = tmp.path().join("already_there");
    fs::create_dir(&dest).expect("seed dest");

    let created = ensure_dest_root_exists(&dest, 7, false, false).expect("ensure ok");

    assert!(!created, "no creation when dest already exists");
    assert!(dest.is_dir());
}

/// `dry_run` must never touch the filesystem; upstream `main.c:802-803`
/// signals the missing dir by incrementing `dry_run`, not by mkdir.
#[test]
fn dry_run_never_creates_dest_root() {
    let tmp = tempdir().expect("tempdir");
    let dest = tmp.path().join("dry_run_dest");
    assert!(!dest.exists());

    let created = ensure_dest_root_exists(&dest, 100, true, true).expect("dry_run ok");

    assert!(!created);
    assert!(!dest.exists(), "dry-run must not create the destination");
}

/// Walks a multi-component path that lives several levels below an existing
/// directory: `--server` invocations from the upstream alt-dest harness pass
/// destinations like `out/sub/leaf/` whose ancestors do not yet exist. The
/// helper relies on `create_dir_all` so the entire chain materializes.
#[test]
fn creates_nested_dest_root() {
    let tmp = tempdir().expect("tempdir");
    let dest = tmp.path().join("a/b/c/d");
    assert!(!dest.exists());

    let created = ensure_dest_root_exists(&dest, 3, false, false).expect("ensure ok");

    assert!(created);
    assert!(dest.is_dir());
}

/// Surfacing an existing-non-directory dest is upstream's "destination path
/// is not a directory" error at `main.c:782-784`. The helper itself returns
/// `Ok(false)` because metadata succeeds; the per-entry mkdir downstream is
/// what reports the conflict. Lock that behaviour so future refactors do not
/// silently elevate the helper into a destructive replacer.
#[test]
fn existing_non_directory_path_is_noop_for_helper() {
    let tmp = tempdir().expect("tempdir");
    let dest = tmp.path().join("existing_file");
    fs::write(&dest, b"x").expect("seed file");

    let created = ensure_dest_root_exists(&dest, 5, true, false).expect("metadata-only check");

    assert!(!created, "helper must not destroy an existing file");
    assert!(dest.is_file(), "the file stays intact");
}
