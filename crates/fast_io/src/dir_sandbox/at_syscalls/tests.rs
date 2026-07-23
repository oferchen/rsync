//! Unit tests for the dirfd-anchored `*at` syscall helpers.
//!
//! These are Unix syscall tests (`fstatat`/`unlinkat`/`renameat`/...);
//! they exercise both the sandbox-anchored fast path and the
//! path-based fallback for every SEC-1 cutover family.

use std::os::fd::AsFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

use tempfile::tempdir;

use super::*;
use crate::dir_sandbox::DirSandbox;
use crate::secure_dir::secure_open_dir;

fn canonical_tempdir() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempdir().expect("tempdir");
    let canon = std::fs::canonicalize(dir.path()).expect("canonicalize");
    (dir, canon)
}

#[test]
fn fstatat_nofollow_stats_regular_file() {
    let (_keep, root) = canonical_tempdir();
    std::fs::write(root.join("file"), b"hello").expect("write");
    let dirfd = secure_open_dir(&root).expect("open root");

    let meta = fstatat_nofollow(dirfd.as_fd(), OsStr::new("file")).expect("fstatat");
    assert!(meta.is_file());
    assert!(!meta.is_symlink());
    assert!(!meta.is_dir());
    assert_eq!(meta.size(), 5);
}

#[test]
fn fstatat_nofollow_stats_directory() {
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("sub")).expect("mkdir");
    let dirfd = secure_open_dir(&root).expect("open root");

    let meta = fstatat_nofollow(dirfd.as_fd(), OsStr::new("sub")).expect("fstatat");
    assert!(meta.is_dir());
    assert!(!meta.is_file());
    assert!(!meta.is_symlink());
}

#[test]
fn fstatat_nofollow_rejects_symlink_leaf() {
    // SEC-1.f core invariant: the helper must observe the symlink
    // itself rather than the entry it points at. A path-based
    // `fs::metadata` would follow and report the target.
    let (_keep, root) = canonical_tempdir();
    std::fs::write(root.join("target"), b"contents").expect("write target");
    symlink(root.join("target"), root.join("link")).expect("symlink");

    let dirfd = secure_open_dir(&root).expect("open root");
    let meta = fstatat_nofollow(dirfd.as_fd(), OsStr::new("link")).expect("fstatat link");

    assert!(
        meta.is_symlink(),
        "AT_SYMLINK_NOFOLLOW must report the symlink itself, not its target"
    );
    assert!(!meta.is_file());
}

#[test]
fn fstatat_nofollow_reports_enoent_for_missing_leaf() {
    let (_keep, root) = canonical_tempdir();
    let dirfd = secure_open_dir(&root).expect("open root");

    let err = fstatat_nofollow(dirfd.as_fd(), OsStr::new("does-not-exist"))
        .expect_err("missing leaf must error");
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
}

#[test]
fn fstatat_nofollow_exposes_dev_and_ino() {
    let (_keep, root) = canonical_tempdir();
    let path = root.join("file");
    std::fs::write(&path, b"x").expect("write");
    let dirfd = secure_open_dir(&root).expect("open root");

    let meta = fstatat_nofollow(dirfd.as_fd(), OsStr::new("file")).expect("fstatat");
    let std_meta = std::fs::symlink_metadata(&path).expect("symlink_metadata");
    assert_eq!(meta.ino(), std_meta.ino());
    assert_eq!(meta.dev(), std_meta.dev());
}

#[test]
fn lstat_via_sandbox_takes_at_path_for_single_component() {
    let (_keep, root) = canonical_tempdir();
    std::fs::write(root.join("file"), b"hello").expect("write");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let leaf = Path::new("file");
    let link = root.join(leaf);
    let outcome = lstat_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &link).expect("lstat");
    assert!(matches!(outcome, LstatOutcome::At(_)));
}

#[test]
fn lstat_via_sandbox_multi_component_anchors_or_falls_back() {
    // A multi-component relative path now resolves its parent under
    // openat2(RESOLVE_BENEATH) where the kernel supports it, and only
    // degrades to the path-based fallback where it does not. Assert the
    // correct outcome variant for each capability state and confirm the
    // reported dev/ino matches the real entry either way.
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("sub")).expect("mkdir sub");
    std::fs::write(root.join("sub/file"), b"hello").expect("write");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let rel = Path::new("sub/file");
    let link = root.join(rel);
    let outcome = lstat_via_sandbox_or_fallback(Some(&sandbox), &root, rel, &link).expect("lstat");

    if crate::linux_capabilities::openat2_supported() {
        assert!(
            matches!(outcome, LstatOutcome::At(_)),
            "multi-component paths must anchor via openat2(RESOLVE_BENEATH) when supported"
        );
    } else {
        assert!(
            matches!(outcome, LstatOutcome::Std(_)),
            "multi-component paths degrade to the path-based fallback without openat2"
        );
    }

    let std_meta = std::fs::symlink_metadata(&link).expect("std stat");
    assert_eq!(outcome.dev(), std_meta.dev());
    assert_eq!(outcome.ino(), std_meta.ino());
}

#[test]
fn lstat_via_sandbox_falls_back_when_sandbox_absent() {
    let (_keep, root) = canonical_tempdir();
    std::fs::write(root.join("file"), b"hello").expect("write");

    let leaf = Path::new("file");
    let link = root.join(leaf);
    let outcome = lstat_via_sandbox_or_fallback(None, &root, leaf, &link).expect("lstat");
    assert!(matches!(outcome, LstatOutcome::Std(_)));
}

#[test]
fn lstat_via_sandbox_outcome_matches_dev_ino_across_paths() {
    let (_keep, root) = canonical_tempdir();
    let path = root.join("file");
    std::fs::write(&path, b"x").expect("write");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let leaf = Path::new("file");
    let via_at =
        lstat_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &path).expect("at-path lstat");
    let via_std = lstat_via_sandbox_or_fallback(None, &root, leaf, &path).expect("std lstat");
    assert_eq!(via_at.dev(), via_std.dev());
    assert_eq!(via_at.ino(), via_std.ino());
}

#[test]
fn unlinkat_removes_regular_file() {
    let (_keep, root) = canonical_tempdir();
    let path = root.join("victim");
    std::fs::write(&path, b"data").expect("write");
    let dirfd = secure_open_dir(&root).expect("open root");

    unlinkat(dirfd.as_fd(), OsStr::new("victim"), UnlinkFlags::File).expect("unlinkat");
    assert!(!path.exists(), "leaf must be gone after unlinkat");
}

#[test]
fn unlinkat_removes_empty_dir_with_at_removedir() {
    let (_keep, root) = canonical_tempdir();
    let path = root.join("empty");
    std::fs::create_dir(&path).expect("mkdir");
    let dirfd = secure_open_dir(&root).expect("open root");

    unlinkat(dirfd.as_fd(), OsStr::new("empty"), UnlinkFlags::Dir).expect("unlinkat dir");
    assert!(
        !path.exists(),
        "empty directory must be gone after unlinkat"
    );
}

#[test]
fn unlinkat_returns_eperm_or_eisdir_on_dir_without_at_removedir() {
    // SEC-1.g invariant: removing a directory without `AT_REMOVEDIR`
    // must fail rather than silently succeed. Linux reports `EISDIR`,
    // BSDs and macOS report `EPERM` per the `unlink(2)` contract.
    let (_keep, root) = canonical_tempdir();
    let path = root.join("dir");
    std::fs::create_dir(&path).expect("mkdir");
    let dirfd = secure_open_dir(&root).expect("open root");

    let err = unlinkat(dirfd.as_fd(), OsStr::new("dir"), UnlinkFlags::File)
        .expect_err("must refuse to unlink a directory");
    let code = err.raw_os_error();
    assert!(
        code == Some(libc::EISDIR) || code == Some(libc::EPERM),
        "expected EISDIR or EPERM for unlink of a directory, got {code:?}"
    );
    assert!(path.exists(), "directory must survive a failed unlink");
}

