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

/// Broken (dangling) symlink at the dest root must error rather than
/// auto-create a directory at the missing link target.
///
/// Upstream `main.c:745-754` accepts a symlinked dest only when
/// `do_stat()` resolves to a directory. A dangling link returns `ENOENT`
/// from `do_stat`, falling through to `do_mkdir(dest_path, ACCESSPERMS)`
/// which would resolve the link and materialize a directory at the
/// outside-the-module target. oc-rsync diverges intentionally on this
/// edge: the helper combines the stat `NotFound` result with an lstat
/// follow-up; if the path is actually a dangling symlink we propagate
/// `NotFound` instead of calling `create_dir_all`. This keeps the
/// auto-create path safe without the strict refusal that broke the
/// `symlink-dirlink-basis` interop scenario (UTS-SLDB).
#[cfg(unix)]
#[test]
fn rejects_broken_symlink_dest_root() {
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
        .expect_err("dangling-symlink dest root must surface NotFound");

    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    assert!(
        !outside_target.exists(),
        "helper must not materialize the symlink target"
    );
}

/// Symlink to an existing directory at the dest root must be accepted -
/// upstream `main.c:745-754` follows the symlink via `do_stat` + `S_ISDIR`
/// and enters the resolved directory. This is the UTS-SLDB interop scenario
/// (issue #715): `$HOME/dir -> $HOME/real-dir` with `-K` so every write
/// lands inside the real directory.
///
/// Per-entry writes are still sandboxed by the SEC-1 `*at` chain (see
/// `transfer::receiver::transfer::setup` for the `DirSandbox::open_root`
/// call site). The setup site canonicalizes a symlinked dest_dir before
/// opening the sandbox, so a malicious post-helper swap of the link is
/// closed by the kernel resolving the link target once into a dirfd.
#[cfg(unix)]
#[test]
fn accepts_symlink_to_existing_directory() {
    use std::os::unix::fs::symlink;

    let tmp = tempdir().expect("tempdir");
    let real_dir = tmp.path().join("real_subdir");
    fs::create_dir(&real_dir).expect("create real intra-module dir");
    let dest = tmp.path().join("dest_root");
    symlink(&real_dir, &dest).expect("symlink dest -> real_dir");

    let created = ensure_dest_root_exists(&dest, 5, false, false)
        .expect("symlink-to-dir dest must be accepted");

    assert!(
        !created,
        "no creation when dest resolves to an existing directory"
    );
    assert!(real_dir.is_dir(), "real directory must remain intact");
    assert!(
        dest.symlink_metadata()
            .expect("lstat dest")
            .file_type()
            .is_symlink(),
        "symlink itself is preserved; helper must not unlink it"
    );
}

/// Symlink to an existing directory outside the dest's lexical parent is
/// also accepted at this layer. Containment is enforced downstream:
///
/// - The per-entry SEC-1 `*at` chain anchors every open at the resolved
///   dest dirfd, so writes cannot escape that fd.
/// - Daemon-mode `module.path` containment is enforced by the daemon
///   module loader, not by this pre-flight.
///
/// This test pins the new behavior so a future regression that re-adds
/// the strict refusal would fail loud here instead of silently breaking
/// the `symlink-dirlink-basis` interop test.
#[cfg(unix)]
#[test]
fn accepts_symlink_to_existing_directory_outside_parent() {
    use std::os::unix::fs::symlink;

    let module_root = tempdir().expect("module tempdir");
    let outside_root = tempdir().expect("outside tempdir");
    let outside_dir = outside_root.path().join("outside_target");
    fs::create_dir(&outside_dir).expect("create outside dir");
    let dest = module_root.path().join("dest_root");
    symlink(&outside_dir, &dest).expect("dest -> outside dir");

    let created = ensure_dest_root_exists(&dest, 8, true, false)
        .expect("symlink-to-dir-outside-parent dest must be accepted");

    assert!(!created, "no creation when dest resolves to existing dir");
    assert!(outside_dir.is_dir(), "outside directory must remain intact");
}

/// A regular file at the dest root must still be rejected: stat succeeds
/// but `is_dir()` is false. Upstream `main.c:756-761` errors out with
/// "destination must be a directory when copying more than 1 file".
#[test]
fn rejects_existing_non_directory_at_dest_root() {
    let tmp = tempdir().expect("tempdir");
    let dest = tmp.path().join("existing_file");
    fs::write(&dest, b"x").expect("seed file");

    let err = ensure_dest_root_exists(&dest, 5, true, false)
        .expect_err("non-directory dest root must error");

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(dest.is_file(), "the file stays intact");
}

/// UTS-SLDB: a symlink-to-dangling-link-target (one link points at
/// another link that points at nothing) must be refused via the lstat
/// fallback. We probe the immediate link with `symlink_metadata`; if it
/// is a symlink and stat fails, we treat it as dangling regardless of
/// chain length. This pins the safe-default for the auto-create path.
#[cfg(unix)]
#[test]
fn rejects_chained_dangling_symlink_dest_root() {
    use std::os::unix::fs::symlink;

    let tmp = tempdir().expect("tempdir");
    let dangling_link = tmp.path().join("link_a");
    let intermediate = tmp.path().join("link_b");
    // link_a -> link_b (which does not exist)
    symlink(&intermediate, &dangling_link).expect("symlink link_a -> link_b");

    let err = ensure_dest_root_exists(&dangling_link, 3, false, false)
        .expect_err("dangling-chain symlink dest must error");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    assert!(
        !intermediate.exists(),
        "no auto-create through the dangling chain"
    );
}
