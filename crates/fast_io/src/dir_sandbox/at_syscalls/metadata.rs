//! `AtMetadata` and the `fstatat(AT_SYMLINK_NOFOLLOW)` primitive.
//!
//! Owns the lstat-class SEC-1.f surface: the typed [`AtMetadata`] wrapper
//! over `libc::stat`, the platform-conditional `widen_*` helpers that
//! match [`std::os::unix::fs::MetadataExt`] widths, and
//! [`fstatat_nofollow`] which fills the struct without following a
//! terminal symlink.

use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;

/// Metadata returned by [`fstatat_nofollow`].
///
/// Owns the raw `libc::stat` filled by the kernel and exposes typed
/// accessors for the fields the SEC-1.f cutover sites consume
/// (`is_symlink` / `is_dir` / `is_file` / `dev` / `ino` / `mode` /
/// `size`). The fields are kept private so future kernels can grow
/// `struct stat` without breaking the wire of this type.
///
/// `AtMetadata` is constructed only through [`fstatat_nofollow`]; there
/// is no public constructor. The type is `Copy` because `libc::stat` is
/// `Copy` on every supported target.
#[derive(Clone, Copy, Debug)]
pub struct AtMetadata {
    stat: libc::stat,
}

impl AtMetadata {
    /// Returns `true` when the entry is a symbolic link.
    ///
    /// Because [`fstatat_nofollow`] passes `AT_SYMLINK_NOFOLLOW`, a
    /// symlink at the leaf is reported as a symlink rather than
    /// resolved to its target.
    #[must_use]
    pub fn is_symlink(&self) -> bool {
        (self.stat.st_mode & libc::S_IFMT) == libc::S_IFLNK
    }

    /// Returns `true` when the entry is a directory.
    #[must_use]
    pub fn is_dir(&self) -> bool {
        (self.stat.st_mode & libc::S_IFMT) == libc::S_IFDIR
    }

    /// Returns `true` when the entry is a regular file.
    #[must_use]
    pub fn is_file(&self) -> bool {
        (self.stat.st_mode & libc::S_IFMT) == libc::S_IFREG
    }

    /// Device id of the filesystem containing the entry.
    ///
    /// Widened to `u64` to match
    /// [`std::os::unix::fs::MetadataExt::dev`]. The widening is
    /// platform-conditional because `dev_t` is `i32` on macOS but
    /// `u64` on Linux.
    #[must_use]
    pub fn dev(&self) -> u64 {
        widen_dev(self.stat.st_dev)
    }

    /// Inode number.
    ///
    /// Widened to `u64` to match
    /// [`std::os::unix::fs::MetadataExt::ino`].
    #[must_use]
    pub fn ino(&self) -> u64 {
        widen_ino(self.stat.st_ino)
    }

    /// Raw `st_mode` from `struct stat`.
    #[must_use]
    pub fn mode(&self) -> u32 {
        widen_mode(self.stat.st_mode)
    }

    /// Size of the file in bytes (or the length of the symlink target
    /// when [`is_symlink`](Self::is_symlink) is `true`).
    #[must_use]
    pub fn size(&self) -> u64 {
        widen_size(self.stat.st_size)
    }
}

/// Widen `st_dev` to `u64`. `dev_t` is `i32` on macOS and `u64` on
/// Linux; the two `#[cfg]` arms keep the conversion explicit without
/// triggering `clippy::unnecessary_cast` on either platform.
#[cfg(target_os = "macos")]
fn widen_dev(value: libc::dev_t) -> u64 {
    value as u64
}

/// Linux widening for `st_dev`: identity, since `dev_t` is already
/// `u64` on every supported glibc/musl target.
#[cfg(not(target_os = "macos"))]
fn widen_dev(value: libc::dev_t) -> u64 {
    value
}

/// Widen `st_ino` to `u64`. `ino_t` is `u64` on every supported Unix
/// target we ship, so the conversion is the identity.
fn widen_ino(value: libc::ino_t) -> u64 {
    value
}

/// Widen `st_size` to `u64`. `off_t` is signed (`i64`) on every
/// supported Unix target.
fn widen_size(value: libc::off_t) -> u64 {
    value as u64
}

/// Widen `st_mode` to `u32`. `mode_t` is `u16` on macOS and `u32` on
/// Linux; the two `#[cfg]` arms keep the conversion explicit without
/// triggering `clippy::useless_conversion` (Linux) or
/// `clippy::unnecessary_cast` (either platform).
#[cfg(target_os = "macos")]
pub(super) fn widen_mode(value: libc::mode_t) -> u32 {
    u32::from(value)
}

/// Linux widening for `st_mode`: identity, since `mode_t` is already
/// `u32` on every supported glibc/musl target.
#[cfg(not(target_os = "macos"))]
pub(super) fn widen_mode(value: libc::mode_t) -> u32 {
    value
}

/// Issue `fstatat(dirfd, name, &mut stat, AT_SYMLINK_NOFOLLOW)`.
///
/// The leaf is resolved relative to `dirfd` and is **not** followed if
/// it turns out to be a symlink, so a TOCTOU symlink swap between path
/// walk and stat cannot redirect the call to a different inode. This is
/// the SEC-1.f primitive consumed by every lstat-class cutover site.
///
/// `name` must not contain an interior NUL byte; callers that pull
/// names from `Path::file_name` cannot trigger this (paths cannot
/// contain NUL on Unix).
///
/// # Errors
///
/// Surfaces the underlying syscall error verbatim. Notable cases:
/// - `ENOENT` when `name` does not exist beneath `dirfd`.
/// - `ENOTDIR` when `dirfd` is not a directory.
/// - `EACCES` when the caller lacks search permission on `dirfd`.
/// - `EINVAL` when `name` contains an interior NUL byte (translated
///   from [`std::ffi::NulError`]).
pub fn fstatat_nofollow(dirfd: BorrowedFd<'_>, name: &OsStr) -> io::Result<AtMetadata> {
    let c_name =
        CString::new(name.as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;

    // SAFETY:
    // - `dirfd.as_raw_fd()` returns the raw fd of a `BorrowedFd<'_>`
    //   whose lifetime is bound to the borrow and outlives the syscall.
    // - `c_name.as_ptr()` is a valid NUL-terminated C string borrowed
    //   for the duration of the call; the kernel does not retain the
    //   pointer past return.
    // - `stat.as_mut_ptr()` points at a stack-local
    //   `MaybeUninit<libc::stat>` that the kernel writes through. On
    //   success we assume the struct is fully initialised (the kernel
    //   contract for `fstatat(2)` on success); on failure we never read
    //   from it.
    // - `AT_SYMLINK_NOFOLLOW` selects the no-follow variant so a
    //   symlink at the leaf is rejected/reported, not resolved.
    #[allow(unsafe_code)]
    let (rc, stat) = unsafe {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let rc = libc::fstatat(
            dirfd.as_raw_fd(),
            c_name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        );
        (rc, stat)
    };

    if rc == 0 {
        // SAFETY: `fstatat` returned 0, so the kernel has fully
        // initialised the `libc::stat` we passed in.
        #[allow(unsafe_code)]
        let stat = unsafe { stat.assume_init() };
        Ok(AtMetadata { stat })
    } else {
        Err(io::Error::last_os_error())
    }
}