#[test]
fn unlinkat_returns_enotempty_on_non_empty_dir_with_at_removedir() {
    // SEC-1.g invariant: `AT_REMOVEDIR` mirrors `rmdir(2)` exactly,
    // refusing to remove a non-empty directory.
    let (_keep, root) = canonical_tempdir();
    let dir = root.join("non-empty");
    std::fs::create_dir(&dir).expect("mkdir");
    std::fs::write(dir.join("inner"), b"x").expect("write inner");
    let dirfd = secure_open_dir(&root).expect("open root");

    let err = unlinkat(dirfd.as_fd(), OsStr::new("non-empty"), UnlinkFlags::Dir)
        .expect_err("must refuse to remove a non-empty directory");
    let code = err.raw_os_error();
    assert!(
        code == Some(libc::ENOTEMPTY) || code == Some(libc::EEXIST),
        "expected ENOTEMPTY or EEXIST for rmdir of non-empty directory, got {code:?}"
    );
    assert!(
        dir.exists(),
        "non-empty directory must survive a failed rmdir"
    );
}

#[test]
fn unlinkat_rejects_symlink_traversal() {
    // SEC-1 TOCTOU invariant: even when an attacker swaps the leaf
    // for a symlink to a sensitive sibling, `unlinkat(File)` removes
    // the symlink itself rather than the target it points at. The
    // syscall is hard-coded to never follow a terminal symlink, but
    // this test pins that contract against future regressions.
    let (_keep, root) = canonical_tempdir();
    // Sensitive target lives outside any path the receiver names.
    let sensitive = root.join("sensitive");
    std::fs::write(&sensitive, b"do-not-delete").expect("write sensitive");
    // The receiver decides to delete `leaf`; meanwhile the attacker
    // swaps it for a symlink pointing at `sensitive`.
    let leaf = root.join("leaf");
    std::os::unix::fs::symlink(&sensitive, &leaf).expect("symlink");

    let dirfd = secure_open_dir(&root).expect("open root");
    unlinkat(dirfd.as_fd(), OsStr::new("leaf"), UnlinkFlags::File).expect("unlinkat leaf");

    assert!(
        !leaf.exists(),
        "the symlink itself must be removed, target chase is forbidden"
    );
    assert!(
        sensitive.exists(),
        "unlinkat must never follow the terminal symlink; the target must survive"
    );
}

#[test]
fn unlinkat_reports_enoent_for_missing_leaf() {
    let (_keep, root) = canonical_tempdir();
    let dirfd = secure_open_dir(&root).expect("open root");

    let err = unlinkat(dirfd.as_fd(), OsStr::new("absent"), UnlinkFlags::File)
        .expect_err("missing leaf must error");
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
}

#[test]
fn unlink_via_sandbox_takes_at_path_for_single_component_file() {
    let (_keep, root) = canonical_tempdir();
    let path = root.join("file");
    std::fs::write(&path, b"x").expect("write");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let leaf = Path::new("file");
    unlink_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &path, UnlinkFlags::File)
        .expect("unlink");
    assert!(
        !path.exists(),
        "single-component file must be removed via sandbox dirfd"
    );
}

#[test]
fn unlink_via_sandbox_takes_at_path_for_single_component_dir() {
    let (_keep, root) = canonical_tempdir();
    let path = root.join("empty");
    std::fs::create_dir(&path).expect("mkdir");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let leaf = Path::new("empty");
    unlink_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &path, UnlinkFlags::Dir)
        .expect("rmdir");
    assert!(
        !path.exists(),
        "single-component dir must be removed via sandbox dirfd"
    );
}

#[test]
fn unlink_via_sandbox_removes_multi_component_end_to_end() {
    // A multi-component path anchors its parent under RESOLVE_BENEATH
    // where supported and falls back to std::fs::remove_file otherwise;
    // in both cases the leaf must be removed end-to-end.
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("sub")).expect("mkdir sub");
    let path = root.join("sub/file");
    std::fs::write(&path, b"x").expect("write");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let rel = Path::new("sub/file");
    unlink_via_sandbox_or_fallback(Some(&sandbox), &root, rel, &path, UnlinkFlags::File)
        .expect("unlink multi-component");
    assert!(
        !path.exists(),
        "multi-component leaf must be removed (anchored or fallback)"
    );
}

#[test]
fn unlink_via_sandbox_falls_back_when_sandbox_absent() {
    let (_keep, root) = canonical_tempdir();
    let path = root.join("file");
    std::fs::write(&path, b"x").expect("write");

    let leaf = Path::new("file");
    unlink_via_sandbox_or_fallback(None, &root, leaf, &path, UnlinkFlags::File)
        .expect("unlink fallback");
    assert!(
        !path.exists(),
        "absent-sandbox path must fall back to std::fs::remove_file"
    );
}

#[test]
fn unlink_via_sandbox_dispatches_rmdir_in_fallback() {
    // Without a sandbox the helper must still pick the correct std
    // call from `UnlinkFlags`: `remove_dir`, not `remove_file`.
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("sub")).expect("mkdir sub");
    let path = root.join("sub/inner");
    std::fs::create_dir(&path).expect("mkdir inner");

    let rel = Path::new("sub/inner");
    unlink_via_sandbox_or_fallback(None, &root, rel, &path, UnlinkFlags::Dir)
        .expect("rmdir fallback");
    assert!(
        !path.exists(),
        "Dir flag must dispatch std::fs::remove_dir on fallback"
    );
}

#[test]
fn fchmodat_sets_mode_on_regular_file() {
    let (_keep, root) = canonical_tempdir();
    let path = root.join("file");
    std::fs::write(&path, b"x").expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("seed perms");
    let dirfd = secure_open_dir(&root).expect("open root");

    fchmodat(dirfd.as_fd(), OsStr::new("file"), 0o640, true).expect("fchmodat");
    let meta = std::fs::metadata(&path).expect("stat");
    assert_eq!(meta.permissions().mode() & 0o777, 0o640);
}

#[test]
fn fchmodat_reports_enoent_for_missing_leaf() {
    let (_keep, root) = canonical_tempdir();
    let dirfd = secure_open_dir(&root).expect("open root");
    let err = fchmodat(dirfd.as_fd(), OsStr::new("absent"), 0o644, true)
        .expect_err("missing leaf must error");
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
}

#[test]
fn fchmodat_does_not_follow_symlink_under_nofollow() {
    // SEC-1.i invariant: with AT_SYMLINK_NOFOLLOW the chmod must
    // either no-op on the link itself (Linux: EOPNOTSUPP) or affect
    // only the link; the target's mode must not change.
    let (_keep, root) = canonical_tempdir();
    let target = root.join("target");
    std::fs::write(&target, b"x").expect("write target");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).expect("seed target");
    let link = root.join("link");
    symlink(&target, &link).expect("symlink");

    let dirfd = secure_open_dir(&root).expect("open root");
    // Some platforms reject AT_SYMLINK_NOFOLLOW chmod with EOPNOTSUPP;
    // either way the target's mode must survive.
    let _ = fchmodat(dirfd.as_fd(), OsStr::new("link"), 0o777, false);
    let target_meta = std::fs::metadata(&target).expect("stat target");
    assert_eq!(
        target_meta.permissions().mode() & 0o777,
        0o600,
        "AT_SYMLINK_NOFOLLOW must never chase the symlink to the target"
    );
}

#[test]
fn fchmodat_via_sandbox_takes_at_path_for_single_component() {
    let (_keep, root) = canonical_tempdir();
    let path = root.join("file");
    std::fs::write(&path, b"x").expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("seed perms");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let leaf = Path::new("file");
    fchmodat_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &path, 0o640, true)
        .expect("fchmodat");
    assert_eq!(
        std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777,
        0o640
    );
}

#[test]
fn fchmodat_via_sandbox_falls_back_for_multi_component() {
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("sub")).expect("mkdir sub");
    let path = root.join("sub/file");
    std::fs::write(&path, b"x").expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("seed perms");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let rel = Path::new("sub/file");
    fchmodat_via_sandbox_or_fallback(Some(&sandbox), &root, rel, &path, 0o640, true)
        .expect("fchmodat fallback");
    assert_eq!(
        std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777,
        0o640
    );
}

#[test]
fn fchmodat_via_sandbox_falls_back_when_sandbox_absent() {
    let (_keep, root) = canonical_tempdir();
    let path = root.join("file");
    std::fs::write(&path, b"x").expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("seed perms");

    let leaf = Path::new("file");
    fchmodat_via_sandbox_or_fallback(None, &root, leaf, &path, 0o644, true)
        .expect("fchmodat fallback");
    assert_eq!(
        std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777,
        0o644
    );
}

#[test]
fn secure_chmod_at_changes_mode_on_clean_path() {
    let (_keep, root) = canonical_tempdir();
    let path = root.join("file");
    std::fs::write(&path, b"x").expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("seed perms");

    super::secure_chmod_at(&path, 0o640, true).expect("secure chmod");
    assert_eq!(
        std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777,
        0o640
    );
}

