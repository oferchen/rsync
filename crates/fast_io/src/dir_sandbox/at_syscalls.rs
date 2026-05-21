//! `*at` syscall helpers anchored on a [`DirSandbox`](super::DirSandbox).
//!
//! Wraps the bare libc entry points that have no safe equivalent in
//! `std::fs` (`fstatat`, `unlinkat`, etc.) and exposes them through a
//! typed surface the engine and transfer crates can consume without
//! taking on any `unsafe` of their own.
//!
//! Today this module carries:
//! - the lstat-class cutover for SEC-1.f
//!   (`fstatat(AT_SYMLINK_NOFOLLOW)`), and
//! - the unlink-class cutover for SEC-1.g
//!   (`unlinkat(dirfd, name, 0 | AT_REMOVEDIR)`).
//!
//! SEC-1.h-j will extend it with the remaining `*at` siblings
//! (`mkdirat`, `fchmodat`, `renameat`, ...) as those tasks land.

use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

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

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd;
    use std::os::unix::fs::symlink;

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
        use std::os::unix::fs::MetadataExt;
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
        assert!(!path.exists(), "empty directory must be gone after unlinkat");
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
        assert!(dir.exists(), "non-empty directory must survive a failed rmdir");
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
        assert!(!path.exists(), "single-component file must be removed via sandbox dirfd");
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
        assert!(!path.exists(), "single-component dir must be removed via sandbox dirfd");
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
        assert!(!path.exists(), "multi-component path must fall back to std::fs::remove_file");
    }

    #[test]
    fn unlink_via_sandbox_falls_back_when_sandbox_absent() {
        let (_keep, root) = canonical_tempdir();
        let path = root.join("file");
        std::fs::write(&path, b"x").expect("write");

        let leaf = Path::new("file");
        unlink_via_sandbox_or_fallback(None, &root, leaf, &path, UnlinkFlags::File)
            .expect("unlink fallback");
        assert!(!path.exists(), "absent-sandbox path must fall back to std::fs::remove_file");
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
        assert!(!path.exists(), "Dir flag must dispatch std::fs::remove_dir on fallback");
    }
}
