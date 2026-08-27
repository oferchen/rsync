//! open/readlink SEC-1.s cutover: `openat`, `readlinkat`, and their
//! `*_via_sandbox_or_fallback` adaptors.
//!
//! [`openat`] returns an owned [`File`] resolved relative to a parent
//! dirfd; callers pass `O_NOFOLLOW` to refuse a terminal symlink swap.
//! [`readlinkat`] reads a symlink target without following it. The
//! fallback paths translate the libc `O_*` bits the stdlib exposes onto
//! [`std::fs::OpenOptions`] / [`std::fs::read_link`].

use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use super::lstat::single_component_leaf;

/// Issue `openat(dirfd, name, flags, mode)` and return the resulting
/// [`File`].
///
/// The leaf is resolved relative to `dirfd`. The caller chooses `flags`;
/// pass `libc::O_NOFOLLOW` to refuse a terminal symlink swap on `name`.
/// `mode` is consulted by the kernel only when `flags` includes
/// `O_CREAT` (otherwise it is ignored) and is interpreted as the
/// requested permission bits with the active umask applied.
///
/// `name` must not contain an interior NUL byte; callers that pull names
/// from `Path::file_name` cannot trigger this (paths cannot contain NUL
/// on Unix).
///
/// # Errors
///
/// Surfaces the underlying syscall error verbatim. Notable cases:
/// - `ENOENT` when `name` does not exist beneath `dirfd` and `O_CREAT`
///   is absent from `flags`.
/// - `EEXIST` when `O_CREAT | O_EXCL` is set and `name` already exists.
/// - `ENOTDIR` when `dirfd` is not a directory.
/// - `ELOOP` when `flags` includes `O_NOFOLLOW` and `name` is a symlink.
/// - `EACCES` when the caller lacks the required permissions on `dirfd`.
/// - `EINVAL` when `name` contains an interior NUL byte (translated
///   from [`std::ffi::NulError`]).
pub fn openat(dirfd: BorrowedFd<'_>, name: &OsStr, flags: i32, mode: u32) -> io::Result<File> {
    let c_name =
        CString::new(name.as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;

    // SAFETY:
    // - `dirfd.as_raw_fd()` returns the raw fd of a `BorrowedFd<'_>`
    //   whose lifetime is bound to the borrow and outlives the syscall.
    // - `c_name.as_ptr()` is a valid NUL-terminated C string borrowed
    //   for the duration of the call; the kernel does not retain the
    //   pointer past return.
    // - `flags` is forwarded verbatim; the caller is responsible for
    //   passing a valid `O_*` bitmask.
    // - `mode` is interpreted by the kernel as `mode_t` and is consulted
    //   only when `flags` includes `O_CREAT`; for other call shapes the
    //   kernel ignores it.
    #[allow(unsafe_code)]
    let raw = unsafe {
        libc::openat(
            dirfd.as_raw_fd(),
            c_name.as_ptr(),
            flags,
            mode as libc::c_uint,
        )
    };

    if raw < 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: `openat(2)` returned a non-negative fd that we own
    // exclusively. We do not duplicate, leak, or alias `raw` anywhere
    // else, so wrapping it as a `File` (which takes exclusive ownership)
    // is sound.
    #[allow(unsafe_code)]
    let file = unsafe { File::from_raw_fd(raw) };
    Ok(file)
}

/// Issue `openat` against `link_path` when the `sandbox` root is the
/// immediate parent.
///
/// SEC-1.s adaptor for callers that already have an absolute path:
/// - When `sandbox` is `Some`, `link_path` equals
///   `dest_dir.join(relative_path)`, and `relative_path` has a single
///   component, the helper resolves the leaf through the sandbox dirfd
///   so a mid-syscall symlink swap on the leaf cannot redirect the
///   open to an attacker-chosen inode.
/// - In every other case the helper applies upstream's three-arm
///   `do_open_at()` contract to `link_path` through
///   [`ConfinedFallback`](crate::ConfinedFallback).
///
/// That tail used to translate the flag word onto [`std::fs::OpenOptions`],
/// which cannot express `O_NOFOLLOW` or `O_DIRECTORY` and dropped them
/// silently - so a caller asking not to follow the leaf was given a following
/// open and no error. `ConfinedFallback::open_at` issues a real `openat(2)`,
/// which honours the whole flag word on both arms, and on arm 2 adds
/// `O_NOFOLLOW` the way upstream does
/// (`rsync-3.5.0/syscall.c:1519`).
///
/// Typical temp-file creation passes
/// `flags = libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW`,
/// `mode = 0o600`; inplace-destination opens pass
/// `flags = libc::O_RDWR | libc::O_NOFOLLOW`, `mode = 0`. The exact
/// bitmask is left to the caller for flexibility.
///
/// # Errors
///
/// Surfaces either the [`openat`] error or, on the
/// [`ConfinedFallback`](crate::ConfinedFallback) tail, that op's error
/// verbatim - including the ownership walk's refusal, and `ELOOP` when the
/// caller asked for `O_NOFOLLOW` and the leaf is a symlink.
pub fn openat_via_sandbox_or_fallback(
    sandbox: Option<&crate::dir_sandbox::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    link_path: &Path,
    flags: i32,
    mode: u32,
) -> io::Result<File> {
    if let Some(sandbox) = sandbox
        && let Some(leaf) = single_component_leaf(dest_dir, relative_path, link_path)
    {
        return openat(sandbox.current_dirfd(), leaf, flags, mode);
    }
    crate::ConfinedFallback::confined().open_at(link_path, flags, mode)
}

/// Issue `readlinkat(dirfd, name, buf, size)` and return the link
/// target as a [`PathBuf`].
///
/// The leaf is resolved relative to `dirfd`. `readlinkat(2)` never
/// follows the terminal symlink (it reads the contents of the link
/// itself), so a TOCTOU swap on the leaf cannot redirect the call to a
/// different inode.
///
/// The helper starts with a 256-byte buffer and doubles until the
/// kernel reports the full target fit (return value strictly less than
/// the buffer size) or the buffer exceeds `PATH_MAX`, at which point
/// the helper returns `ENAMETOOLONG`.
///
/// `name` must not contain an interior NUL byte; callers that pull
/// names from `Path::file_name` cannot trigger this.
///
/// # Errors
///
/// Surfaces the underlying syscall error verbatim. Notable cases:
/// - `ENOENT` when `name` does not exist beneath `dirfd`.
/// - `EINVAL` when `name` is not a symbolic link.
/// - `ENOTDIR` when `dirfd` is not a directory.
/// - `EACCES` when the caller lacks search permission on `dirfd`.
/// - `ENAMETOOLONG` when the link target exceeds `PATH_MAX`.
/// - `EINVAL` when `name` contains an interior NUL byte (translated
///   from [`std::ffi::NulError`]).
pub fn readlinkat(dirfd: BorrowedFd<'_>, name: &OsStr) -> io::Result<PathBuf> {
    use std::ffi::OsString;

    let c_name =
        CString::new(name.as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;

    // PATH_MAX cap; double the buffer until the kernel reports a result
    // strictly smaller than `buf.len()` (full target fit) or we exceed
    // the cap, in which case the link target is pathologically long and
    // we surface ENAMETOOLONG verbatim.
    let mut cap = 256usize;
    let max_cap = libc::PATH_MAX as usize;
    let mut buf: Vec<u8> = vec![0; cap];
    loop {
        // SAFETY:
        // - `dirfd.as_raw_fd()` returns the raw fd of a `BorrowedFd<'_>`
        //   whose lifetime outlives the syscall.
        // - `c_name.as_ptr()` is a valid NUL-terminated C string
        //   borrowed for the duration of the call.
        // - `buf.as_mut_ptr()` points at `cap` writable bytes the kernel
        //   may fill; the result count cannot exceed `cap` so the write
        //   stays in-bounds.
        #[allow(unsafe_code)]
        let rc = unsafe {
            libc::readlinkat(
                dirfd.as_raw_fd(),
                c_name.as_ptr(),
                buf.as_mut_ptr() as *mut libc::c_char,
                cap,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        let used = rc as usize;
        if used < cap {
            buf.truncate(used);
            let os = OsString::from_vec(buf);
            return Ok(PathBuf::from(os));
        }
        // Buffer was filled exactly; the target may be longer. Grow and
        // retry until we either fit or exceed PATH_MAX.
        if cap >= max_cap {
            return Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG));
        }
        cap = (cap * 2).min(max_cap);
        buf.resize(cap, 0);
    }
}

/// Issue `readlinkat` against `link_path` when the `sandbox` root is
/// the immediate parent.
///
/// SEC-1.s adaptor for callers that already have an absolute path:
/// - When `sandbox` is `Some`, `link_path` equals
///   `dest_dir.join(relative_path)`, and `relative_path` has a single
///   component, the helper resolves the leaf through the sandbox dirfd
///   so a mid-syscall symlink swap on the leaf cannot redirect the
///   read to a different symlink inode.
/// - In every other case the helper falls back to
///   [`std::fs::read_link`] on `link_path`.
///
/// # Errors
///
/// Surfaces either the [`readlinkat`] error or the
/// [`std::fs::read_link`] error verbatim, depending on which path was
/// taken.
pub fn readlinkat_via_sandbox_or_fallback(
    sandbox: Option<&crate::dir_sandbox::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    link_path: &Path,
) -> io::Result<PathBuf> {
    if let Some(sandbox) = sandbox
        && let Some(leaf) = single_component_leaf(dest_dir, relative_path, link_path)
    {
        return readlinkat(sandbox.current_dirfd(), leaf);
    }
    std::fs::read_link(link_path)
}

/// Open the directory the supplied dirfd refers to as a fresh `File`
/// suitable for handing to `fdopendir(3)`.
///
/// `openat(dirfd, ".", ...)` returns a fresh fd that points at the same
/// inode as `dirfd` without aliasing the caller's borrowed handle.
/// `O_NOFOLLOW` is omitted because the leaf is `.` (a directory by
/// definition); `O_DIRECTORY` is set so a kernel race that swapped the
/// inode to a non-directory between `openat` and the kernel reaching it
/// would surface as `ENOTDIR`.
pub(super) fn openat_dot(dirfd: BorrowedFd<'_>) -> io::Result<File> {
    openat(
        dirfd,
        OsStr::new("."),
        libc::O_DIRECTORY | libc::O_RDONLY | libc::O_CLOEXEC,
        0,
    )
}
