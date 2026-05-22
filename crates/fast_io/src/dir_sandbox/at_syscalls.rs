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
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

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
}