#[test]
fn secure_chmod_at_refuses_symlinked_parent_leaf() {
    // chdir-symlink-race regression: a symlink swapped into the
    // immediate parent component of `path` must reject the chmod
    // rather than chase the link to an outside target. `O_NOFOLLOW`
    // on the parent `secure_open_dir` is enough to surface ELOOP on
    // every Unix target (Linux 5.6+ additionally rejects any
    // symlink anywhere in the parent path via openat2).
    let (_keep, root) = canonical_tempdir();
    let outside = root.join("outside");
    let module = root.join("module");
    std::fs::create_dir(&outside).expect("mkdir outside");
    std::fs::create_dir(&module).expect("mkdir module");
    let outside_target = outside.join("target");
    std::fs::write(&outside_target, b"OUTSIDE").expect("write outside");
    std::fs::set_permissions(&outside_target, std::fs::Permissions::from_mode(0o600))
        .expect("seed outside");
    // module/subdir -> outside (parent-component symlink trap).
    symlink(&outside, module.join("subdir")).expect("plant symlink");

    let dest = module.join("subdir").join("target");
    let err = super::secure_chmod_at(&dest, 0o666, true)
        .expect_err("chmod through symlinked parent must error");
    // Platform-dependent: Linux + openat2 surfaces ELOOP or EXDEV;
    // O_NOFOLLOW | O_DIRECTORY on a symlinked leaf surfaces ELOOP on
    // Linux without openat2 and ENOTDIR on macOS. All three confirm
    // the parent open was refused before any chmod issued.
    let raw = err.raw_os_error();
    assert!(
        matches!(
            raw,
            Some(libc::ELOOP) | Some(libc::EXDEV) | Some(libc::ENOTDIR)
        ),
        "expected ELOOP/EXDEV/ENOTDIR, got {raw:?}"
    );
    let outside_mode = std::fs::metadata(&outside_target)
        .expect("stat")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        outside_mode, 0o600,
        "outside file must keep 0o600 after refused chmod escape"
    );
}

#[test]
fn fchownat_no_change_when_uid_gid_are_neg1_sentinel() {
    // Passing the (-1, -1) sentinel must succeed and leave the
    // existing uid/gid unchanged. Exercising real reowning requires
    // CAP_CHOWN / root which CI workers do not have.
    let (_keep, root) = canonical_tempdir();
    let path = root.join("file");
    std::fs::write(&path, b"x").expect("write");
    let dirfd = secure_open_dir(&root).expect("open root");
    let before = std::fs::metadata(&path).expect("stat");

    fchownat(dirfd.as_fd(), OsStr::new("file"), u32::MAX, u32::MAX, true).expect("fchownat neg1");

    let after = std::fs::metadata(&path).expect("stat");
    assert_eq!(after.uid(), before.uid());
    assert_eq!(after.gid(), before.gid());
}

#[test]
fn fchownat_via_sandbox_takes_at_path_for_single_component() {
    let (_keep, root) = canonical_tempdir();
    let path = root.join("file");
    std::fs::write(&path, b"x").expect("write");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let leaf = Path::new("file");
    // (-1, -1) leaves uid/gid alone; the point of the assertion is
    // that the helper took the *at fast path without erroring.
    fchownat_via_sandbox_or_fallback(
        Some(&sandbox),
        &root,
        leaf,
        &path,
        u32::MAX,
        u32::MAX,
        false,
    )
    .expect("fchownat sandbox");
}

#[test]
fn fchownat_via_sandbox_falls_back_for_multi_component() {
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("sub")).expect("mkdir sub");
    let path = root.join("sub/file");
    std::fs::write(&path, b"x").expect("write");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let rel = Path::new("sub/file");
    fchownat_via_sandbox_or_fallback(Some(&sandbox), &root, rel, &path, u32::MAX, u32::MAX, false)
        .expect("fchownat fallback");
}

#[test]
fn fchownat_via_sandbox_falls_back_when_sandbox_absent() {
    let (_keep, root) = canonical_tempdir();
    let path = root.join("file");
    std::fs::write(&path, b"x").expect("write");

    let leaf = Path::new("file");
    fchownat_via_sandbox_or_fallback(None, &root, leaf, &path, u32::MAX, u32::MAX, false)
        .expect("fchownat fallback");
}

#[test]
fn utimensat_sets_atime_and_mtime() {
    let (_keep, root) = canonical_tempdir();
    let path = root.join("file");
    std::fs::write(&path, b"x").expect("write");
    let dirfd = secure_open_dir(&root).expect("open root");

    let atime = FileTime::from_unix_time(1_000_000, 0);
    let mtime = FileTime::from_unix_time(2_000_000, 0);
    utimensat(dirfd.as_fd(), OsStr::new("file"), atime, mtime, true).expect("utimensat");

    let meta = std::fs::metadata(&path).expect("stat");
    assert_eq!(FileTime::from_last_modification_time(&meta), mtime);
    assert_eq!(FileTime::from_last_access_time(&meta), atime);
}

#[test]
fn utimensat_reports_enoent_for_missing_leaf() {
    let (_keep, root) = canonical_tempdir();
    let dirfd = secure_open_dir(&root).expect("open root");
    let atime = FileTime::from_unix_time(1, 0);
    let mtime = FileTime::from_unix_time(2, 0);
    let err = utimensat(dirfd.as_fd(), OsStr::new("absent"), atime, mtime, true)
        .expect_err("missing leaf must error");
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
}

#[test]
fn utimensat_via_sandbox_takes_at_path_for_single_component() {
    let (_keep, root) = canonical_tempdir();
    let path = root.join("file");
    std::fs::write(&path, b"x").expect("write");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let atime = FileTime::from_unix_time(1_000_000, 0);
    let mtime = FileTime::from_unix_time(2_000_000, 0);
    let leaf = Path::new("file");
    utimensat_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &path, atime, mtime, true)
        .expect("utimensat sandbox");

    let meta = std::fs::metadata(&path).expect("stat");
    assert_eq!(FileTime::from_last_modification_time(&meta), mtime);
}

#[test]
fn utimensat_via_sandbox_falls_back_for_multi_component() {
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("sub")).expect("mkdir sub");
    let path = root.join("sub/file");
    std::fs::write(&path, b"x").expect("write");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let atime = FileTime::from_unix_time(3_000_000, 0);
    let mtime = FileTime::from_unix_time(4_000_000, 0);
    let rel = Path::new("sub/file");
    utimensat_via_sandbox_or_fallback(Some(&sandbox), &root, rel, &path, atime, mtime, true)
        .expect("utimensat fallback");

    let meta = std::fs::metadata(&path).expect("stat");
    assert_eq!(FileTime::from_last_modification_time(&meta), mtime);
}

#[test]
fn utimensat_via_sandbox_falls_back_when_sandbox_absent() {
    let (_keep, root) = canonical_tempdir();
    let path = root.join("file");
    std::fs::write(&path, b"x").expect("write");

    let atime = FileTime::from_unix_time(5_000_000, 0);
    let mtime = FileTime::from_unix_time(6_000_000, 0);
    let leaf = Path::new("file");
    utimensat_via_sandbox_or_fallback(None, &root, leaf, &path, atime, mtime, true)
        .expect("utimensat fallback");

    let meta = std::fs::metadata(&path).expect("stat");
    assert_eq!(FileTime::from_last_modification_time(&meta), mtime);
}

#[test]
fn utimensat_via_sandbox_symlink_no_follow_preserves_target_mtime() {
    // SEC-1.i invariant: with `follow_symlinks = false` the helper
    // must affect the symlink itself, not the target it points at.
    let (_keep, root) = canonical_tempdir();
    let target = root.join("target");
    std::fs::write(&target, b"x").expect("write target");
    let initial_target_mtime = FileTime::from_unix_time(100, 0);
    filetime::set_file_times(&target, initial_target_mtime, initial_target_mtime)
        .expect("seed target mtime");
    let link = root.join("link");
    symlink(&target, &link).expect("symlink");

    let sandbox = DirSandbox::open_root(&root).expect("sandbox");
    let new_atime = FileTime::from_unix_time(9_000_000, 0);
    let new_mtime = FileTime::from_unix_time(9_500_000, 0);
    utimensat_via_sandbox_or_fallback(
        Some(&sandbox),
        &root,
        Path::new("link"),
        &link,
        new_atime,
        new_mtime,
        false,
    )
    .expect("utimensat lutimes");

    let target_meta = std::fs::metadata(&target).expect("stat target");
    assert_eq!(
        FileTime::from_last_modification_time(&target_meta),
        initial_target_mtime,
        "AT_SYMLINK_NOFOLLOW must never chase the symlink to the target"
    );
}

