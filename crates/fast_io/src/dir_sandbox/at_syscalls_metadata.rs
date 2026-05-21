//! `*at` syscall helpers for the metadata-application cutover (SEC-1.i).
//!
//! Companion to [`super::at_syscalls`]: the unlink/lstat/create-class
//! siblings live there, the chmod/chown/utimensat siblings live here. The
//! split keeps the file mid-flight with sibling SEC-1.h additions
//! mergeable without conflicts; once both PRs land the two modules may be
//! re-folded into a single `at_syscalls` namespace.
//!
//! Each helper takes a parent dirfd plus a single-component leaf so the
//! call cannot be redirected by a TOCTOU symlink swap between the
//! receiver's "decide to apply metadata" moment and the kernel reaching
//! the inode. The path-based [`std::fs::set_permissions`] and
//! [`filetime::set_file_times`] fallbacks remain for multi-component
//! paths and the no-sandbox case so behaviour is byte-identical for
//! callers that have not yet plumbed a [`DirSandbox`](super::DirSandbox).

use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use filetime::FileTime;

/// Issue `fchmodat(dirfd, name, mode, flags)`.
///
/// The leaf is resolved relative to `dirfd`. When `follow_symlinks` is
/// `false` the helper passes `AT_SYMLINK_NOFOLLOW` so a symlink at the
/// leaf is not chased into a different inode. On Linux the `chmod` on a
/// symlink is a no-op (kernels return `EOPNOTSUPP`) but `AT_SYMLINK_NOFOLLOW`
/// is still the correct flag because it ensures the kernel never resolves
/// the link first.
///
/// `name` must not contain an interior NUL byte; callers that pull names
/// from `Path::file_name` cannot trigger this.
///
/// # Errors
///
/// Surfaces the underlying syscall error verbatim. Notable cases:
/// - `ENOENT` when `name` does not exist beneath `dirfd`.
/// - `ENOTDIR` when `dirfd` is not a directory.
/// - `EPERM` when the caller is not the owner and is not privileged.
/// - `EOPNOTSUPP` on Linux when called on a symlink with
///   `AT_SYMLINK_NOFOLLOW`.
/// - `EINVAL` when `name` contains an interior NUL byte.
pub fn fchmodat(
    dirfd: BorrowedFd<'_>,
    name: &OsStr,
    mode: u32,
    follow_symlinks: bool,
) -> io::Result<()> {
    let c_name =
        CString::new(name.as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let flags = if follow_symlinks {
        0
    } else {
        libc::AT_SYMLINK_NOFOLLOW
    };

    // SAFETY:
    // - `dirfd.as_raw_fd()` returns the raw fd of a `BorrowedFd<'_>` whose
    //   lifetime outlives the syscall.
    // - `c_name.as_ptr()` is a valid NUL-terminated C string borrowed for
    //   the duration of the call.
    // - `mode` is interpreted by the kernel as `mode_t`; the cast is the
    //   identity on every supported target (mode_t is u32 on Linux, u16 on
    //   macOS — `as` truncates the upper 16 bits, which are unused by the
    //   POSIX permission bitmask).
    // - `flags` is either `0` or `AT_SYMLINK_NOFOLLOW`, the only values
    //   `fchmodat(2)` accepts.
    #[allow(unsafe_code)]
    let rc = unsafe {
        libc::fchmodat(
            dirfd.as_raw_fd(),
            c_name.as_ptr(),
            mode as libc::mode_t,
            flags,
        )
    };

    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Issue `fchownat(dirfd, name, uid, gid, flags)`.
///
/// The leaf is resolved relative to `dirfd`. When `follow_symlinks` is
/// `false` the helper passes `AT_SYMLINK_NOFOLLOW` so the call has
/// `lchown(2)` semantics — the symlink itself is reowned rather than the
/// target it points at.
///
/// `uid` / `gid` of `u32::MAX` map to `(uid_t)-1` / `(gid_t)-1`, the
/// "leave unchanged" sentinel `fchownat(2)` recognises.
///
/// # Errors
///
/// Surfaces the underlying syscall error verbatim. Notable cases:
/// - `ENOENT` when `name` does not exist beneath `dirfd`.
/// - `ENOTDIR` when `dirfd` is not a directory.
/// - `EPERM` when the caller lacks `CAP_CHOWN` (Linux) or is not root
///   on a BSD/macOS target.
/// - `EINVAL` when `name` contains an interior NUL byte.
pub fn fchownat(
    dirfd: BorrowedFd<'_>,
    name: &OsStr,
    uid: u32,
    gid: u32,
    follow_symlinks: bool,
) -> io::Result<()> {
    let c_name =
        CString::new(name.as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let flags = if follow_symlinks {
        0
    } else {
        libc::AT_SYMLINK_NOFOLLOW
    };

    // SAFETY:
    // - `dirfd.as_raw_fd()` returns a raw fd whose lifetime is bound to
    //   the borrow and outlives the syscall.
    // - `c_name.as_ptr()` is a valid NUL-terminated C string.
    // - `uid as uid_t` / `gid as gid_t` are identity casts on every
    //   supported target (`uid_t` and `gid_t` are `u32`).
    // - `flags` is either `0` or `AT_SYMLINK_NOFOLLOW`, the only values
    //   `fchownat(2)` accepts.
    #[allow(unsafe_code)]
    let rc = unsafe {
        libc::fchownat(
            dirfd.as_raw_fd(),
            c_name.as_ptr(),
            uid as libc::uid_t,
            gid as libc::gid_t,
            flags,
        )
    };

    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Issue `utimensat(dirfd, name, [atime, mtime], flags)` with nanosecond
/// precision.
///
/// The leaf is resolved relative to `dirfd`. When `follow_symlinks` is
/// `false` the helper passes `AT_SYMLINK_NOFOLLOW`, matching the behaviour
/// of [`filetime::set_symlink_file_times`].
///
/// Times are passed as [`filetime::FileTime`] for parity with the rest of
/// the metadata path which already standardises on that type.
///
/// # Errors
///
/// Surfaces the underlying syscall error verbatim. Notable cases:
/// - `ENOENT` when `name` does not exist beneath `dirfd`.
/// - `EACCES` when the caller lacks permission and the times are not
///   `UTIME_OMIT`.
/// - `EINVAL` when `name` contains an interior NUL byte.
pub fn utimensat(
    dirfd: BorrowedFd<'_>,
    name: &OsStr,
    atime: FileTime,
    mtime: FileTime,
    follow_symlinks: bool,
) -> io::Result<()> {
    let c_name =
        CString::new(name.as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let flags = if follow_symlinks {
        0
    } else {
        libc::AT_SYMLINK_NOFOLLOW
    };
    let times = [
        libc::timespec {
            tv_sec: atime.unix_seconds() as _,
            tv_nsec: atime.nanoseconds() as _,
        },
        libc::timespec {
            tv_sec: mtime.unix_seconds() as _,
            tv_nsec: mtime.nanoseconds() as _,
        },
    ];

    // SAFETY:
    // - `dirfd.as_raw_fd()` returns a raw fd whose lifetime outlives the
    //   syscall.
    // - `c_name.as_ptr()` is a valid NUL-terminated C string.
    // - `times.as_ptr()` points at a stack-local `[timespec; 2]` (atime,
    //   mtime) that the kernel reads through for the duration of the call.
    // - `flags` is either `0` or `AT_SYMLINK_NOFOLLOW`, the only values
    //   `utimensat(2)` accepts.
    #[allow(unsafe_code)]
    let rc = unsafe { libc::utimensat(dirfd.as_raw_fd(), c_name.as_ptr(), times.as_ptr(), flags) };

    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Issue `fchmodat` against `link_path` when the `sandbox` root is the
/// immediate parent.
///
/// SEC-1.i adaptor for callers that already have an absolute path:
/// - When `sandbox` is `Some`, `link_path` equals
///   `dest_dir.join(relative_path)`, and `relative_path` has a single
///   component, the helper resolves the leaf through the sandbox dirfd so
///   a mid-syscall symlink swap on the leaf cannot redirect the chmod to
///   an attacker-chosen inode.
/// - In every other case the helper falls back to
///   [`std::fs::set_permissions`] on `link_path`.
///
/// `follow_symlinks` controls whether the sandbox fast path uses
/// `AT_SYMLINK_NOFOLLOW`. The fallback path uses [`std::fs::set_permissions`]
/// regardless because the standard library has no `lchmod`-equivalent;
/// callers that need symlink-no-follow semantics on the fallback path
/// must short-circuit before reaching this helper.
///
/// # Errors
///
/// Surfaces either the [`fchmodat`] error or the
/// [`std::fs::set_permissions`] error verbatim, depending on which path
/// was taken.
pub fn fchmodat_via_sandbox_or_fallback(
    sandbox: Option<&super::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    link_path: &Path,
    mode: u32,
    follow_symlinks: bool,
) -> io::Result<()> {
    if let Some(sandbox) = sandbox
        && let Some(leaf) = single_component_leaf(dest_dir, relative_path, link_path)
    {
        return fchmodat(sandbox.current_dirfd(), leaf, mode, follow_symlinks);
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(link_path, std::fs::Permissions::from_mode(mode))
}

/// Issue `fchownat` against `link_path` when the `sandbox` root is the
/// immediate parent.
///
/// SEC-1.i adaptor for the lchown-class cutover. Behaves like
/// [`fchmodat_via_sandbox_or_fallback`] but reowns instead of rechmoding.
/// Pass `follow_symlinks = false` for `lchown` semantics so a swap-to-symlink
/// at the leaf cannot redirect the chown into an attacker-chosen target.
///
/// `uid` / `gid` of `u32::MAX` are interpreted as "leave unchanged" per
/// `fchownat(2)`'s `(uid_t)-1` / `(gid_t)-1` sentinel.
///
/// # Errors
///
/// Surfaces either the [`fchownat`] error or the underlying
/// [`rustix::fs::chownat`] error verbatim, depending on which path was
/// taken.
pub fn fchownat_via_sandbox_or_fallback(
    sandbox: Option<&super::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    link_path: &Path,
    uid: u32,
    gid: u32,
    follow_symlinks: bool,
) -> io::Result<()> {
    if let Some(sandbox) = sandbox
        && let Some(leaf) = single_component_leaf(dest_dir, relative_path, link_path)
    {
        return fchownat(sandbox.current_dirfd(), leaf, uid, gid, follow_symlinks);
    }
    // Fallback uses raw libc::lchown / libc::chown so the no-sandbox
    // path preserves symlink-no-follow semantics that std does not
    // expose. Callers depending on the sandbox path picking up
    // AT_SYMLINK_NOFOLLOW still get matching behaviour on the fallback.
    let c_path = CString::new(link_path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY:
    // - `c_path.as_ptr()` is a valid NUL-terminated C string.
    // - The chosen syscall (`chown` vs `lchown`) is the POSIX-portable
    //   way to reach the same semantics that the sandbox path expresses
    //   via `AT_SYMLINK_NOFOLLOW`. Both syscalls are present on every
    //   supported Unix target.
    #[allow(unsafe_code)]
    let rc = unsafe {
        if follow_symlinks {
            libc::chown(c_path.as_ptr(), uid as libc::uid_t, gid as libc::gid_t)
        } else {
            libc::lchown(c_path.as_ptr(), uid as libc::uid_t, gid as libc::gid_t)
        }
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Issue `utimensat` against `link_path` when the `sandbox` root is the
/// immediate parent.
///
/// SEC-1.i adaptor for the utimes-class cutover. Routes to the sandbox
/// dirfd when the relative path is a single component, falls back to the
/// [`filetime`] crate otherwise.
///
/// `follow_symlinks = false` mirrors [`filetime::set_symlink_file_times`].
///
/// # Errors
///
/// Surfaces either the [`utimensat`] error or the
/// [`filetime::set_file_times`] / [`filetime::set_symlink_file_times`]
/// error verbatim, depending on which path was taken.
pub fn utimensat_via_sandbox_or_fallback(
    sandbox: Option<&super::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    link_path: &Path,
    atime: FileTime,
    mtime: FileTime,
    follow_symlinks: bool,
) -> io::Result<()> {
    if let Some(sandbox) = sandbox
        && let Some(leaf) = single_component_leaf(dest_dir, relative_path, link_path)
    {
        return utimensat(sandbox.current_dirfd(), leaf, atime, mtime, follow_symlinks);
    }
    if follow_symlinks {
        filetime::set_file_times(link_path, atime, mtime)
    } else {
        filetime::set_symlink_file_times(link_path, atime, mtime)
    }
}

/// Returns the leaf component of `link_path` when `link_path` is exactly
/// `dest_dir` joined with a single-component `relative_path`.
///
/// Mirrors the SEC-1.f / SEC-1.g leaf detector verbatim; kept private to
/// this module so the SEC-1.i cutover can land independently of the
/// in-flight SEC-1.h refactor that lives in [`super::at_syscalls`].
/// Multi-component relative paths take the path-based fallback until the
/// per-directory dirfd stack lands.
fn single_component_leaf<'a>(
    dest_dir: &Path,
    relative_path: &'a Path,
    link_path: &Path,
) -> Option<&'a OsStr> {
    let mut comps = relative_path.components();
    let first = match comps.next()? {
        std::path::Component::Normal(name) => name,
        _ => return None,
    };
    if comps.next().is_some() {
        return None;
    }
    if dest_dir.join(relative_path) != link_path {
        return None;
    }
    Some(first)
}

#[cfg(test)]
mod tests {
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
    fn fchmodat_sets_mode_on_regular_file() {
        let (_keep, root) = canonical_tempdir();
        let path = root.join("file");
        std::fs::write(&path, b"x").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("seed perms");
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
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600))
            .expect("seed target");
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
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("seed perms");
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
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("seed perms");
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
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("seed perms");

        let leaf = Path::new("file");
        fchmodat_via_sandbox_or_fallback(None, &root, leaf, &path, 0o644, true)
            .expect("fchmodat fallback");
        assert_eq!(
            std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777,
            0o644
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

        fchownat(dirfd.as_fd(), OsStr::new("file"), u32::MAX, u32::MAX, true)
            .expect("fchownat neg1");

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
        fchownat_via_sandbox_or_fallback(
            Some(&sandbox),
            &root,
            rel,
            &path,
            u32::MAX,
            u32::MAX,
            false,
        )
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
}
