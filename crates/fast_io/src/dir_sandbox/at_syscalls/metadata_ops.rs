//! chmod/chown/utimes SEC-1.i cutover.
//!
//! Anchors the metadata-mutating `*at` syscalls (`fchmodat`,
//! `fchownat`, `utimensat`) on a parent dirfd and exposes the
//! `*_via_sandbox_or_fallback` adaptors plus the symlink-race-safe
//! [`secure_chmod_at`] that walks the parent through
//! [`crate::secure_open_dir`].

use std::ffi::{CStr, CString, OsStr};
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

/// Path-based `chown(2)` / `lchown(2)` on `link_path` through the libc
/// symbol.
///
/// The no-sandbox fallback for the lchown-class cutover. Uses the libc
/// `chown`/`lchown` symbol (not a raw syscall) so `fakeroot`'s
/// `LD_PRELOAD` interposition observes the ownership change; a raw
/// syscall would bypass fakeroot and drop every file to `0:0` under an
/// unprivileged fakeroot session.
///
/// `follow_symlinks = false` selects `lchown` so a symlink leaf is
/// reowned rather than its target.
fn chown_path_libc(link_path: &Path, uid: u32, gid: u32, follow_symlinks: bool) -> io::Result<()> {
    let c_path = CString::new(link_path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY:
    // - `c_path.as_ptr()` is a valid NUL-terminated C string borrowed for
    //   the duration of the call.
    // - `uid as uid_t` / `gid as gid_t` are identity casts on every
    //   supported target (`uid_t` / `gid_t` are `u32`); `u32::MAX` maps to
    //   the `(uid_t)-1` / `(gid_t)-1` "leave unchanged" sentinel.
    // - Both `chown(2)` and `lchown(2)` are present on every supported
    //   Unix target.
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

/// Chown `path` after walking its parent through [`secure_open_dir`].
///
/// Symlink-race-safe counterpart to a path-based `fchownat(AT_FDCWD,
/// path, ..., AT_SYMLINK_NOFOLLOW)`. `AT_SYMLINK_NOFOLLOW` only guards the
/// leaf component; a symlink swapped into any *ancestor* directory is
/// still followed, redirecting the chown to an attacker-chosen inode
/// outside the receiver's confinement. This helper mirrors
/// [`secure_chmod_at`]: the parent directory of `path` is opened with
/// `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` on Linux 5.6+ or
/// `open(O_NOFOLLOW | O_DIRECTORY | O_CLOEXEC)` elsewhere, then `fchownat`
/// is anchored on that dirfd against the leaf basename. A symlink inserted
/// into any parent component causes the open to fail with `ELOOP` (or
/// `EXDEV` for `..` escapes under `openat2`), so a TOCTOU swap cannot
/// redirect the chown.
///
/// `follow_symlinks` controls only the leaf: `false` passes
/// `AT_SYMLINK_NOFOLLOW` so a swap-to-symlink at the leaf is reowned
/// rather than chased into a different inode.
///
/// `uid` / `gid` of `u32::MAX` map to `(uid_t)-1` / `(gid_t)-1`, the
/// `fchownat(2)` "leave unchanged" sentinel.
///
/// Falls back to a path-based libc `chown`/`lchown` when `path` has no
/// parent component (single-component name in the cwd) - there is no
/// ancestor to walk in that case.
///
/// upstream: syscall.c `do_lchown()` moved under the module dirfd
/// (rsync 3.4.3+, CVE-2026-29518 fd-relative resolution).
///
/// # Errors
///
/// Surfaces either the [`secure_open_dir`](crate::secure_open_dir) error
/// or the [`fchownat`] error verbatim. The notable security cases are
/// `ELOOP` (parent symlink), `EXDEV` (parent `..` escape under
/// `openat2`), and `ENOTDIR` (parent component is not a directory).
pub fn secure_chown_at(path: &Path, uid: u32, gid: u32, follow_symlinks: bool) -> io::Result<()> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => return chown_path_libc(path, uid, gid, follow_symlinks),
    };
    let leaf = path
        .file_name()
        .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
    let dirfd = crate::secure_open_dir(parent)?;
    fchownat(dirfd.as_fd(), leaf, uid, gid, follow_symlinks)
}