#[test]
fn renameat_renames_regular_file_in_same_dir() {
    let (_keep, root) = canonical_tempdir();
    let src = root.join("src");
    let dst = root.join("dst");
    std::fs::write(&src, b"payload").expect("write src");

    let dirfd = secure_open_dir(&root).expect("open root");
    renameat(
        dirfd.as_fd(),
        OsStr::new("src"),
        dirfd.as_fd(),
        OsStr::new("dst"),
        true,
    )
    .expect("renameat");

    assert!(!src.exists(), "source must be gone after renameat");
    assert_eq!(std::fs::read(&dst).expect("read dst"), b"payload");
}

#[test]
fn renameat_reports_enoent_for_missing_source() {
    let (_keep, root) = canonical_tempdir();
    let dirfd = secure_open_dir(&root).expect("open root");
    let err = renameat(
        dirfd.as_fd(),
        OsStr::new("absent"),
        dirfd.as_fd(),
        OsStr::new("target"),
        true,
    )
    .expect_err("missing source must error");
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
}

#[test]
fn renameat_at_fdcwd_interop() {
    // AT_FDCWD passed via BorrowedFd::borrow_raw must behave like a
    // path-based rename(2). Sanity check: rename inside a tempdir
    // referenced by relative path with the process cwd set there.
    let (_keep, root) = canonical_tempdir();
    let original_cwd = std::env::current_dir().expect("getcwd");
    std::env::set_current_dir(&root).expect("chdir");
    let src_relative = OsStr::new("at_fdcwd_src");
    let dst_relative = OsStr::new("at_fdcwd_dst");
    std::fs::write(root.join("at_fdcwd_src"), b"x").expect("write src");

    // SAFETY: AT_FDCWD is a kernel-defined sentinel, not a real fd,
    // but `BorrowedFd::borrow_raw` accepts negative ints for exactly
    // this use case.
    #[allow(unsafe_code)]
    let cwd_fd = unsafe { BorrowedFd::borrow_raw(libc::AT_FDCWD) };
    let result = renameat(cwd_fd, src_relative, cwd_fd, dst_relative, true);
    // Restore cwd before any assertion so a failure does not leave
    // the test binary in the tempdir.
    std::env::set_current_dir(&original_cwd).expect("restore cwd");
    result.expect("renameat AT_FDCWD");

    assert!(!root.join("at_fdcwd_src").exists());
    assert_eq!(
        std::fs::read(root.join("at_fdcwd_dst")).expect("read"),
        b"x"
    );
}

#[test]
fn renameat_across_two_distinct_dirfds() {
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("from")).expect("mkdir from");
    std::fs::create_dir(root.join("to")).expect("mkdir to");
    std::fs::write(root.join("from").join("file"), b"cross").expect("write");

    let from_fd = secure_open_dir(&root.join("from")).expect("open from");
    let to_fd = secure_open_dir(&root.join("to")).expect("open to");
    renameat(
        from_fd.as_fd(),
        OsStr::new("file"),
        to_fd.as_fd(),
        OsStr::new("file"),
        true,
    )
    .expect("renameat cross-dir");

    assert!(!root.join("from").join("file").exists());
    assert_eq!(
        std::fs::read(root.join("to").join("file")).expect("read dst"),
        b"cross"
    );
}

#[test]
fn renameat_via_sandbox_takes_at_path_for_single_component() {
    let (_keep, root) = canonical_tempdir();
    let src = root.join("src");
    let dst = root.join("dst");
    std::fs::write(&src, b"sandboxed").expect("write src");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let src_rel = Path::new("src");
    let dst_rel = Path::new("dst");
    renameat_via_sandbox_or_fallback(
        Some(&sandbox),
        &root,
        src_rel,
        &src,
        &root,
        dst_rel,
        &dst,
        true,
    )
    .expect("renameat sandbox");

    assert!(!src.exists());
    assert_eq!(std::fs::read(&dst).expect("read dst"), b"sandboxed");
}

#[test]
fn renameat_via_sandbox_falls_back_for_multi_component_source() {
    // A multi-component source paired with a single-component dest must
    // take the path-based fallback: the nested anchor only engages when
    // BOTH endpoints resolve their parents, never mixing an anchored and
    // an ambient endpoint. The dest here is single-component, so the
    // helper drops to std::fs::rename regardless of openat2 support.
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("sub")).expect("mkdir sub");
    let src = root.join("sub").join("src");
    let dst = root.join("dst");
    std::fs::write(&src, b"fallback-src").expect("write src");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    renameat_via_sandbox_or_fallback(
        Some(&sandbox),
        &root,
        Path::new("sub/src"),
        &src,
        &root,
        Path::new("dst"),
        &dst,
        true,
    )
    .expect("renameat fallback");

    assert!(!src.exists());
    assert_eq!(std::fs::read(&dst).expect("read dst"), b"fallback-src");
}

#[test]
fn renameat_via_sandbox_falls_back_when_sandbox_absent() {
    let (_keep, root) = canonical_tempdir();
    let src = root.join("src");
    let dst = root.join("dst");
    std::fs::write(&src, b"no-sandbox").expect("write src");

    renameat_via_sandbox_or_fallback(
        None,
        &root,
        Path::new("src"),
        &src,
        &root,
        Path::new("dst"),
        &dst,
        true,
    )
    .expect("renameat no-sandbox");

    assert!(!src.exists());
    assert_eq!(std::fs::read(&dst).expect("read dst"), b"no-sandbox");
}

#[test]
fn renameat_via_sandbox_falls_back_when_paths_cross_sandbox_boundary() {
    // When `dest_dir` does not match the `link_path`'s actual parent
    // the single-component-leaf check fails and the helper falls
    // back to std::fs::rename. This is the safety net for callers
    // that pass a mismatched (dest_dir, link_path) pair.
    let (_keep, root) = canonical_tempdir();
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir(&elsewhere).expect("mkdir elsewhere");
    let src = elsewhere.join("src");
    let dst = root.join("dst");
    std::fs::write(&src, b"cross-boundary").expect("write src");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    // dest_dir = root, relative_path = "src", but link_path = root/elsewhere/src.
    // The helper rejects the leaf shortcut and falls through.
    renameat_via_sandbox_or_fallback(
        Some(&sandbox),
        &root,
        Path::new("src"),
        &src,
        &root,
        Path::new("dst"),
        &dst,
        true,
    )
    .expect("renameat cross-boundary fallback");

    assert!(!src.exists());
    assert_eq!(std::fs::read(&dst).expect("read dst"), b"cross-boundary");
}

#[test]
fn renameat_overwrites_existing_destination_by_default() {
    let (_keep, root) = canonical_tempdir();
    let src = root.join("src");
    let dst = root.join("dst");
    std::fs::write(&src, b"new").expect("write src");
    std::fs::write(&dst, b"old").expect("write dst");
    let dirfd = secure_open_dir(&root).expect("open root");

    renameat(
        dirfd.as_fd(),
        OsStr::new("src"),
        dirfd.as_fd(),
        OsStr::new("dst"),
        true,
    )
    .expect("renameat overwrite");

    assert!(!src.exists());
    assert_eq!(std::fs::read(&dst).expect("read dst"), b"new");
}

#[test]
#[cfg(target_os = "linux")]
fn renameat_noreplace_refuses_existing_destination_on_linux() {
    // On Linux 3.15+ RENAME_NOREPLACE returns EEXIST when the
    // destination is present. Older kernels return ENOSYS / EINVAL
    // and the helper falls back to overwriting; on that path the
    // assertion below would fail, so this test is Linux-only and
    // tolerates the fallback path by accepting overwrite as well.
    let (_keep, root) = canonical_tempdir();
    let src = root.join("src");
    let dst = root.join("dst");
    std::fs::write(&src, b"new").expect("write src");
    std::fs::write(&dst, b"old").expect("write dst");
    let dirfd = secure_open_dir(&root).expect("open root");

    match renameat(
        dirfd.as_fd(),
        OsStr::new("src"),
        dirfd.as_fd(),
        OsStr::new("dst"),
        false,
    ) {
        Err(err) if err.raw_os_error() == Some(libc::EEXIST) => {
            assert!(src.exists(), "src must remain after EEXIST");
            assert_eq!(std::fs::read(&dst).expect("read dst"), b"old");
        }
        Ok(()) => {
            // Pre-3.15 kernel or filesystem without RENAME_NOREPLACE
            // support: helper transparently overwrote.
            assert!(!src.exists());
            assert_eq!(std::fs::read(&dst).expect("read dst"), b"new");
        }
        Err(err) => panic!("unexpected error: {err}"),
    }
}

