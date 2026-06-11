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

/// Broken symlink pointing OUTSIDE the would-be containment must be refused.
///
/// The pre-PR-5567 helper called `dest_root.metadata()`, which follows the
/// symlink. For a dangling link the stat returns ENOENT, the helper then
/// calls `create_dir_all(dest_root)`, and the std machinery resolves the
/// symlink to materialize a directory at the link target - a containment
/// bypass analogous to the SEC-1 TOCTOU family. The lstat-based check now
/// observes the symlink directly and refuses.
#[cfg(unix)]
#[test]
fn refuses_broken_symlink_to_outside_module() {
    use std::os::unix::fs::symlink;

    let tmp = tempdir().expect("tempdir");
    let outside_root = tempdir().expect("outside tempdir");
    let outside_target = outside_root.path().join("nonexistent/target");
    let dest = tmp.path().join("dest_root");
    symlink(&outside_target, &dest).expect("create broken symlink");
    assert!(
        dest.symlink_metadata()
            .expect("lstat dest")
            .file_type()
            .is_symlink(),
        "fixture must place a symlink at dest"
    );
    assert!(!outside_target.exists(), "target must remain dangling");

    let err = ensure_dest_root_exists(&dest, 3, false, false)
        .expect_err("symlinked dest_root must be refused");

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        err.to_string().contains("symlink"),
        "error must name the symlink condition, got: {err}"
    );
    assert!(
        !outside_target.exists(),
        "helper must not materialize the symlink target"
    );
}

/// Broken symlink pointing inside the eventual module path is still suspect.
///
/// Even if the target happens to land inside the module root, accepting a
/// symlink-as-dest leaves the per-entry writes resolving through a link the
/// receiver did not create - the dest may be re-pointed later or shared with
/// an attacker-writable location. Refusing is the safe-default.
#[cfg(unix)]
#[test]
fn refuses_broken_symlink_to_intra_module_target() {
    use std::os::unix::fs::symlink;

    let tmp = tempdir().expect("tempdir");
    let inside_target = tmp.path().join("would-be-safe/leaf");
    let dest = tmp.path().join("dest_root");
    symlink(&inside_target, &dest).expect("create broken intra-module symlink");

    let err = ensure_dest_root_exists(&dest, 4, true, false)
        .expect_err("intra-module symlinked dest is still refused");

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        !inside_target.exists(),
        "helper must not auto-create at the symlink target even when intra-module"
    );
}

/// Symlink to an existing directory inside the eventual module is refused
/// too: the helper has no protocol-level signal that the link was placed by
/// the operator rather than by an attacker, and a permissive helper here
/// would break the SEC-1 containment that downstream `*at` calls rely on.
#[cfg(unix)]
#[test]
fn refuses_symlink_to_existing_intra_module_directory() {
    use std::os::unix::fs::symlink;

    let tmp = tempdir().expect("tempdir");
    let real_dir = tmp.path().join("real_subdir");
    fs::create_dir(&real_dir).expect("create real intra-module dir");
    let dest = tmp.path().join("dest_root");
    symlink(&real_dir, &dest).expect("symlink dest -> real_dir");

    let err = ensure_dest_root_exists(&dest, 5, false, false)
        .expect_err("symlinked dest_root is refused even when target exists");

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    // The real directory must remain untouched - no per-entry writes ever
    // resolved through the link because the helper aborted first.
    assert!(real_dir.is_dir());
    assert!(
        dest.symlink_metadata()
            .expect("lstat dest")
            .file_type()
            .is_symlink(),
        "symlink itself is preserved; helper must not unlink it"
    );
}

/// Symlink pointing to an existing directory outside the module must be
/// refused on containment grounds. A permissive helper here would let every
/// subsequent per-entry write land at the outside-the-module target.
#[cfg(unix)]
#[test]
fn refuses_symlink_to_existing_directory_outside_module() {
    use std::os::unix::fs::symlink;

    let module_root = tempdir().expect("module tempdir");
    let outside_root = tempdir().expect("outside tempdir");
    let outside_dir = outside_root.path().join("outside_target");
    fs::create_dir(&outside_dir).expect("create outside dir");
    let dest = module_root.path().join("dest_root");
    symlink(&outside_dir, &dest).expect("dest -> outside dir");

    let err = ensure_dest_root_exists(&dest, 8, true, false)
        .expect_err("outside-the-module symlinked dest must be refused");

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        outside_dir
            .read_dir()
            .expect("read outside dir")
            .next()
            .is_none(),
        "outside directory must remain untouched - no writes leaked through the symlink"
    );
}