/// Build a `timespec` for a `utimensat` slot, mapping `None` to
/// `UTIME_OMIT` so the corresponding timestamp is left unchanged.
fn omit_timespec(time: Option<FileTime>) -> libc::timespec {
    match time {
        Some(time) => libc::timespec {
            tv_sec: time.unix_seconds() as _,
            tv_nsec: time.nanoseconds() as _,
        },
        None => libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT,
        },
    }
}

/// Issue `utimensat(dirfd, name, [atime, mtime], flags)` where either
/// slot may be `None` (mapped to `UTIME_OMIT`).
fn utimensat_omit_raw(
    dirfd: libc::c_int,
    c_name: &CStr,
    atime: Option<FileTime>,
    mtime: Option<FileTime>,
    follow_symlinks: bool,
) -> io::Result<()> {
    let flags = if follow_symlinks {
        0
    } else {
        libc::AT_SYMLINK_NOFOLLOW
    };
    let times = [omit_timespec(atime), omit_timespec(mtime)];
    // SAFETY:
    // - `dirfd` is either a raw fd owned by the caller for the duration of
    //   the call or `AT_FDCWD`.
    // - `c_name.as_ptr()` is a valid NUL-terminated C string.
    // - `times.as_ptr()` points at a stack-local `[timespec; 2]` the
    //   kernel reads through for the duration of the call.
    // - `flags` is either `0` or `AT_SYMLINK_NOFOLLOW`, the only values
    //   `utimensat(2)` accepts.
    #[allow(unsafe_code)]
    let rc = unsafe { libc::utimensat(dirfd, c_name.as_ptr(), times.as_ptr(), flags) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Set atime/mtime on `path` after walking its parent through
/// [`secure_open_dir`], with `None` slots left unchanged (`UTIME_OMIT`).
///
/// Symlink-race-safe counterpart to a path-based `utimensat(AT_FDCWD,
/// path, ...)`. Like [`secure_chown_at`] and [`secure_chmod_at`] the
/// parent directory is opened with strict resolution and the `utimensat`
/// is anchored on that dirfd, so a symlink swapped into any ancestor
/// component is rejected (`ELOOP` / `EXDEV`) before the timestamps are
/// written. The syscall never opens the target inode, so a peerless FIFO
/// cannot block the call.
///
/// `follow_symlinks = false` passes `AT_SYMLINK_NOFOLLOW`, matching
/// [`filetime::set_symlink_file_times`]; `true` follows a symlink leaf,
/// matching [`filetime::set_file_times`].
///
/// Falls back to a path-based `utimensat(AT_FDCWD, ...)` when `path` has
/// no parent component (single-component name in the cwd).
///
/// upstream: syscall.c `do_utime()`/`set_times()` under the module dirfd
/// (rsync 3.4.3+, CVE-2026-29518 fd-relative resolution).
///
/// # Errors
///
/// Surfaces either the [`secure_open_dir`](crate::secure_open_dir) error
/// or the underlying `utimensat(2)` error verbatim.
pub fn secure_utimes_at(
    path: &Path,
    atime: Option<FileTime>,
    mtime: Option<FileTime>,
    follow_symlinks: bool,
) -> io::Result<()> {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            let leaf = path
                .file_name()
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
            let c_leaf = CString::new(leaf.as_bytes())
                .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
            let dirfd = crate::secure_open_dir(parent)?;
            utimensat_omit_raw(dirfd.as_raw_fd(), &c_leaf, atime, mtime, follow_symlinks)
        }
        _ => {
            let c_path = CString::new(path.as_os_str().as_bytes())
                .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
            utimensat_omit_raw(libc::AT_FDCWD, &c_path, atime, mtime, follow_symlinks)
        }
    }
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
    // Fallback uses the libc `chown`/`lchown` symbol so the no-sandbox
    // path preserves symlink-no-follow semantics that std does not
    // expose. Callers depending on the sandbox path picking up
    // AT_SYMLINK_NOFOLLOW still get matching behaviour on the fallback.
    chown_path_libc(link_path, uid, gid, follow_symlinks)
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