#[test]
fn renameat_via_sandbox_succeeds_with_sandbox_secondary_dirs() {
    // Confirm the sandbox path works when the same dest_dir is used
    // for both endpoints (the common receiver case where temp file
    // and final file share a parent).
    let (_keep, root) = canonical_tempdir();
    let temp = root.join(".final.XXXXXX");
    let final_path = root.join("final");
    std::fs::write(&temp, b"committed").expect("write temp");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    renameat_via_sandbox_or_fallback(
        Some(&sandbox),
        &root,
        Path::new(".final.XXXXXX"),
        &temp,
        &root,
        Path::new("final"),
        &final_path,
        true,
    )
    .expect("renameat sandbox temp commit");

    assert!(!temp.exists());
    assert_eq!(std::fs::read(&final_path).expect("read"), b"committed");
}

#[test]
fn openat_raw_returns_file_for_existing_path() {
    let (_keep, root) = canonical_tempdir();
    let path = root.join("file");
    std::fs::write(&path, b"hello").expect("write");
    let dirfd = secure_open_dir(&root).expect("open root");

    let mut file =
        openat(dirfd.as_fd(), OsStr::new("file"), libc::O_RDONLY, 0).expect("openat existing");
    use std::io::Read;
    let mut buf = String::new();
    file.read_to_string(&mut buf).expect("read");
    assert_eq!(buf, "hello");
}

#[test]
fn openat_raw_returns_enoent_for_missing_name() {
    let (_keep, root) = canonical_tempdir();
    let dirfd = secure_open_dir(&root).expect("open root");

    let err = openat(dirfd.as_fd(), OsStr::new("absent"), libc::O_RDONLY, 0)
        .expect_err("missing leaf must error");
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
}

#[test]
fn openat_via_sandbox_fast_path_succeeds_on_leaf() {
    let (_keep, root) = canonical_tempdir();
    let path = root.join("created");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let leaf = Path::new("created");
    let file = openat_via_sandbox_or_fallback(
        Some(&sandbox),
        &root,
        leaf,
        &path,
        libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW,
        0o600,
    )
    .expect("openat sandbox create");
    drop(file);

    assert!(path.exists(), "single-component leaf must be created");
    let meta = std::fs::metadata(&path).expect("stat");
    // The kernel applies the active umask to the requested mode, so
    // the exact bits depend on the test environment. Asserting that
    // no group/other write bits leaked beyond the umask-filtered
    // request is enough to confirm the mode argument was honoured.
    let mode = meta.permissions().mode() & 0o777;
    assert!(
        mode & 0o066 == 0,
        "mode 0o600 must not grant group/other access, got {mode:o}"
    );
}

/// Positive control for the receiver's hardened destination open
/// (`temp_guard.rs:236`, flags `O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW`).
/// The symlink-race guard (`O_NOFOLLOW`) must not break a normal upload:
/// a non-malicious leaf is created, written through the returned fd, and
/// the payload must land intact at the real path as a regular file (never
/// a symlink). This complements the negative
/// `sandbox_anchored_guard_resists_symlink_swap_on_parent` and the
/// upstream `bare-do-open-symlink-race.test` (which assert only the
/// rejection side). Mirrors upstream `syscall.c:do_open_at` line 750,
/// where `O_NOFOLLOW` is a no-op on a real leaf and the create succeeds.
#[test]
fn openat_via_sandbox_nofollow_create_lands_payload_at_real_path() {
    use std::io::Write;

    let (_keep, root) = canonical_tempdir();
    let path = root.join("upload.bin");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let leaf = Path::new("upload.bin");
    let mut file = openat_via_sandbox_or_fallback(
        Some(&sandbox),
        &root,
        leaf,
        &path,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
        0o600,
    )
    .expect("hardened create must succeed for a normal (non-symlink) leaf");

    let payload = b"hardened upload payload";
    file.write_all(payload).expect("write through hardened fd");
    file.flush().expect("flush");
    drop(file);

    // The bytes must land at the real path - not redirected through a
    // symlink, and not silently dropped by the O_NOFOLLOW guard.
    let meta = std::fs::symlink_metadata(&path).expect("stat real path");
    assert!(
        meta.file_type().is_file(),
        "hardened create must land a regular file, got {:?}",
        meta.file_type()
    );
    assert!(
        !meta.file_type().is_symlink(),
        "the real path must never be a symlink after a hardened create"
    );
    assert_eq!(
        std::fs::read(&path).expect("read back real path"),
        payload,
        "payload written through the hardened fd must round-trip intact"
    );
}

#[test]
fn openat_via_sandbox_fallback_for_multi_component() {
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("sub")).expect("mkdir sub");
    let path = root.join("sub").join("created");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let rel = Path::new("sub/created");
    let file = openat_via_sandbox_or_fallback(
        Some(&sandbox),
        &root,
        rel,
        &path,
        libc::O_WRONLY | libc::O_CREAT,
        0o644,
    )
    .expect("openat fallback create");
    drop(file);

    assert!(
        path.exists(),
        "multi-component path must fall back to std OpenOptions"
    );
}

#[test]
fn openat_via_sandbox_or_fallback_with_no_sandbox() {
    let (_keep, root) = canonical_tempdir();
    let path = root.join("file");
    std::fs::write(&path, b"present").expect("write");

    let leaf = Path::new("file");
    let mut file = openat_via_sandbox_or_fallback(None, &root, leaf, &path, libc::O_RDONLY, 0)
        .expect("openat no-sandbox fallback");
    use std::io::Read;
    let mut buf = String::new();
    file.read_to_string(&mut buf).expect("read");
    assert_eq!(buf, "present");
}

#[test]
fn readlinkat_returns_target_for_symlink() {
    let (_keep, root) = canonical_tempdir();
    let target = root.join("target");
    std::fs::write(&target, b"x").expect("write target");
    let link = root.join("link");
    symlink(&target, &link).expect("symlink");

    let dirfd = secure_open_dir(&root).expect("open root");
    let got = readlinkat(dirfd.as_fd(), OsStr::new("link")).expect("readlinkat");
    assert_eq!(got, target);
}

#[test]
fn readlinkat_returns_einval_for_non_symlink() {
    let (_keep, root) = canonical_tempdir();
    std::fs::write(root.join("file"), b"x").expect("write");
    let dirfd = secure_open_dir(&root).expect("open root");

    let err = readlinkat(dirfd.as_fd(), OsStr::new("file")).expect_err("non-symlink must error");
    assert_eq!(err.raw_os_error(), Some(libc::EINVAL));
}

#[test]
fn readlinkat_via_sandbox_returns_target_for_symlink() {
    let (_keep, root) = canonical_tempdir();
    let target = root.join("target");
    std::fs::write(&target, b"x").expect("write target");
    let link = root.join("link");
    symlink(&target, &link).expect("symlink");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let leaf = Path::new("link");
    let got = readlinkat_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &link)
        .expect("readlinkat sandbox");
    assert_eq!(got, target);
}

#[test]
fn readlinkat_via_sandbox_returns_einval_for_non_symlink() {
    let (_keep, root) = canonical_tempdir();
    let path = root.join("file");
    std::fs::write(&path, b"x").expect("write");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let leaf = Path::new("file");
    let err = readlinkat_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &path)
        .expect_err("non-symlink must error");
    assert_eq!(err.raw_os_error(), Some(libc::EINVAL));
}

#[test]
fn readlinkat_via_sandbox_falls_back_for_multi_component() {
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("sub")).expect("mkdir sub");
    let target = root.join("sub").join("target");
    std::fs::write(&target, b"x").expect("write target");
    let link = root.join("sub").join("link");
    symlink(&target, &link).expect("symlink");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let rel = Path::new("sub/link");
    let got = readlinkat_via_sandbox_or_fallback(Some(&sandbox), &root, rel, &link)
        .expect("readlinkat fallback");
    assert_eq!(got, target);
}

// recursive_unlinkat_via_sandbox_or_fallback tests (SEC-1.s)

