//! `*at` syscall helpers anchored on a [`DirSandbox`](super::DirSandbox).
//!
//! Wraps the bare libc entry points that have no safe equivalent in
//! `std::fs` (`fstatat`, `unlinkat`, etc.) and exposes them through a
//! typed surface the engine and transfer crates can consume without
//! taking on any `unsafe` of their own.
//!
//! This module carries every SEC-1 `*at` cutover:
//! - the lstat-class cutover for SEC-1.f
//!   (`fstatat(AT_SYMLINK_NOFOLLOW)`),
//! - the unlink-class cutover for SEC-1.g
//!   (`unlinkat(dirfd, name, 0 | AT_REMOVEDIR)`),
//! - the create-class cutover for SEC-1.h
//!   (`mkdirat`, `symlinkat`, `linkat`),
//! - the metadata-class cutover for SEC-1.i
//!   (`fchmodat`, `fchownat`, `utimensat`), and
//! - the rename-class cutover for SEC-1.j
//!   (`renameat`, optionally upgraded to `renameat2(RENAME_NOREPLACE)`
//!   on Linux 3.15+).
//!
//! Each helper takes a parent dirfd plus a single-component leaf so the
//! call cannot be redirected by a TOCTOU symlink swap between the
//! receiver's "decide to act" moment and the kernel reaching the inode.
//! The path-based `std::fs` / [`filetime`] fallbacks remain for
//! multi-component paths and the no-sandbox case so behaviour is
//! byte-identical for callers that have not yet plumbed a
//! [`DirSandbox`](super::DirSandbox).

use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use filetime::FileTime;

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
fn widen_mode(value: libc::mode_t) -> u32 {
    u32::from(value)
}

/// Linux widening for `st_mode`: identity, since `mode_t` is already
/// `u32` on every supported glibc/musl target.
#[cfg(not(target_os = "macos"))]
fn widen_mode(value: libc::mode_t) -> u32 {
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

/// Result of [`lstat_via_sandbox_or_fallback`].
///
/// The variant indicates which lstat path satisfied the call. Both
/// variants expose `dev` / `ino` so the hardlink quick-check can
/// compare inode identity without caring which syscall produced the
/// numbers.
#[derive(Debug)]
pub enum LstatOutcome {
    /// Sandbox-anchored `fstatat(AT_SYMLINK_NOFOLLOW)` result.
    At(AtMetadata),
    /// Path-based [`std::fs::symlink_metadata`] result used when the
    /// sandbox was unavailable or the relative path was not a single
    /// component.
    Std(std::fs::Metadata),
}

impl LstatOutcome {
    /// Device id of the entry.
    #[must_use]
    pub fn dev(&self) -> u64 {
        match self {
            Self::At(meta) => meta.dev(),
            Self::Std(meta) => std::os::unix::fs::MetadataExt::dev(meta),
        }
    }

    /// Inode number of the entry.
    #[must_use]
    pub fn ino(&self) -> u64 {
        match self {
            Self::At(meta) => meta.ino(),
            Self::Std(meta) => std::os::unix::fs::MetadataExt::ino(meta),
        }
    }
}

/// Issue `fstatat(AT_SYMLINK_NOFOLLOW)` against `link_path` when the
/// `sandbox` root is the immediate parent.
///
/// SEC-1.f adaptor for callers that already have an absolute path:
/// - When `sandbox` is `Some`, `link_path` equals
///   `dest_dir.join(relative_path)`, and `relative_path` has a single
///   component, the helper resolves the leaf through the sandbox dirfd
///   so a mid-syscall symlink swap on the leaf cannot redirect the
///   stat.
/// - In every other case the helper falls back to
///   [`std::fs::symlink_metadata`] on `link_path`.
///
/// # Errors
///
/// Surfaces either the [`fstatat_nofollow`] error or the
/// [`std::fs::symlink_metadata`] error verbatim, depending on which
/// path was taken.
pub fn lstat_via_sandbox_or_fallback(
    sandbox: Option<&super::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    link_path: &Path,
) -> io::Result<LstatOutcome> {
    if let Some(sandbox) = sandbox
        && let Some(leaf) = single_component_leaf(dest_dir, relative_path, link_path)
    {
        return fstatat_nofollow(sandbox.current_dirfd(), leaf).map(LstatOutcome::At);
    }
    std::fs::symlink_metadata(link_path).map(LstatOutcome::Std)
}

/// Returns the leaf component of `link_path` when `link_path` is
/// exactly `dest_dir` joined with a single-component `relative_path`.
///
/// Multi-component relative paths need a per-directory dirfd stack
/// (SEC-1.f's follow-up work), so they take the path-based fallback
/// for now.
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

/// Selector for [`unlinkat`].
///
/// `unlinkat(2)` overloads a single syscall to remove either a regular
/// file/symlink/device or an empty directory, controlled by the
/// `AT_REMOVEDIR` flag. Encoding the choice as an enum at the public
/// surface keeps call sites self-documenting and makes the
/// `remove_file` / `remove_dir` distinction explicit instead of hiding
/// it behind a raw bitflag the caller has to remember.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnlinkFlags {
    /// Remove a non-directory entry (regular file, symlink, FIFO,
    /// device, socket). Maps to `flags == 0` for `unlinkat(2)`.
    ///
    /// A non-empty directory at the leaf is rejected by the kernel
    /// with `EISDIR` (Linux) or `EPERM` (other Unix variants); a
    /// missing leaf yields `ENOENT`.
    File,
    /// Remove an empty directory entry. Maps to
    /// `flags == AT_REMOVEDIR` for `unlinkat(2)` (equivalent to a
    /// `rmdir(2)` anchored on `dirfd`).
    ///
    /// A non-empty directory at the leaf is rejected with `ENOTEMPTY`
    /// (or `EEXIST` on some BSDs); a non-directory at the leaf yields
    /// `ENOTDIR`.
    Dir,
}

impl UnlinkFlags {
    /// Returns the raw `flags` argument for `unlinkat(2)`.
    fn as_raw(self) -> libc::c_int {
        match self {
            Self::File => 0,
            Self::Dir => libc::AT_REMOVEDIR,
        }
    }
}

