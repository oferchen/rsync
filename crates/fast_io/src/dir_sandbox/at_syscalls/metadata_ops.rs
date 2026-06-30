//! chmod/chown/utimes SEC-1.i cutover.
//!
//! Anchors the metadata-mutating `*at` syscalls (`fchmodat`,
//! `fchownat`, `utimensat`) on a parent dirfd and exposes the
//! `*_via_sandbox_or_fallback` adaptors plus the symlink-race-safe
//! [`secure_chmod_at`] that walks the parent through
//! [`crate::secure_open_dir`].

use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use filetime::FileTime;

use super::lstat::single_component_leaf;

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
    sandbox: Option<&crate::dir_sandbox::DirSandbox>,
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

/// Chmod `path` after walking its parent through [`secure_open_dir`].
///
/// Symlink-race-safe variant of [`std::fs::set_permissions`] that
/// mirrors upstream `syscall.c:do_chmod_at()` (rsync 3.4.3+). The parent
/// directory of `path` is opened with `openat2(RESOLVE_BENEATH |
/// RESOLVE_NO_SYMLINKS)` on Linux 5.6+ or `open(O_NOFOLLOW | O_DIRECTORY
/// | O_CLOEXEC)` elsewhere, then `fchmodat` is anchored on that dirfd
/// against the leaf basename. A symlink inserted into any parent
/// component of `path` causes the open to fail with `ELOOP` (or `EXDEV`
/// for `..` escapes under `openat2`), so a TOCTOU swap cannot redirect
/// the chmod to an attacker-chosen inode outside the carrier directory.
///
/// `follow_symlinks` controls only the leaf: when `false` the helper
/// passes `AT_SYMLINK_NOFOLLOW` so a swap-to-symlink at the leaf is not
/// chased into a different inode either.
///
/// Falls back to [`std::fs::set_permissions`] when `path` has no parent
/// component (root, single-component) - there is nothing to walk in that
/// case.
///
/// [`secure_open_dir`]: crate::secure_open_dir
///
/// # Errors
///
/// Surfaces either the [`secure_open_dir`](crate::secure_open_dir) error
/// or the [`fchmodat`] error verbatim. The notable security cases are
/// `ELOOP` (parent symlink), `EXDEV` (parent `..` escape under
/// `openat2`), and `ENOTDIR` (parent component is not a directory).
pub fn secure_chmod_at(path: &Path, mode: u32, follow_symlinks: bool) -> io::Result<()> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => {
            use std::os::unix::fs::PermissionsExt;
            return std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
        }
    };
    let leaf = path
        .file_name()
        .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
    let dirfd = crate::secure_open_dir(parent)?;
    fchmodat(dirfd.as_fd(), leaf, mode, follow_symlinks)
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
    sandbox: Option<&crate::dir_sandbox::DirSandbox>,
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
    sandbox: Option<&crate::dir_sandbox::DirSandbox>,
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