fn build_three_deep_tree(root: &Path, leaf: &str) {
    let l1 = root.join(leaf);
    let l2 = l1.join("b");
    let l3 = l2.join("c");
    std::fs::create_dir_all(&l3).expect("mkdir -p");
    std::fs::write(l1.join("sibling-file"), b"sibling").expect("sibling file");
    std::fs::write(l2.join("mid-file"), b"mid").expect("mid file");
    std::fs::write(l3.join("file"), b"leaf-bytes").expect("leaf file");
    symlink(Path::new("../mid-file"), l3.join("symlink-to-mid")).expect("symlink in leaf");
}

#[test]
fn recursive_unlinkat_removes_three_deep_tree() {
    let (_keep, root) = canonical_tempdir();
    build_three_deep_tree(&root, "tree");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let leaf = Path::new("tree");
    let target = root.join(leaf);
    recursive_unlinkat_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &target)
        .expect("recursive unlinkat");

    assert!(!target.exists(), "tree must be gone");
    let remaining: Vec<_> = std::fs::read_dir(&root)
        .expect("read root")
        .map(|e| e.expect("dirent").file_name())
        .collect();
    assert!(remaining.is_empty(), "root must be empty: {remaining:?}");
}

#[test]
fn recursive_unlinkat_treats_missing_root_as_success() {
    let (_keep, root) = canonical_tempdir();
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let leaf = Path::new("does-not-exist");
    let target = root.join(leaf);
    recursive_unlinkat_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &target)
        .expect("missing leaf must be idempotent success");
}

#[test]
fn recursive_unlinkat_refuses_to_follow_symlink_at_descent_root() {
    // SEC-1.s core invariant: a symlink at the descent root must
    // never be dereferenced; the helper must refuse with ELOOP and
    // leave the symlink target intact.
    let (_keep, root) = canonical_tempdir();
    let outside = root.join("outside");
    std::fs::create_dir(&outside).expect("mkdir outside");
    let sentinel = outside.join("sentinel");
    std::fs::write(&sentinel, b"do-not-touch").expect("sentinel");

    let link = root.join("link-to-outside");
    symlink(&outside, &link).expect("symlink outside");

    let sandbox = DirSandbox::open_root(&root).expect("sandbox");
    let leaf = Path::new("link-to-outside");
    let err = recursive_unlinkat_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &link)
        .expect_err("symlink at root must be refused");
    // Linux returns ENOTDIR when O_DIRECTORY + O_NOFOLLOW races a symlink
    // (the kernel checks the symlink-not-a-directory class before the
    // O_NOFOLLOW refusal), while POSIX-strict implementations return
    // ELOOP. Either is acceptable: neither follows the symlink.
    let errno = err.raw_os_error();
    assert!(
        errno == Some(libc::ELOOP) || errno == Some(libc::ENOTDIR),
        "expected ELOOP or ENOTDIR (symlink not followed), got {err:?}"
    );
    assert!(sentinel.exists(), "sentinel must be intact");
    assert!(outside.exists(), "outside dir must be intact");
}

#[test]
fn recursive_unlinkat_unlinks_symlinks_inside_tree_without_following() {
    // Symlinks beneath the descent root must be unlinked as files
    // (their inode goes away) without the helper following them
    // into the link target. We assert the target survives.
    let (_keep, root) = canonical_tempdir();
    let outside = root.join("outside");
    std::fs::create_dir(&outside).expect("mkdir outside");
    let sentinel = outside.join("sentinel");
    std::fs::write(&sentinel, b"do-not-touch").expect("sentinel");

    let tree = root.join("tree");
    std::fs::create_dir(&tree).expect("mkdir tree");
    symlink(&outside, tree.join("escape")).expect("symlink escape");
    std::fs::write(tree.join("file"), b"x").expect("file");

    let sandbox = DirSandbox::open_root(&root).expect("sandbox");
    let leaf = Path::new("tree");
    recursive_unlinkat_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &tree)
        .expect("recursive unlinkat");
    assert!(!tree.exists(), "tree must be gone");
    assert!(
        sentinel.exists(),
        "escape symlink must not have been followed"
    );
    assert!(outside.exists(), "outside directory must still exist");
}

#[test]
fn recursive_unlinkat_fallback_matches_std_remove_dir_all() {
    let (_keep, root) = canonical_tempdir();
    build_three_deep_tree(&root, "sandbox_tree");
    build_three_deep_tree(&root, "control_tree");

    // Fallback path: pass `None` for the sandbox so the helper
    // delegates to `std::fs::remove_dir_all`.
    let sandbox_target = root.join("sandbox_tree");
    recursive_unlinkat_via_sandbox_or_fallback(
        None,
        &root,
        Path::new("sandbox_tree"),
        &sandbox_target,
    )
    .expect("fallback remove");

    // Control: directly call std::fs::remove_dir_all.
    let control_target = root.join("control_tree");
    std::fs::remove_dir_all(&control_target).expect("std remove");

    assert!(!sandbox_target.exists());
    assert!(!control_target.exists());
}

#[test]
fn recursive_unlinkat_fallback_treats_missing_root_as_success() {
    // Fallback path mirrors the sandbox path's idempotent-ENOENT
    // policy so callers can rely on a single error contract
    // regardless of which path is taken.
    let (_keep, root) = canonical_tempdir();
    let leaf = Path::new("does-not-exist");
    let target = root.join(leaf);
    recursive_unlinkat_via_sandbox_or_fallback(None, &root, leaf, &target)
        .expect("fallback must absorb ENOENT on root");
}

#[test]
fn recursive_unlinkat_removes_multi_component_relative_end_to_end() {
    // Multi-component relative paths anchor their parent under
    // RESOLVE_BENEATH where supported and fall back to the path-based
    // walk otherwise; either way the helper must remove the subtree
    // end-to-end while leaving the parent directory intact.
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("outer")).expect("mkdir outer");
    let inner = root.join("outer").join("inner");
    std::fs::create_dir(&inner).expect("mkdir inner");
    std::fs::write(inner.join("file"), b"x").expect("write");

    let sandbox = DirSandbox::open_root(&root).expect("sandbox");
    let rel = Path::new("outer/inner");
    recursive_unlinkat_via_sandbox_or_fallback(Some(&sandbox), &root, rel, &inner)
        .expect("multi-component fallback");

    assert!(!inner.exists(), "inner tree must be gone");
    assert!(root.join("outer").exists(), "outer must remain");
}

#[test]
fn recursive_unlinkat_propagates_enotdir_for_non_directory_leaf() {
    // A non-directory at the descent root surfaces ENOTDIR
    // verbatim from openat(O_DIRECTORY).
    let (_keep, root) = canonical_tempdir();
    let path = root.join("not-a-dir");
    std::fs::write(&path, b"hello").expect("write file");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let leaf = Path::new("not-a-dir");
    let err = recursive_unlinkat_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &path)
        .expect_err("non-directory leaf must error");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOTDIR),
        "expected ENOTDIR, got {err:?}"
    );
    // The non-directory entry must survive the failed call.
    assert!(path.exists(), "non-dir leaf must be untouched on ENOTDIR");
}

#[test]
fn recursive_unlinkat_handles_wide_directory() {
    // Exercises the `read_dir_entries` collect loop with enough
    // entries that the `readdir(3)` walk wraps several internal
    // buffer-fill rounds.
    let (_keep, root) = canonical_tempdir();
    let tree = root.join("wide");
    std::fs::create_dir(&tree).expect("mkdir wide");
    for i in 0..256 {
        std::fs::write(tree.join(format!("file-{i:03}")), b"x").expect("write");
    }
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");
    let leaf = Path::new("wide");
    recursive_unlinkat_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &tree)
        .expect("recursive unlinkat wide");
    assert!(!tree.exists());
}

#[test]
fn recursive_unlinkat_via_sandbox_or_fallback_with_no_sandbox_removes_tree() {
    let (_keep, root) = canonical_tempdir();
    build_three_deep_tree(&root, "tree");
    let leaf = Path::new("tree");
    let target = root.join(leaf);
    recursive_unlinkat_via_sandbox_or_fallback(None, &root, leaf, &target)
        .expect("no-sandbox fallback");
    assert!(!target.exists());
}