/// Issue `unlinkat(dirfd, name, flags)`.
///
/// The leaf is resolved relative to `dirfd` and is **not** followed if
/// it is a symlink (`unlinkat(2)` never follows a terminal symlink). A
/// TOCTOU symlink swap between the receiver's decide-to-delete moment
/// and the syscall therefore cannot redirect the call into an
/// attacker-chosen directory, because the parent is pinned by the
/// dirfd that was opened at receiver setup.
///
/// `name` must not contain an interior NUL byte; callers that pull
/// names from `Path::file_name` cannot trigger this (paths cannot
/// contain NUL on Unix).
///
/// # Errors
///
/// Surfaces the underlying syscall error verbatim. Notable cases:
/// - `ENOENT` when `name` does not exist beneath `dirfd`.
/// - `ENOTDIR` when `dirfd` is not a directory, or when
///   [`UnlinkFlags::Dir`] is asked for a non-directory entry.
/// - `EISDIR` (Linux) / `EPERM` (other Unix) when [`UnlinkFlags::File`]
///   is asked for a directory entry.
/// - `ENOTEMPTY` (or `EEXIST` on some BSDs) when
///   [`UnlinkFlags::Dir`] is asked for a non-empty directory.
/// - `EACCES` when the caller lacks write permission on `dirfd`.
/// - `EINVAL` when `name` contains an interior NUL byte (translated
///   from [`std::ffi::NulError`]).
pub fn unlinkat(dirfd: BorrowedFd<'_>, name: &OsStr, flags: UnlinkFlags) -> io::Result<()> {
    let c_name =
        CString::new(name.as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;

    // SAFETY:
    // - `dirfd.as_raw_fd()` returns the raw fd of a `BorrowedFd<'_>`
    //   whose lifetime is bound to the borrow and outlives the syscall.
    // - `c_name.as_ptr()` is a valid NUL-terminated C string borrowed
    //   for the duration of the call; the kernel does not retain the
    //   pointer past return.
    // - `flags` is either 0 or `AT_REMOVEDIR`, the only valid values
    //   `unlinkat(2)` accepts. `UnlinkFlags::as_raw` enforces this
    //   discriminator-to-flag mapping at the type level.
    // - `unlinkat(2)` never follows a terminal symlink, regardless of
    //   the flag, so a swap-to-symlink TOCTOU on `name` cannot
    //   redirect the unlink to an attacker-chosen target inode.
    #[allow(unsafe_code)]
    let rc = unsafe { libc::unlinkat(dirfd.as_raw_fd(), c_name.as_ptr(), flags.as_raw()) };

    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Issue `unlinkat` against `link_path` when the `sandbox` root is the
/// immediate parent.
///
/// SEC-1.g adaptor for callers that already have an absolute path:
/// - When `sandbox` is `Some`, `link_path` equals
///   `dest_dir.join(relative_path)`, and `relative_path` has a single
///   component, the helper resolves the leaf through the sandbox dirfd
///   so a mid-syscall symlink swap on the leaf cannot redirect the
///   unlink to an attacker-chosen parent.
/// - In every other case the helper falls back to
///   [`std::fs::remove_file`] / [`std::fs::remove_dir`] on `link_path`,
///   per the [`UnlinkFlags`] selector.
///
/// # Errors
///
/// Surfaces either the [`unlinkat`] error or the
/// [`std::fs::remove_file`] / [`std::fs::remove_dir`] error verbatim,
/// depending on which path was taken.
pub fn unlink_via_sandbox_or_fallback(
    sandbox: Option<&super::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    link_path: &Path,
    flags: UnlinkFlags,
) -> io::Result<()> {
    if let Some(sandbox) = sandbox
        && let Some(leaf) = single_component_leaf(dest_dir, relative_path, link_path)
    {
        return unlinkat(sandbox.current_dirfd(), leaf, flags);
    }
    match flags {
        UnlinkFlags::File => std::fs::remove_file(link_path),
        UnlinkFlags::Dir => std::fs::remove_dir(link_path),
    }
}

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
    sandbox: Option<&super::DirSandbox>,
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
    sandbox: Option<&super::DirSandbox>,
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
    sandbox: Option<&super::DirSandbox>,
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

// ============================================================
// chmod/chown/utimes helpers (SEC-1.i)
// ============================================================

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

// ============================================================
// rename helpers (SEC-1.j)
// ============================================================

/// `RENAME_NOREPLACE` from `renameat2(2)`.
///
/// Causes `renameat2(2)` to fail with `EEXIST` when the destination
/// already exists, instead of silently overwriting it. Available on
/// Linux 3.15+ on most filesystems; older kernels return `ENOSYS` /
/// `EINVAL` and the caller must fall back to plain `renameat(2)`.
#[cfg(target_os = "linux")]
const RENAME_NOREPLACE: libc::c_uint = 1;

/// Issue `renameat(old_dirfd, old_name, new_dirfd, new_name)`.
///
/// Both endpoints are resolved relative to their respective dirfds. When
/// `replace` is `false` the helper attempts `renameat2(2)` with
/// `RENAME_NOREPLACE` so the kernel fails with `EEXIST` instead of
/// overwriting; on kernels that lack the opcode the helper falls back to
/// plain `renameat(2)` after the kernel reports `ENOSYS` / `EINVAL`.
///
/// `replace == true` matches the default [`std::fs::rename`] semantics:
/// overwrite the destination if it exists, atomically swapping the two
/// inodes when possible.
///
/// `old_name` and `new_name` must not contain interior NUL bytes;
/// callers that pull names from `Path::file_name` cannot trigger this.
///
/// # Errors
///
/// Surfaces the underlying syscall error verbatim. Notable cases:
/// - `ENOENT` when `old_name` does not exist beneath `old_dirfd`, or
///   when an intermediate component of `new_name` is missing.
/// - `EEXIST` when `replace == false` and `new_name` already exists
///   (only on kernels that accept `RENAME_NOREPLACE`; older kernels
///   silently overwrite via the fallback path).
/// - `EXDEV` when the two paths resolve to different filesystems.
/// - `EISDIR` when `new_name` is an existing directory and `old_name`
///   is not.
/// - `ENOTDIR` when `old_name` is a directory but `new_name` exists
///   and is not, or vice versa.
/// - `EACCES` when the caller lacks the required permissions.
/// - `EINVAL` when either name contains an interior NUL byte
///   (translated from [`std::ffi::NulError`]).
pub fn renameat(
    old_dirfd: BorrowedFd<'_>,
    old_name: &OsStr,
    new_dirfd: BorrowedFd<'_>,
    new_name: &OsStr,
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))] replace: bool,
) -> io::Result<()> {
    let c_old = CString::new(old_name.as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let c_new = CString::new(new_name.as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;

    #[cfg(target_os = "linux")]
    {
        if !replace {
            // SAFETY:
            // - Both `BorrowedFd<'_>` arguments outlive the syscall
            //   (lifetime bound to the borrows passed in).
            // - Both `CString` arguments are valid NUL-terminated C
            //   strings borrowed for the duration of the call; the
            //   kernel does not retain the pointers past return.
            // - `RENAME_NOREPLACE` is the only flag passed; the kernel
            //   accepts it on Linux 3.15+ and reports `ENOSYS` /
            //   `EINVAL` on older kernels which we map to the fallback.
            #[allow(unsafe_code)]
            let rc = unsafe {
                libc::syscall(
                    libc::SYS_renameat2,
                    old_dirfd.as_raw_fd(),
                    c_old.as_ptr(),
                    new_dirfd.as_raw_fd(),
                    c_new.as_ptr(),
                    RENAME_NOREPLACE,
                )
            };
            if rc == 0 {
                return Ok(());
            }
            let err = io::Error::last_os_error();
            match err.raw_os_error() {
                // Older kernels and exotic filesystems may reject the
                // flag; fall through to plain renameat(2) below so the
                // caller still gets a result, accepting that the
                // overwrite-or-not check becomes a TOCTOU on those
                // kernels. The same trade-off is documented for the
                // upstream renameat2 backstop in `util1.c`.
                Some(libc::ENOSYS) | Some(libc::EINVAL) | Some(libc::EOPNOTSUPP) => {}
                _ => return Err(err),
            }
        }
    }
    // SAFETY:
    // - Both `BorrowedFd<'_>` arguments outlive the syscall.
    // - Both `CString` arguments are valid NUL-terminated C strings
    //   borrowed for the duration of the call.
    // - `renameat(2)` is the POSIX-portable rename entry point and
    //   accepts dirfds plus relative names without flag knobs.
    #[allow(unsafe_code)]
    let rc = unsafe {
        libc::renameat(
            old_dirfd.as_raw_fd(),
            c_old.as_ptr(),
            new_dirfd.as_raw_fd(),
            c_new.as_ptr(),
        )
    };

    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Issue `renameat` against `old_link_path` / `new_link_path` when the
/// `sandbox` root is the immediate parent of **both** endpoints.
///
/// SEC-1.j adaptor for the receiver temp-file → final-name commit:
/// - When `sandbox` is `Some`, both `old_link_path` and `new_link_path`
///   equal `<old_dest_dir>.join(<old_relative_path>)` /
///   `<new_dest_dir>.join(<new_relative_path>)` with single-component
///   relatives, the helper resolves both leaves through the sandbox
///   dirfd so a mid-syscall symlink swap on either leaf cannot redirect
///   the rename to an attacker-chosen inode.
/// - In every other case the helper falls back to [`std::fs::rename`]
///   on the absolute paths so behaviour matches the existing
///   path-based commit semantics.
///
/// Today both endpoints anchor on `sandbox.current_dirfd()` because the
/// receiver always creates its temp file inside the same destination
/// parent as the final name (see `temp_guard::open_tmpfile`). The
/// two-`dest_dir` signature is retained so a future cross-dir rename
/// (e.g., `--backup-dir`) can be plumbed through here without changing
/// the call sites.
///
/// `replace` mirrors the [`renameat`] knob: `true` overwrites the
/// destination atomically (default [`std::fs::rename`] semantics);
/// `false` attempts `renameat2(RENAME_NOREPLACE)` on Linux.
///
/// # Errors
///
/// Surfaces either the [`renameat`] error or the [`std::fs::rename`]
/// error verbatim, depending on which path was taken.
#[allow(clippy::too_many_arguments)]
pub fn renameat_via_sandbox_or_fallback(
    sandbox: Option<&super::DirSandbox>,
    old_dest_dir: &Path,
    old_relative_path: &Path,
    old_link_path: &Path,
    new_dest_dir: &Path,
    new_relative_path: &Path,
    new_link_path: &Path,
    replace: bool,
) -> io::Result<()> {
    if let Some(sandbox) = sandbox
        && let Some(old_leaf) =
            single_component_leaf(old_dest_dir, old_relative_path, old_link_path)
        && let Some(new_leaf) =
            single_component_leaf(new_dest_dir, new_relative_path, new_link_path)
    {
        let dirfd = sandbox.current_dirfd();
        return renameat(dirfd, old_leaf, dirfd, new_leaf, replace);
    }
    std::fs::rename(old_link_path, new_link_path)
}

// ============================================================
// open / readlink helpers (SEC-1.s)
// ============================================================

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
/// - In every other case the helper falls back to
///   [`std::fs::OpenOptions`] against `link_path` with a best-effort
///   translation of the standard `O_*` bits (`O_RDONLY` / `O_WRONLY` /
///   `O_RDWR` for the access mode, plus `O_CREAT` / `O_TRUNC` /
///   `O_APPEND` / `O_EXCL` for the lifecycle modifiers). Flags outside
///   that set are silently dropped on the fallback path because
///   [`std::fs::OpenOptions`] does not expose every libc bit.
///
/// Typical temp-file creation passes
/// `flags = libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW`,
/// `mode = 0o600`; inplace-destination opens pass
/// `flags = libc::O_RDWR | libc::O_NOFOLLOW`, `mode = 0`. The exact
/// bitmask is left to the caller for flexibility.
///
/// # Errors
///
/// Surfaces either the [`openat`] error or the
/// [`std::fs::OpenOptions::open`] error verbatim, depending on which
/// path was taken.
pub fn openat_via_sandbox_or_fallback(
    sandbox: Option<&super::DirSandbox>,
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
    open_path_with_flags(link_path, flags, mode)
}

/// Best-effort translation of libc `O_*` flag bits onto
/// [`std::fs::OpenOptions`].
///
/// Only the bits the stdlib actually exposes are consulted: the access
/// mode (`O_RDONLY` / `O_WRONLY` / `O_RDWR`), the lifecycle bits
/// (`O_CREAT`, `O_TRUNC`, `O_APPEND`, `O_EXCL`), and the create-mode
/// argument. Everything else (`O_NOFOLLOW`, `O_DIRECTORY`, `O_CLOEXEC`,
/// `O_NONBLOCK`, ...) is silently dropped on the fallback path because
/// the stdlib has no portable knob for them; callers that need those
/// semantics must take the sandbox fast path.
fn open_path_with_flags(path: &Path, flags: i32, mode: u32) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut opts = std::fs::OpenOptions::new();
    match flags & libc::O_ACCMODE {
        libc::O_WRONLY => {
            opts.write(true);
        }
        libc::O_RDWR => {
            opts.read(true).write(true);
        }
        _ => {
            opts.read(true);
        }
    }
    if flags & libc::O_CREAT != 0 {
        opts.create(true);
        opts.mode(mode);
    }
    if flags & libc::O_TRUNC != 0 {
        opts.truncate(true);
    }
    if flags & libc::O_APPEND != 0 {
        opts.append(true);
    }
    if flags & libc::O_EXCL != 0 {
        opts.create_new(true);
    }
    opts.open(path)
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
    sandbox: Option<&super::DirSandbox>,
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

// ============================================================
// recursive unlinkat helper (SEC-1.s)
// ============================================================

/// Recursively remove the directory at `target_path` anchored on the
/// sandbox parent dirfd.
///
/// SEC-1.s adaptor that closes the symlink-swap TOCTOU window on the
/// `--delete` recursive-fallback site (audit row #27) and on the
/// receiver's `delete_extraneous_files` recursive branch (audit row
/// #6). Mirrors upstream rsync's `delete_dir_contents` + `delete_item`
/// pair (see `delete.c:48-176`) while pinning every level of the
/// descent on its own dirfd:
///
/// 1. When `sandbox` is `Some`, `target_path` equals
///    `dest_dir.join(relative_path)`, and `relative_path` has a single
///    component, the helper opens the leaf through
///    `openat(sandbox.current_dirfd(), leaf, O_DIRECTORY |
///    O_NOFOLLOW | O_RDONLY | O_CLOEXEC)` and walks the subtree using
///    only `*at` syscalls anchored on the freshly opened dirfd at each
///    level. After the inner loop drains, the helper closes the walked
///    dirfd and removes the now-empty directory through [`unlinkat`]
///    with [`UnlinkFlags::Dir`] against the sandbox parent.
/// 2. In every other case the helper falls back to
///    [`std::fs::remove_dir_all`] on `target_path`. The fallback is
///    vulnerable to the symlink-swap class the carrier closes and is
///    intended only for the no-sandbox contexts (test fixtures,
///    client-side `--local` callers, callers that have not yet plumbed
///    a [`DirSandbox`](super::DirSandbox)).
///
/// `target_path` must point at a directory; a non-directory leaf is
/// surfaced verbatim from the kernel as `ENOTDIR`. A symlink at the
/// leaf is refused with `ELOOP` (sandbox path, via `O_NOFOLLOW`) or
/// returned verbatim from [`std::fs::remove_dir_all`] (fallback path).
///
/// # Errors
///
/// Surfaces the underlying syscall error verbatim on the sandbox path
/// and the [`std::fs::remove_dir_all`] error verbatim on the fallback
/// path. Notable cases:
/// - `ENOENT` on the descent root: returned as `Ok(())` (idempotent
///   delete, matching upstream `delete_item` line 201-206).
/// - `ELOOP` when `target_path` resolves to a symlink (sandbox path
///   only): never followed.
/// - `EACCES` on an individual child entry: logged via
///   [`tracing::debug!`] and stepped over; the descent continues.
/// - `ENOTEMPTY` on the final `unlinkat(AT_REMOVEDIR)` after the
///   inner loop drained: surfaced verbatim. This indicates either a
///   concurrent writer outraced the helper or an entry was skipped
///   for `EACCES`; mirrors upstream's `DR_NOT_EMPTY` return.
/// - `ELOOP` (`io::Error::from_raw_os_error(libc::ELOOP)`) when the
///   cycle detector trips on a previously-visited `(dev, ino)` pair
///   (hardlink-to-directory is the only way to construct this and
///   requires `CAP_SYS_ADMIN` on Linux).
pub fn recursive_unlinkat_via_sandbox_or_fallback(
    sandbox: Option<&super::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    target_path: &Path,
) -> io::Result<()> {
    if let Some(sandbox) = sandbox
        && let Some(leaf) = single_component_leaf(dest_dir, relative_path, target_path)
    {
        return recursive_unlinkat(sandbox.current_dirfd(), leaf);
    }
    match std::fs::remove_dir_all(target_path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Recursively remove the directory `leaf` anchored on `parent_dirfd`.
///
/// Drives the dirfd-anchored descent loop documented in section 3 of
/// `docs/design/sec-1-s-recursive-unlinkat-helper-2026-05-22.md`. Seeds
/// the cycle-detection set with the leaf's `(dev, ino)` so any hardlink
/// pointing back at the leaf (Linux + `CAP_SYS_ADMIN` only) is
/// short-circuited before any destructive syscall fires inside the
/// cycle.
///
/// Direct-dirfd entry point for callers that already hold the parent
/// dirfd and a single-component leaf (SEC-1.q `DeleteFs::remove_dir_all_at`
/// is the first consumer). Callers that come in with absolute paths
/// should prefer
/// [`recursive_unlinkat_via_sandbox_or_fallback`] which selects between
/// this dirfd-anchored path and the [`std::fs::remove_dir_all`]
/// fallback based on whether the supplied sandbox can resolve the
/// leaf as a single component.
///
/// # Errors
///
/// Surfaces the same error set as
/// [`recursive_unlinkat_via_sandbox_or_fallback`]: `ENOENT` on the
/// descent root is folded into `Ok(())`, `ELOOP` is returned for a
/// symlink at the root or a hardlink cycle, and `ENOTEMPTY` is surfaced
/// when an `EACCES` skip left residual entries behind.
pub fn recursive_unlinkat(parent_dirfd: BorrowedFd<'_>, leaf: &OsStr) -> io::Result<()> {
    let mut visited: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    recursive_unlinkat_inner(parent_dirfd, leaf, &mut visited)
}

/// Inner recursive walker shared by the public entry point and the
/// per-entry subdir recursion. Threads the cycle-detection set through
/// each descent level so a `(dev, ino)` we have already entered aborts
/// the recursion with `ELOOP`.
fn recursive_unlinkat_inner(
    parent_dirfd: BorrowedFd<'_>,
    leaf: &OsStr,
    visited: &mut std::collections::HashSet<(u64, u64)>,
) -> io::Result<()> {
    // Step 1: open the descent root with O_DIRECTORY | O_NOFOLLOW so a
    // symlink at the leaf is refused (`ELOOP`) rather than followed.
    let listing_handle = match openat(
        parent_dirfd,
        leaf,
        libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_RDONLY | libc::O_CLOEXEC,
        0,
    ) {
        Ok(file) => file,
        Err(err) if err.raw_os_error() == Some(libc::ENOENT) => return Ok(()),
        Err(err) => return Err(err),
    };

    // Step 2: stat the leaf to seed the cycle detector. A second
    // `fstatat` from the parent is cheaper and simpler than `fstat` on
    // the freshly opened fd; both yield the same `(dev, ino)` since
    // O_NOFOLLOW guarantees we opened the same inode the kernel just
    // statted.
    let leaf_meta = fstatat_nofollow(parent_dirfd, leaf)?;
    let key = (leaf_meta.dev(), leaf_meta.ino());
    if !visited.insert(key) {
        return Err(io::Error::from_raw_os_error(libc::ELOOP));
    }

    // Step 3a: drain the children. Names are collected up-front so the
    // per-entry actions cannot race with the open `DIR*` cursor; this
    // also avoids relying on POSIX-undefined behaviour when a directory
    // is mutated during an in-flight `readdir(3)` walk. `read_dir_entries`
    // consumes the `File` (transferring fd ownership to `fdopendir(3)`),
    // so step 3b reopens the dirfd off the parent to drive the
    // per-entry `*at` calls.
    let entries = read_dir_entries(listing_handle)?;
    let dir_handle = openat(
        parent_dirfd,
        leaf,
        libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_RDONLY | libc::O_CLOEXEC,
        0,
    )?;
    let dirfd = std::os::fd::AsFd::as_fd(&dir_handle);
    for name in entries {
        if name.as_bytes() == b"." || name.as_bytes() == b".." {
            continue;
        }
        let child_meta = match fstatat_nofollow(dirfd, &name) {
            Ok(meta) => meta,
            Err(err) if err.raw_os_error() == Some(libc::ENOENT) => continue,
            Err(err) => return Err(err),
        };
        if child_meta.is_dir() {
            recursive_unlinkat_inner(dirfd, &name, visited)?;
        } else {
            unlink_child_entry(dirfd, &name)?;
        }
    }

    // Step 4: close the descent dirfd before issuing rmdir against the
    // parent. Some filesystems (notably NFS) reject `unlinkat(.., AT_REMOVEDIR)`
    // while the target is still open through a separate fd.
    drop(dir_handle);

    // Step 5: rmdir the now-empty directory. ENOENT is idempotent
    // success (matches upstream `delete_item` line 201-206); any other
    // error - including ENOTEMPTY when EACCES skips left residual
    // entries behind - propagates verbatim.
    match unlinkat(parent_dirfd, leaf, UnlinkFlags::Dir) {
        Ok(()) => Ok(()),
        Err(err) if err.raw_os_error() == Some(libc::ENOENT) => Ok(()),
        Err(err) => Err(err),
    }
}

/// Remove a single non-directory child entry, retrying the TOCTOU
/// classify-vs-act race once with [`UnlinkFlags::Dir`] when the kernel
/// reports a swapped-to-directory entry (`EISDIR` on Linux, `EPERM`
/// elsewhere). `EACCES` is logged and stepped over per upstream
/// `delete.c:48-176`; `ENOENT` is treated as idempotent success since
/// the entry already vanished.
fn unlink_child_entry(dirfd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
    match unlinkat(dirfd, name, UnlinkFlags::File) {
        Ok(()) => Ok(()),
        Err(err) => match err.raw_os_error() {
            Some(libc::ENOENT) => Ok(()),
            Some(libc::EISDIR | libc::EPERM) => match unlinkat(dirfd, name, UnlinkFlags::Dir) {
                Ok(()) => Ok(()),
                Err(retry) => match retry.raw_os_error() {
                    Some(libc::ENOENT | libc::ENOTEMPTY) => {
                        tracing::debug!(
                            target: "fast_io::dir_sandbox",
                            name = ?name,
                            os_error = retry.raw_os_error(),
                            "recursive_unlinkat: classify-vs-act race left entry unremovable, stepping over"
                        );
                        Ok(())
                    }
                    _ => Err(retry),
                },
            },
            Some(libc::EACCES) => {
                tracing::debug!(
                    target: "fast_io::dir_sandbox",
                    name = ?name,
                    "recursive_unlinkat: EACCES on child entry, stepping over per upstream"
                );
                Ok(())
            }
            _ => Err(err),
        },
    }
}

/// Read every directory entry from `dirfile` into an owned `Vec` so the
/// caller can iterate without holding a live `DIR*` cursor across
/// per-entry `unlinkat`/`fstatat` calls.
///
/// Consumes `dirfile`: ownership of the underlying fd is transferred to
/// the `DIR*` via `fdopendir(3)` and released by `closedir(3)` before
/// this helper returns. The caller therefore must not keep its own
/// handle to the fd alive past this call.
fn read_dir_entries(dirfile: File) -> io::Result<Vec<std::ffi::OsString>> {
    use std::ffi::OsString;
    use std::os::fd::IntoRawFd;

    // SAFETY:
    // - `dirfile.into_raw_fd()` releases ownership of the raw fd to us;
    //   we hand that ownership directly to `fdopendir(3)`. On success
    //   the resulting `DIR*` owns the fd and `closedir(3)` will close
    //   it. On failure the fd is leaked rather than closed twice: we
    //   reclaim ownership with `OwnedFd::from_raw_fd` so the standard
    //   `Drop` impl closes it exactly once.
    // - `dirfile` is not used after `into_raw_fd`, so the fd cannot be
    //   double-closed by `File::drop`.
    #[allow(unsafe_code)]
    let dirp = unsafe {
        let raw = dirfile.into_raw_fd();
        let ptr = libc::fdopendir(raw);
        if ptr.is_null() {
            let err = io::Error::last_os_error();
            // Reclaim ownership so the fd is closed exactly once.
            let _reclaim = std::os::fd::OwnedFd::from_raw_fd(raw);
            return Err(err);
        }
        ptr
    };

    let mut names: Vec<OsString> = Vec::new();
    let result: io::Result<()> = loop {
        // SAFETY:
        // - `errno` is reset before every call so we can distinguish
        //   end-of-stream (`readdir` returns NULL with errno unchanged)
        //   from an error (`readdir` returns NULL with errno set).
        // - `dirp` is the live `DIR*` we just created; we hold it for
        //   the duration of the loop and `closedir` is called below.
        // - The returned `*mut dirent` is owned by the C runtime and is
        //   only valid until the next `readdir(3)` call on the same
        //   `DIR*`; we copy `d_name` into an owned `OsString` before
        //   the next iteration so the borrow does not outlive the
        //   pointer.
        #[allow(unsafe_code)]
        let ent_ptr = unsafe {
            *errno_location() = 0;
            libc::readdir(dirp)
        };
        if ent_ptr.is_null() {
            // SAFETY: `errno_location` returns a thread-local
            // `*mut c_int` whose lifetime is the calling thread.
            #[allow(unsafe_code)]
            let raw_errno = unsafe { *errno_location() };
            if raw_errno == 0 {
                break Ok(());
            }
            break Err(io::Error::from_raw_os_error(raw_errno));
        }
        // SAFETY: `ent_ptr` is non-NULL per the check above; the
        // pointed-to `dirent` is owned by the C runtime for the
        // lifetime of this `readdir` call. We read `d_name` as a
        // NUL-terminated byte slice and copy it out before issuing the
        // next `readdir`.
        #[allow(unsafe_code)]
        let name_bytes = unsafe {
            let name_ptr = (*ent_ptr).d_name.as_ptr();
            let cstr = std::ffi::CStr::from_ptr(name_ptr);
            cstr.to_bytes().to_vec()
        };
        names.push(OsString::from_vec(name_bytes));
    };

    // SAFETY: `dirp` is the live `DIR*` we created above; `closedir(3)`
    // closes the underlying fd and frees the C-runtime state. After
    // this call `dirp` must not be dereferenced.
    #[allow(unsafe_code)]
    unsafe {
        libc::closedir(dirp);
    }

    result.map(|()| names)
}

/// Return a pointer to the calling thread's `errno` cell.
///
/// libc exposes the thread-local cell under different names on
/// different targets: `__errno_location` on Linux, `__error` on
/// Apple/FreeBSD/Dragonfly, `__errno` on the remaining BSDs and on
/// Android. The shim forwards to whichever symbol the current target
/// provides so the caller can clear `errno` immediately before
/// invoking `readdir(3)` (which requires the clear-then-check idiom to
/// disambiguate end-of-stream from a real error).
///
/// Kept private to this module: callers should prefer
/// [`io::Error::last_os_error`] for syscalls that do not need the
/// pre-clear contract.
fn errno_location() -> *mut libc::c_int {
    // SAFETY: each libc accessor is documented as never failing and
    // returns a thread-local `*mut c_int` whose lifetime is the calling
    // thread. We only return the pointer here; dereferences live at
    // the call site behind their own `unsafe` block and SAFETY comment.
    #[allow(unsafe_code)]
    unsafe {
        #[cfg(any(target_os = "linux", target_os = "hurd", target_os = "redox"))]
        {
            libc::__errno_location()
        }
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "dragonfly"
        ))]
        {
            libc::__error()
        }
        #[cfg(any(target_os = "netbsd", target_os = "openbsd", target_os = "android"))]
        {
            libc::__errno()
        }
        #[cfg(any(target_os = "illumos", target_os = "solaris"))]
        {
            libc::___errno()
        }
    }
}

// ============================================================
// read_dir helper (SEC-1.q2)
// ============================================================

/// File-type discriminator returned by [`DirEntryView::file_type`].
///
/// The receiver-side `--delete` loop only needs to distinguish
/// directories, symlinks, and everything else (regular files, devices,
/// FIFOs, sockets); the kernel `d_type` field exposes the same shape
/// when available and [`fstatat_nofollow`] backfills it when the
/// underlying filesystem reports [`libc::DT_UNKNOWN`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    /// Directory entry.
    Dir,
    /// Symbolic link (never followed by the sandbox helpers).
    Symlink,
    /// Regular file, device, FIFO, socket, or unknown non-directory.
    Other,
}

impl EntryKind {
    fn from_dt(dt: u8) -> Option<Self> {
        match dt {
            libc::DT_DIR => Some(Self::Dir),
            libc::DT_LNK => Some(Self::Symlink),
            libc::DT_UNKNOWN => None,
            _ => Some(Self::Other),
        }
    }

    fn from_mode(mode: u32) -> Self {
        // `libc::S_IFMT` is `mode_t`: `u16` on macOS, `u32` on Linux.
        // [`widen_mode`] keeps the comparison portable without tripping
        // `clippy::unnecessary_cast` on either target.
        let masked = mode & widen_mode(libc::S_IFMT);
        if masked == widen_mode(libc::S_IFDIR) {
            Self::Dir
        } else if masked == widen_mode(libc::S_IFLNK) {
            Self::Symlink
        } else {
            Self::Other
        }
    }

    /// Returns `true` when the entry is a directory.
    #[must_use]
    pub fn is_dir(self) -> bool {
        matches!(self, Self::Dir)
    }

    /// Returns `true` when the entry is a symbolic link.
    #[must_use]
    pub fn is_symlink(self) -> bool {
        matches!(self, Self::Symlink)
    }
}

/// One entry from [`ReadDirOutcome`] exposing the leaf name and the
/// classify bits the receiver-side `--delete` loop needs.
///
/// The view is produced by both the sandbox-anchored and the path-based
/// branches so the caller can swap on [`ReadDirOutcome`] without
/// branching on the underlying syscall family. The leaf name is owned so
/// the caller can hold it across further `*at` calls without keeping the
/// directory cursor live.
#[derive(Clone, Debug)]
pub struct DirEntryView {
    name: std::ffi::OsString,
    kind: Option<EntryKind>,
}

impl DirEntryView {
    /// The leaf name of this directory entry.
    #[must_use]
    pub fn file_name(&self) -> &std::ffi::OsStr {
        &self.name
    }

    /// Consume the view and return the owned leaf name.
    #[must_use]
    pub fn into_file_name(self) -> std::ffi::OsString {
        self.name
    }

    /// Classifies the entry without following symlinks, or `None` when
    /// the underlying filesystem reported [`libc::DT_UNKNOWN`] and the
    /// caller chose not to stat the leaf.
    #[must_use]
    pub fn file_type(&self) -> Option<EntryKind> {
        self.kind
    }
}

/// Result of [`read_dir_via_sandbox_or_fallback`].
///
/// The variant indicates which read path satisfied the call. Both
/// variants iterate as `io::Result<DirEntryView>` so the caller can swap
/// on the variant without branching on the per-entry shape.
#[derive(Debug)]
pub enum ReadDirOutcome {
    /// Sandbox-anchored listing collected via `openat(O_DIRECTORY |
    /// O_NOFOLLOW)` + `fdopendir`. The whole listing is materialised up
    /// front so the dirfd is released before the caller starts issuing
    /// per-entry actions (matches the `recursive_unlinkat` invariant).
    At(std::vec::IntoIter<DirEntryView>),
    /// Path-based [`std::fs::read_dir`] iterator used when the sandbox
    /// was unavailable or the relative path was not a single component.
    Std(std::fs::ReadDir),
}

impl Iterator for ReadDirOutcome {
    type Item = io::Result<DirEntryView>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::At(iter) => iter.next().map(Ok),
            Self::Std(iter) => iter.next().map(|res| {
                res.map(|entry| {
                    let kind = entry.file_type().ok().map(|ft| {
                        if ft.is_dir() {
                            EntryKind::Dir
                        } else if ft.is_symlink() {
                            EntryKind::Symlink
                        } else {
                            EntryKind::Other
                        }
                    });
                    DirEntryView {
                        name: entry.file_name(),
                        kind,
                    }
                })
            }),
        }
    }
}

