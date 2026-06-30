//! create-class SEC-1.h cutover: `mkdirat`, `symlinkat`, `linkat`.
//!
//! Each primitive anchors the new entry on a parent dirfd so a TOCTOU
//! swap on a mid-path component cannot redirect the create to an
//! attacker-chosen parent. The `*_via_sandbox_or_fallback` adaptors
//! pick the sandbox fast path for single-component leaves and fall back
//! to the path-based `std`/`fast_io` entry points otherwise.

use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::lstat::single_component_leaf;

/// Issue `mkdirat(dirfd, name, mode)`.
///
/// The leaf is resolved relative to `dirfd`. `mkdirat(2)` creates the
/// new directory atomically beneath the dirfd, so a TOCTOU swap on a
/// mid-path component between the receiver's decide-to-create moment
/// and the syscall cannot redirect the create to an attacker-chosen
/// parent: the parent is pinned by the dirfd that was opened at
/// receiver setup.
///
/// `name` must not contain an interior NUL byte; callers that pull
/// names from `Path::file_name` cannot trigger this (paths cannot
/// contain NUL on Unix).
///
/// # Errors
///
/// Surfaces the underlying syscall error verbatim. Notable cases:
/// - `EEXIST` when `name` already exists beneath `dirfd`.
/// - `ENOENT` when an intermediate component of `name` is missing.
/// - `ENOTDIR` when `dirfd` is not a directory.
/// - `EACCES` when the caller lacks write permission on `dirfd`.
/// - `EINVAL` when `name` contains an interior NUL byte (translated
///   from [`std::ffi::NulError`]).
pub fn mkdirat(dirfd: BorrowedFd<'_>, name: &OsStr, mode: u32) -> io::Result<()> {
    let c_name =
        CString::new(name.as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;

    // SAFETY:
    // - `dirfd.as_raw_fd()` returns the raw fd of a `BorrowedFd<'_>`
    //   whose lifetime is bound to the borrow and outlives the syscall.
    // - `c_name.as_ptr()` is a valid NUL-terminated C string borrowed
    //   for the duration of the call; the kernel does not retain the
    //   pointer past return.
    // - `mode` is the requested permission bits; the active umask is
    //   applied by the kernel in the standard way.
    #[allow(unsafe_code)]
    let rc = unsafe { libc::mkdirat(dirfd.as_raw_fd(), c_name.as_ptr(), mode as libc::mode_t) };

    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Issue `symlinkat(target, dirfd, name)`.
///
/// The link entry is created beneath `dirfd` so a TOCTOU swap on a
/// mid-path component cannot redirect the create to an attacker-chosen
/// parent. The link **target** string is written verbatim into the
/// symlink and is never resolved by `symlinkat(2)` itself: a malicious
/// or non-existent target is therefore not a TOCTOU concern for this
/// helper (the receiver decides whether to follow the link later).
///
/// `name` and `target` must not contain interior NUL bytes; callers
/// that pull names from `Path::file_name` cannot trigger this.
///
/// # Errors
///
/// Surfaces the underlying syscall error verbatim. Notable cases:
/// - `EEXIST` when `name` already exists beneath `dirfd`.
/// - `ENOENT` when an intermediate component of `name` is missing.
/// - `ENOTDIR` when `dirfd` is not a directory.
/// - `EACCES` when the caller lacks write permission on `dirfd`.
/// - `EINVAL` when `name` or `target` contains an interior NUL byte
///   (translated from [`std::ffi::NulError`]).
pub fn symlinkat(target: &Path, dirfd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
    let c_target = CString::new(target.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let c_name =
        CString::new(name.as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;

    // SAFETY:
    // - `c_target.as_ptr()` is a valid NUL-terminated C string borrowed
    //   for the duration of the call; the kernel does not resolve it.
    // - `dirfd.as_raw_fd()` returns the raw fd of a `BorrowedFd<'_>`
    //   whose lifetime is bound to the borrow and outlives the syscall.
    // - `c_name.as_ptr()` is a valid NUL-terminated C string borrowed
    //   for the duration of the call; the kernel does not retain the
    //   pointer past return.
    #[allow(unsafe_code)]
    let rc = unsafe { libc::symlinkat(c_target.as_ptr(), dirfd.as_raw_fd(), c_name.as_ptr()) };

    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Issue `linkat(old_dirfd, old_name, new_dirfd, new_name, 0)`.
///
/// Both endpoints are resolved relative to their respective dirfds.
/// `flags == 0` means the source must not be a symlink (the standard
/// "follow nothing" hardlink semantics rsync uses; see `hlink.c`).
/// Pinning the new parent to `new_dirfd` closes the TOCTOU window
/// between leader-path resolution and link creation.
///
/// `old_name` and `new_name` must not contain interior NUL bytes;
/// callers that pull names from `Path::file_name` cannot trigger this.
///
/// # Errors
///
/// Surfaces the underlying syscall error verbatim. Notable cases:
/// - `EEXIST` when `new_name` already exists beneath `new_dirfd`.
/// - `ENOENT` when `old_name` does not exist beneath `old_dirfd`, or
///   when an intermediate component of `new_name` is missing.
/// - `EXDEV` when the two paths resolve to different filesystems.
/// - `EPERM` when the underlying filesystem refuses hardlinks
///   (e.g., directories, or filesystems without hardlink support).
/// - `EACCES` when the caller lacks the required permissions.
/// - `EINVAL` when either name contains an interior NUL byte
///   (translated from [`std::ffi::NulError`]).
pub fn linkat(
    old_dirfd: BorrowedFd<'_>,
    old_name: &OsStr,
    new_dirfd: BorrowedFd<'_>,
    new_name: &OsStr,
) -> io::Result<()> {
    let c_old = CString::new(old_name.as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let c_new = CString::new(new_name.as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;

    // SAFETY:
    // - Both `BorrowedFd<'_>` arguments outlive the syscall (lifetime
    //   bound to the borrows passed in).
    // - Both `CString` arguments are valid NUL-terminated C strings
    //   borrowed for the duration of the call; the kernel does not
    //   retain the pointers past return.
    // - `flags == 0` is the standard rsync hardlink shape: refuse to
    //   follow the source if it is a symlink, mirroring `link(2)`.
    #[allow(unsafe_code)]
    let rc = unsafe {
        libc::linkat(
            old_dirfd.as_raw_fd(),
            c_old.as_ptr(),
            new_dirfd.as_raw_fd(),
            c_new.as_ptr(),
            0,
        )
    };

    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Issue `mkdirat` against `dir_path` when the `sandbox` root is the
/// immediate parent.
///
/// SEC-1.h adaptor for callers that already have an absolute path:
/// - When `sandbox` is `Some`, `dir_path` equals
///   `dest_dir.join(relative_path)`, and `relative_path` has a single
///   component, the helper resolves the leaf through the sandbox dirfd
///   so a mid-syscall symlink swap on the leaf cannot redirect the
///   create to an attacker-chosen parent.
/// - In every other case the helper falls back to
///   [`std::fs::create_dir`] on `dir_path`.
///
/// # Errors
///
/// Surfaces either the [`mkdirat`] error or the
/// [`std::fs::create_dir`] error verbatim, depending on which path was
/// taken.
pub fn mkdirat_via_sandbox_or_fallback(
    sandbox: Option<&crate::dir_sandbox::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    dir_path: &Path,
    mode: u32,
) -> io::Result<()> {
    if let Some(sandbox) = sandbox
        && let Some(leaf) = single_component_leaf(dest_dir, relative_path, dir_path)
    {
        return mkdirat(sandbox.current_dirfd(), leaf, mode);
    }
    std::fs::create_dir(dir_path)
}

/// Issue `symlinkat` against `link_path` when the `sandbox` root is
/// the immediate parent.
///
/// SEC-1.h adaptor for callers that already have an absolute path:
/// - When `sandbox` is `Some`, `link_path` equals
///   `dest_dir.join(relative_path)`, and `relative_path` has a single
///   component, the helper resolves the leaf through the sandbox dirfd
///   so a mid-syscall symlink swap on the leaf cannot redirect the
///   create to an attacker-chosen parent.
/// - In every other case the helper falls back to
///   [`std::os::unix::fs::symlink`] on `link_path`.
///
/// # Errors
///
/// Surfaces either the [`symlinkat`] error or the
/// [`std::os::unix::fs::symlink`] error verbatim, depending on which
/// path was taken.
pub fn symlinkat_via_sandbox_or_fallback(
    sandbox: Option<&crate::dir_sandbox::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    link_path: &Path,
    target: &Path,
) -> io::Result<()> {
    if let Some(sandbox) = sandbox
        && let Some(leaf) = single_component_leaf(dest_dir, relative_path, link_path)
    {
        return symlinkat(target, sandbox.current_dirfd(), leaf);
    }
    std::os::unix::fs::symlink(target, link_path)
}

/// Issue `linkat` against `new_path` when the `sandbox` root is the
/// immediate parent of the new entry.
///
/// SEC-1.h adaptor for hardlink follower creation:
/// - When `sandbox` is `Some`, `new_path` equals
///   `dest_dir.join(new_relative)`, and `new_relative` has a single
///   component, the helper anchors the **new** endpoint on the
///   sandbox dirfd so a mid-syscall symlink swap on the follower's
///   parent cannot redirect the create to an attacker-chosen
///   directory. The **old** (leader) endpoint stays on `AT_FDCWD`:
///   the leader path is tracked by the receiver-managed
///   `HardlinkApplyTracker`, may live under a different parent than
///   `dest_dir` for cross-segment hardlinks, and SEC-1 explicitly
///   limits this cutover to single-component leaves under
///   `dest_dir`.
/// - In every other case the helper falls back to
///   [`fast_io::hard_link`](crate::hard_link) which preserves the
///   existing io_uring `LINKAT` fast path plus
///   [`std::fs::hard_link`] error semantics (`EXDEV`, `EPERM`, ...).
///
/// # Errors
///
/// Surfaces either the [`linkat`] error or the
/// [`fast_io::hard_link`](crate::hard_link) error verbatim, depending
/// on which path was taken.
pub fn linkat_via_sandbox_or_fallback(
    sandbox: Option<&crate::dir_sandbox::DirSandbox>,
    leader_path: &Path,
    dest_dir: &Path,
    new_relative: &Path,
    new_path: &Path,
) -> io::Result<()> {
    if let Some(sandbox) = sandbox
        && let Some(new_leaf) = single_component_leaf(dest_dir, new_relative, new_path)
    {
        // The leader endpoint is intentionally resolved against
        // `AT_FDCWD`: SEC-1.h scopes the sandbox cutover to the
        // receiver-managed destination parent, and the leader may
        // live outside it. `BorrowedFd::borrow_raw(AT_FDCWD)` keeps
        // the call shape uniform without inventing a new helper.
        let leader_c = CString::new(leader_path.as_os_str().as_bytes())
            .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        let new_c = CString::new(new_leaf.as_bytes())
            .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        // SAFETY:
        // - `sandbox.current_dirfd()` outlives the syscall.
        // - Both C strings are valid NUL-terminated and borrowed for
        //   the duration of the call.
        // - `flags == 0` matches the standard rsync hardlink shape.
        #[allow(unsafe_code)]
        let rc = unsafe {
            libc::linkat(
                libc::AT_FDCWD,
                leader_c.as_ptr(),
                sandbox.current_dirfd().as_raw_fd(),
                new_c.as_ptr(),
                0,
            )
        };
        return if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        };
    }
    crate::hard_link(leader_path, new_path)
}