/// `true` when the test runs as root, in which case every permission
/// bit is bypassed and the `DEL_NO_UID_WRITE` chmod-before-delete fix
/// below is both meaningless and untestable.
fn running_as_root() -> bool {
    // SAFETY: `geteuid(2)` is a pure accessor with no arguments and no
    // failure mode.
    #[allow(unsafe_code)]
    let euid = unsafe { libc::geteuid() };
    euid == 0
}

// DEL_NO_UID_WRITE chmod-before-delete tests (upstream delete.c:100-101 /
// 141-142): a directory we own but cannot write to must still be emptied
// and removed under --delete rather than failing with EACCES.

#[test]
fn recursive_unlinkat_removes_owned_read_only_directory() {
    if running_as_root() {
        return;
    }

    let (_keep, root) = canonical_tempdir();
    let tree = root.join("readonly");
    std::fs::create_dir(&tree).expect("mkdir");
    std::fs::write(tree.join("extraneous"), b"x").expect("write");
    std::fs::set_permissions(&tree, std::fs::Permissions::from_mode(0o555)).expect("chmod 0555");

    let sandbox = DirSandbox::open_root(&root).expect("sandbox");
    let leaf = Path::new("readonly");
    let result = recursive_unlinkat_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &tree);
    // Defensive restore so a failed assertion below still leaves the
    // tempdir removable by the `TempDir` drop.
    let _ = std::fs::set_permissions(&tree, std::fs::Permissions::from_mode(0o755));

    result.expect("a read-only owned directory must be removable, matching upstream");
    assert!(!tree.exists(), "read-only directory must be gone");
}

#[test]
fn recursive_unlinkat_removes_nested_owned_read_only_directory() {
    // Exercises the recursive (non-top-level) call site: upstream's
    // delete_dir_contents() chmods a doomed child before descending into
    // it, not just the top-level delete_item() candidate.
    if running_as_root() {
        return;
    }

    let (_keep, root) = canonical_tempdir();
    let tree = root.join("tree");
    let nested = tree.join("readonly-child");
    std::fs::create_dir_all(&nested).expect("mkdir -p");
    std::fs::write(nested.join("extraneous"), b"x").expect("write");
    std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o555)).expect("chmod 0555");

    let sandbox = DirSandbox::open_root(&root).expect("sandbox");
    let leaf = Path::new("tree");
    let result = recursive_unlinkat_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &tree);
    let _ = std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o755));

    result.expect("a nested owned read-only directory must not block removal");
    assert!(!tree.exists());
}

#[test]
fn recursive_unlinkat_fallback_removes_owned_read_only_directory() {
    // Same fix exercised through the no-sandbox std::fs::remove_dir_all
    // retry path (recursive_unlinkat_via_sandbox_or_fallback's fallback
    // arm).
    if running_as_root() {
        return;
    }

    let (_keep, root) = canonical_tempdir();
    let tree = root.join("readonly");
    std::fs::create_dir(&tree).expect("mkdir");
    std::fs::write(tree.join("extraneous"), b"x").expect("write");
    std::fs::set_permissions(&tree, std::fs::Permissions::from_mode(0o555)).expect("chmod 0555");

    let result =
        recursive_unlinkat_via_sandbox_or_fallback(None, &root, Path::new("readonly"), &tree);
    let _ = std::fs::set_permissions(&tree, std::fs::Permissions::from_mode(0o755));

    result.expect("the no-sandbox fallback must also honor DEL_NO_UID_WRITE");
    assert!(!tree.exists());
}

// read_dir_via_sandbox_or_fallback tests (SEC-1.q2)

fn collect_names(outcome: ReadDirOutcome) -> Vec<std::ffi::OsString> {
    outcome
        .map(|res| res.expect("dir entry").into_file_name())
        .collect()
}

#[test]
fn read_dir_via_sandbox_lists_root_when_relative_is_empty() {
    let (_keep, root) = canonical_tempdir();
    std::fs::write(root.join("a"), b"x").expect("write a");
    std::fs::create_dir(root.join("b")).expect("mkdir b");
    symlink(root.join("a"), root.join("c")).expect("symlink c");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let outcome = read_dir_via_sandbox_or_fallback(Some(&sandbox), &root, Path::new(""), &root)
        .expect("read_dir");
    assert!(matches!(outcome, ReadDirOutcome::At(_)));

    let mut names: Vec<_> = collect_names(outcome)
        .into_iter()
        .map(|n| n.to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["a", "b", "c"]);
}

#[test]
fn read_dir_via_sandbox_lists_single_component_subdir() {
    let (_keep, root) = canonical_tempdir();
    let sub = root.join("sub");
    std::fs::create_dir(&sub).expect("mkdir sub");
    std::fs::write(sub.join("file"), b"x").expect("write file");
    std::fs::create_dir(sub.join("nested")).expect("mkdir nested");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let outcome = read_dir_via_sandbox_or_fallback(Some(&sandbox), &root, Path::new("sub"), &sub)
        .expect("read_dir");
    assert!(matches!(outcome, ReadDirOutcome::At(_)));

    let entries: Vec<_> = outcome.map(|res| res.expect("entry")).collect();
    assert_eq!(entries.len(), 2);
    let mut by_name: std::collections::HashMap<_, _> = entries
        .into_iter()
        .map(|e| (e.file_name().to_os_string(), e.file_type()))
        .collect();
    let file_kind = by_name.remove(OsStr::new("file")).expect("file present");
    let nested_kind = by_name
        .remove(OsStr::new("nested"))
        .expect("nested present");
    assert_eq!(file_kind, Some(EntryKind::Other));
    assert_eq!(nested_kind, Some(EntryKind::Dir));
}

#[test]
fn read_dir_via_sandbox_falls_back_for_multi_component() {
    let (_keep, root) = canonical_tempdir();
    let nested = root.join("a/b");
    std::fs::create_dir_all(&nested).expect("mkdir -p");
    std::fs::write(nested.join("leaf"), b"x").expect("write leaf");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let outcome =
        read_dir_via_sandbox_or_fallback(Some(&sandbox), &root, Path::new("a/b"), &nested)
            .expect("read_dir");
    assert!(
        matches!(outcome, ReadDirOutcome::Std(_)),
        "multi-component path must take the path-based fallback"
    );
    let names = collect_names(outcome);
    assert_eq!(names.len(), 1);
    assert_eq!(names[0], OsStr::new("leaf"));
}

#[test]
fn read_dir_via_sandbox_falls_back_when_sandbox_absent() {
    let (_keep, root) = canonical_tempdir();
    std::fs::write(root.join("file"), b"x").expect("write");

    let outcome =
        read_dir_via_sandbox_or_fallback(None, &root, Path::new(""), &root).expect("read_dir");
    assert!(matches!(outcome, ReadDirOutcome::Std(_)));
    let names = collect_names(outcome);
    assert_eq!(names.len(), 1);
    assert_eq!(names[0], OsStr::new("file"));
}

#[test]
fn read_dir_via_sandbox_refuses_symlink_at_leaf() {
    // SEC-1.q2 core invariant: when an attacker swaps a subdir for a
    // symlink to an outside directory between the receiver's
    // decide-to-list moment and the syscall, the sandbox-anchored
    // helper must refuse with ELOOP/ENOTDIR rather than redirect the
    // listing to the attacker-chosen tree.
    let (_keep, root) = canonical_tempdir();
    let outside = root.join("outside");
    std::fs::create_dir(&outside).expect("mkdir outside");
    std::fs::write(outside.join("sentinel"), b"do-not-touch").expect("sentinel");
    let link = root.join("link");
    symlink(&outside, &link).expect("symlink");

    let sandbox = DirSandbox::open_root(&root).expect("sandbox");
    let err = read_dir_via_sandbox_or_fallback(Some(&sandbox), &root, Path::new("link"), &link)
        .expect_err("symlink leaf must be refused");
    let errno = err.raw_os_error();
    assert!(
        errno == Some(libc::ELOOP) || errno == Some(libc::ENOTDIR),
        "expected ELOOP or ENOTDIR (symlink not followed), got {err:?}"
    );
    assert!(outside.exists(), "symlink target outside must survive");
}

#[test]
fn read_dir_view_via_sandbox_matches_std_for_subdir_listing() {
    let (_keep, root) = canonical_tempdir();
    let sub = root.join("sub");
    std::fs::create_dir(&sub).expect("mkdir sub");
    std::fs::write(sub.join("a"), b"a").expect("write a");
    std::fs::write(sub.join("b"), b"b").expect("write b");
    std::fs::create_dir(sub.join("c")).expect("mkdir c");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let via_at = read_dir_via_sandbox_or_fallback(Some(&sandbox), &root, Path::new("sub"), &sub)
        .expect("via at");
    let mut at_names = collect_names(via_at);
    at_names.sort();

    let via_std =
        read_dir_via_sandbox_or_fallback(None, &root, Path::new("sub"), &sub).expect("via std");
    let mut std_names = collect_names(via_std);
    std_names.sort();

    assert_eq!(at_names, std_names, "sandbox and std listings must agree");
}