/// Open `target_path` as a directory and list its entries, anchoring on
/// the sandbox dirfd when possible.
///
/// SEC-1.q2 adaptor for the receiver-side `--delete` scan (audit row #5).
/// Mirrors the existing `*_via_sandbox_or_fallback` shape:
/// - When `sandbox` is `Some` and `relative_path` is empty or `.`, the
///   listing targets `dest_dir` itself; the helper opens a fresh
///   `openat(dirfd, ".", O_DIRECTORY | O_RDONLY | O_CLOEXEC)` against
///   the sandbox dirfd so `fdopendir(3)` receives its own owned fd and
///   the caller's sandbox handle stays intact.
/// - When `sandbox` is `Some`, `target_path` equals
///   `dest_dir.join(relative_path)`, and `relative_path` has a single
///   `Component::Normal`, the helper opens the leaf through
///   `openat(sandbox.current_dirfd(), leaf, O_DIRECTORY | O_NOFOLLOW |
///   O_RDONLY | O_CLOEXEC)` so a TOCTOU symlink swap on the leaf cannot
///   redirect the listing into an attacker-chosen directory.
/// - In every other case the helper falls back to
///   [`std::fs::read_dir`] on `target_path`. The fallback is vulnerable
///   to the symlink-swap class the carrier closes; it is intended only
///   for the no-sandbox contexts and multi-component descents that the
///   SEC-1.f-q chain has not yet plumbed.
///
/// The sandbox-anchored branch materialises the full listing up front
/// (matching the `recursive_unlinkat` invariant) so the dirfd is closed
/// before per-entry `unlinkat`/`fstatat` syscalls fire. The std branch
/// lazily iterates as today.
///
/// # Errors
///
/// Surfaces the underlying syscall error verbatim. Notable cases:
/// - `ELOOP` or `ENOTDIR` when the leaf is a symlink and the sandbox
///   path was selected (`O_NOFOLLOW`).
/// - `ENOENT` when `target_path` does not exist.
/// - `EACCES` when the caller lacks search permission on the leaf or
///   the parent dirfd.
pub fn read_dir_via_sandbox_or_fallback(
    sandbox: Option<&super::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    target_path: &Path,
) -> io::Result<ReadDirOutcome> {
    if let Some(sandbox) = sandbox {
        if relative_path.as_os_str().is_empty() || relative_path == Path::new(".") {
            if dest_dir == target_path {
                let parent = sandbox.current_dirfd();
                let dir_handle = openat_dot(parent)?;
                let entries = read_dir_entry_views(dir_handle, parent, None)?;
                return Ok(ReadDirOutcome::At(entries.into_iter()));
            }
        } else if let Some(leaf) = single_component_leaf(dest_dir, relative_path, target_path) {
            let parent = sandbox.current_dirfd();
            let dir_handle = openat(
                parent,
                leaf,
                libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_RDONLY | libc::O_CLOEXEC,
                0,
            )?;
            let entries = read_dir_entry_views(dir_handle, parent, Some(leaf))?;
            return Ok(ReadDirOutcome::At(entries.into_iter()));
        }
    }
    std::fs::read_dir(target_path).map(ReadDirOutcome::Std)
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
fn openat_dot(dirfd: BorrowedFd<'_>) -> io::Result<File> {
    openat(
        dirfd,
        OsStr::new("."),
        libc::O_DIRECTORY | libc::O_RDONLY | libc::O_CLOEXEC,
        0,
    )
}

/// Materialise the full listing for the directory `dirfile` refers to,
/// classifying each entry by `d_type` when the filesystem provides it
/// and falling back to [`fstatat_nofollow`] off `parent_dirfd` when it
/// reports [`libc::DT_UNKNOWN`].
///
/// Consumes `dirfile`: ownership of the underlying fd is transferred to
/// the `DIR*` via `fdopendir(3)` and released by `closedir(3)` before
/// this helper returns. `parent_dirfd` plus `leaf_in_parent` describe
/// where to reopen the directory when a DT_UNKNOWN backfill is needed;
/// `leaf_in_parent == None` means `parent_dirfd` itself is the listing
/// target (the `dest_dir == target_path` branch).
fn read_dir_entry_views(
    dirfile: File,
    parent_dirfd: BorrowedFd<'_>,
    leaf_in_parent: Option<&OsStr>,
) -> io::Result<Vec<DirEntryView>> {
    use std::ffi::OsString;
    use std::os::fd::IntoRawFd;

    // SAFETY:
    // - `dirfile.into_raw_fd()` releases ownership of the raw fd to us;
    //   we hand that ownership directly to `fdopendir(3)`. On success
    //   the resulting `DIR*` owns the fd and `closedir(3)` will close
    //   it. On failure we reclaim ownership with `OwnedFd::from_raw_fd`
    //   so the standard `Drop` impl closes it exactly once.
    // - `dirfile` is not used after `into_raw_fd`, so the fd cannot be
    //   double-closed by `File::drop`.
    #[allow(unsafe_code)]
    let dirp = unsafe {
        let raw = dirfile.into_raw_fd();
        let ptr = libc::fdopendir(raw);
        if ptr.is_null() {
            let err = io::Error::last_os_error();
            let _reclaim = std::os::fd::OwnedFd::from_raw_fd(raw);
            return Err(err);
        }
        ptr
    };

    let mut entries: Vec<DirEntryView> = Vec::new();
    let mut needs_backfill = false;
    let result: io::Result<()> = loop {
        // SAFETY:
        // - `errno` is reset before every call so we can distinguish
        //   end-of-stream (`readdir` returns NULL with errno unchanged)
        //   from an error (`readdir` returns NULL with errno set).
        // - `dirp` is the live `DIR*` we just created; we hold it for
        //   the duration of the loop and `closedir` is called below.
        // - The returned `*mut dirent` is owned by the C runtime and is
        //   only valid until the next `readdir(3)` call on the same
        //   `DIR*`; we copy `d_name` into an owned `OsString` before
        //   the next iteration so the borrow does not outlive the
        //   pointer.
        #[allow(unsafe_code)]
        let ent_ptr = unsafe {
            *errno_location() = 0;
            libc::readdir(dirp)
        };
        if ent_ptr.is_null() {
            // SAFETY: `errno_location` returns a thread-local
            // `*mut c_int` whose lifetime is the calling thread.
            #[allow(unsafe_code)]
            let raw_errno = unsafe { *errno_location() };
            if raw_errno == 0 {
                break Ok(());
            }
            break Err(io::Error::from_raw_os_error(raw_errno));
        }
        // SAFETY: `ent_ptr` is non-NULL per the check above; the
        // pointed-to `dirent` is owned by the C runtime for the
        // lifetime of this `readdir` call. We read `d_name` and
        // `d_type` and copy `d_name` out before issuing the next
        // `readdir`.
        #[allow(unsafe_code)]
        let (name_bytes, dt) = unsafe {
            let name_ptr = (*ent_ptr).d_name.as_ptr();
            let cstr = std::ffi::CStr::from_ptr(name_ptr);
            let dt = (*ent_ptr).d_type;
            (cstr.to_bytes().to_vec(), dt)
        };
        let name = OsString::from_vec(name_bytes);
        if name.as_bytes() == b"." || name.as_bytes() == b".." {
            continue;
        }
        let kind = EntryKind::from_dt(dt);
        if kind.is_none() {
            needs_backfill = true;
        }
        entries.push(DirEntryView { name, kind });
    };

    // SAFETY: `dirp` is the live `DIR*` we created above; `closedir(3)`
    // closes the underlying fd and frees the C-runtime state. After
    // this call `dirp` must not be dereferenced.
    #[allow(unsafe_code)]
    unsafe {
        libc::closedir(dirp);
    }

    result?;

    // Backfill DT_UNKNOWN entries with `fstatat` against a freshly
    // opened dirfd. Filesystems such as XFS and many FUSE mounts always
    // report DT_UNKNOWN; without the backfill the receiver cannot
    // distinguish a directory from a regular file and would mis-dispatch
    // the unlink. The reopen anchors on the same parent that produced
    // the listing so the stat is consistent with the directory we just
    // read.
    if needs_backfill {
        let backfill_dir = match leaf_in_parent {
            Some(leaf) => openat(
                parent_dirfd,
                leaf,
                libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_RDONLY | libc::O_CLOEXEC,
                0,
            )?,
            None => openat_dot(parent_dirfd)?,
        };
        let backfill_fd = std::os::fd::AsFd::as_fd(&backfill_dir);
        for entry in entries.iter_mut().filter(|e| e.kind.is_none()) {
            entry.kind = match fstatat_nofollow(backfill_fd, &entry.name) {
                Ok(meta) => Some(EntryKind::from_mode(meta.mode())),
                Err(err) if err.raw_os_error() == Some(libc::ENOENT) => None,
                Err(err) => return Err(err),
            };
        }
    }

    Ok(entries)
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
        let outcome =
            lstat_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &link).expect("lstat");
        assert!(matches!(outcome, LstatOutcome::At(_)));
    }

    #[test]
    fn lstat_via_sandbox_falls_back_for_multi_component() {
        let (_keep, root) = canonical_tempdir();
        std::fs::create_dir(root.join("sub")).expect("mkdir sub");
        std::fs::write(root.join("sub/file"), b"hello").expect("write");
        let sandbox = DirSandbox::open_root(&root).expect("sandbox");

        let rel = Path::new("sub/file");
        let link = root.join(rel);
        let outcome =
            lstat_via_sandbox_or_fallback(Some(&sandbox), &root, rel, &link).expect("lstat");
        assert!(
            matches!(outcome, LstatOutcome::Std(_)),
            "multi-component paths must take the fallback until SEC-1.f descent is wired"
        );
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
        let via_at = lstat_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &path)
            .expect("at-path lstat");
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
    fn unlink_via_sandbox_falls_back_for_multi_component() {
        let (_keep, root) = canonical_tempdir();
        std::fs::create_dir(root.join("sub")).expect("mkdir sub");
        let path = root.join("sub/file");
        std::fs::write(&path, b"x").expect("write");
        let sandbox = DirSandbox::open_root(&root).expect("sandbox");

        let rel = Path::new("sub/file");
        unlink_via_sandbox_or_fallback(Some(&sandbox), &root, rel, &path, UnlinkFlags::File)
            .expect("unlink fallback");
        assert!(
            !path.exists(),
            "multi-component path must fall back to std::fs::remove_file"
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
    fn secure_chmod_at_changes_mode_on_clean_path() {
        let (_keep, root) = canonical_tempdir();
        let path = root.join("file");
        std::fs::write(&path, b"x").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("seed perms");

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

        let err =
            readlinkat(dirfd.as_fd(), OsStr::new("file")).expect_err("non-symlink must error");
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

    // ========================================================
    // recursive_unlinkat_via_sandbox_or_fallback tests (SEC-1.s)
    // ========================================================

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
    fn recursive_unlinkat_falls_back_for_multi_component_relative() {
        // Multi-component relative paths take the path-based fallback
        // (the SEC-1.f / SEC-1.g family does the same); the helper
        // must still remove the subtree end-to-end.
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

    // ========================================================
    // read_dir_via_sandbox_or_fallback tests (SEC-1.q2)
    // ========================================================

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

        let outcome =
            read_dir_via_sandbox_or_fallback(Some(&sandbox), &root, Path::new("sub"), &sub)
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

        let via_at =
            read_dir_via_sandbox_or_fallback(Some(&sandbox), &root, Path::new("sub"), &sub)
                .expect("via at");
        let mut at_names = collect_names(via_at);
        at_names.sort();

        let via_std =
            read_dir_via_sandbox_or_fallback(None, &root, Path::new("sub"), &sub).expect("via std");
        let mut std_names = collect_names(via_std);
        std_names.sort();

        assert_eq!(at_names, std_names, "sandbox and std listings must agree");
    }
}