// SEC nested-path parent anchoring (RESOLVE_BENEATH) tests.
//
// These cover the interior-directory TOCTOU gap the single-component
// fast path could not close: for `dest/a/b/leaf` the leaf op must be
// confined to a parent resolved beneath the sandbox root, so a swapped
// interior symlink (`a/b -> outside`) cannot redirect the op. On Linux
// 5.6+ the anchor refuses the escape in-kernel (EXDEV/ELOOP/ENOTDIR);
// on kernels/platforms without openat2 the helper degrades to today's
// path-based behaviour, which these tests account for.

/// Returns whether `openat2(RESOLVE_BENEATH)` anchoring is live on this
/// host. Off implies the graceful path-based fallback is exercised.
fn nested_anchor_live() -> bool {
    cfg!(target_os = "linux") && crate::linux_capabilities::openat2_supported()
}

#[test]
fn nested_symlinkat_via_sandbox_creates_under_interior_dir() {
    // Legitimate nested create: `a/b/link` with real interior dirs must
    // succeed and place the symlink exactly under the resolved parent.
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir_all(root.join("a/b")).expect("mkdir a/b");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let rel = Path::new("a/b/link");
    let link_path = root.join(rel);
    symlinkat_via_sandbox_or_fallback(
        Some(&sandbox),
        &root,
        rel,
        &link_path,
        Path::new("../target"),
    )
    .expect("nested symlinkat");

    let meta = std::fs::symlink_metadata(&link_path).expect("stat link");
    assert!(meta.is_symlink(), "nested symlink must be created");
}

#[test]
fn nested_symlinkat_via_sandbox_allows_legit_intree_symlink() {
    // RESOLVE_BENEATH must NOT reject an in-tree symlink along the path:
    // `a/blink -> b` (both inside root) is legitimate and the create at
    // `a/blink/link` must resolve through it to `a/b/link`.
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir_all(root.join("a/b")).expect("mkdir a/b");
    // In-tree relative symlink a/blink -> b.
    symlink("b", root.join("a/blink")).expect("plant in-tree symlink");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let rel = Path::new("a/blink/link");
    let link_path = root.join(rel);
    symlinkat_via_sandbox_or_fallback(Some(&sandbox), &root, rel, &link_path, Path::new("t"))
        .expect("create through in-tree symlink must succeed");

    // The real entry lands under the resolved directory a/b.
    let resolved = root.join("a/b/link");
    assert!(
        std::fs::symlink_metadata(&resolved).is_ok(),
        "entry must exist under the symlink target dir a/b"
    );
}

#[test]
fn nested_symlinkat_via_sandbox_refuses_interior_symlink_escape() {
    // The keystone security assertion: an interior directory swapped for
    // a symlink pointing OUTSIDE the root must not let the create escape.
    let (_keep, root) = canonical_tempdir();
    let outside = root.join("outside");
    std::fs::create_dir(&outside).expect("mkdir outside");
    std::fs::create_dir(root.join("a")).expect("mkdir a");
    // a/evil -> ../outside : an interior-component symlink that escapes
    // beneath the sandbox root.
    symlink(&outside, root.join("a/evil")).expect("plant escaping symlink");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let rel = Path::new("a/evil/pwned");
    let link_path = root.join(rel);
    let result = symlinkat_via_sandbox_or_fallback(
        Some(&sandbox),
        &root,
        rel,
        &link_path,
        Path::new("payload"),
    );

    if nested_anchor_live() {
        let err = result.expect_err("interior symlink escape must be refused");
        let raw = err.raw_os_error();
        assert!(
            matches!(
                raw,
                Some(libc::EXDEV) | Some(libc::ELOOP) | Some(libc::ENOTDIR)
            ),
            "expected EXDEV/ELOOP/ENOTDIR, got {raw:?}"
        );
        assert!(
            !outside.join("pwned").exists(),
            "no entry may be created outside the root"
        );
    } else {
        // Without openat2 the helper degrades to the path-based fallback,
        // which follows the symlink (today's behaviour). Assert only that
        // the call is total; the security guarantee is the Linux gate.
        let _ = result;
    }
}

#[test]
fn nested_unlinkat_via_sandbox_refuses_interior_symlink_escape() {
    // Deleting `a/evil/victim` where `a/evil -> outside` must not reach
    // the outside file when anchoring is live.
    let (_keep, root) = canonical_tempdir();
    let outside = root.join("outside");
    std::fs::create_dir(&outside).expect("mkdir outside");
    let victim = outside.join("victim");
    std::fs::write(&victim, b"keep me").expect("write victim");
    std::fs::create_dir(root.join("a")).expect("mkdir a");
    symlink(&outside, root.join("a/evil")).expect("plant escaping symlink");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let rel = Path::new("a/evil/victim");
    let link_path = root.join(rel);
    let result =
        unlink_via_sandbox_or_fallback(Some(&sandbox), &root, rel, &link_path, UnlinkFlags::File);

    if nested_anchor_live() {
        let err = result.expect_err("unlink through interior symlink escape must be refused");
        let raw = err.raw_os_error();
        assert!(
            matches!(
                raw,
                Some(libc::EXDEV) | Some(libc::ELOOP) | Some(libc::ENOTDIR)
            ),
            "expected EXDEV/ELOOP/ENOTDIR, got {raw:?}"
        );
        assert!(
            victim.exists(),
            "outside victim must survive a refused unlink"
        );
    } else {
        let _ = result;
    }
}

#[test]
fn nested_mkdirat_via_sandbox_creates_under_interior_dir() {
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir_all(root.join("a/b")).expect("mkdir a/b");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let rel = Path::new("a/b/newdir");
    let dir_path = root.join(rel);
    mkdirat_via_sandbox_or_fallback(Some(&sandbox), &root, rel, &dir_path, 0o755)
        .expect("nested mkdirat");

    assert!(dir_path.is_dir(), "nested directory must be created");
}

#[test]
fn nested_lstat_via_sandbox_stats_under_interior_dir() {
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir_all(root.join("a/b")).expect("mkdir a/b");
    std::fs::write(root.join("a/b/file"), b"hi").expect("write");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let rel = Path::new("a/b/file");
    let full = root.join(rel);
    let outcome =
        lstat_via_sandbox_or_fallback(Some(&sandbox), &root, rel, &full).expect("nested lstat");
    // dev/ino must match the real entry regardless of which path served.
    let std_meta = std::fs::symlink_metadata(&full).expect("std stat");
    assert_eq!(outcome.dev(), std_meta.dev());
    assert_eq!(outcome.ino(), std_meta.ino());
}

#[test]
fn nested_renameat_via_sandbox_commits_under_interior_dir() {
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir_all(root.join("a/b")).expect("mkdir a/b");
    std::fs::write(root.join("a/b/tmp"), b"payload").expect("write tmp");
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let old_rel = Path::new("a/b/tmp");
    let new_rel = Path::new("a/b/final");
    let old_full = root.join(old_rel);
    let new_full = root.join(new_rel);
    renameat_via_sandbox_or_fallback(
        Some(&sandbox),
        &root,
        old_rel,
        &old_full,
        &root,
        new_rel,
        &new_full,
        true,
    )
    .expect("nested renameat");

    assert!(!old_full.exists(), "temp name must be gone after rename");
    assert_eq!(
        std::fs::read(&new_full).expect("read final"),
        b"payload",
        "final name must hold the renamed payload"
    );
}

#[test]
fn single_component_symlinkat_unchanged_by_nested_path() {
    // Regression guard: the common single-component case must still take
    // the existing fast path and behave byte-identically.
    let (_keep, root) = canonical_tempdir();
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let rel = Path::new("link");
    let link_path = root.join(rel);
    symlinkat_via_sandbox_or_fallback(Some(&sandbox), &root, rel, &link_path, Path::new("target"))
        .expect("single-component symlinkat");
    assert!(
        std::fs::symlink_metadata(&link_path)
            .expect("stat")
            .is_symlink(),
        "single-component symlink must still be created"
    );
}
