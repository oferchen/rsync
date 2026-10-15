//! lstat-class SEC-1.f adaptor and the shared single-component-leaf
//! resolver.
//!
//! [`EntryMetadata`] unifies the sandbox-anchored
//! `fstatat(AT_SYMLINK_NOFOLLOW)` result and the path-based
//! [`std::fs::symlink_metadata`] fallback behind a `dev`/`ino` surface.
//! [`single_component_leaf`] is the gate every `*_via_sandbox_or_fallback`
//! helper in this module uses to decide whether the sandbox fast path is
//! eligible.

use crate::EntryMetadata;
use std::ffi::OsStr;
use std::io;
use std::os::fd::BorrowedFd;
use std::path::Path;

use super::metadata::{fstatat_follow, fstatat_nofollow};

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
    sandbox: Option<&crate::dir_sandbox::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    link_path: &Path,
) -> io::Result<EntryMetadata> {
    stat_outcome(sandbox, dest_dir, relative_path, link_path, false)
}

/// [`lstat_via_sandbox_or_fallback`] with the leaf followed: upstream's
/// `do_stat_at()` beside its `do_lstat_at()`.
///
/// Only the leaf may resolve as a symlink; every component above it is
/// reached through the sandbox exactly as for the `lstat` form.
///
/// # Errors
///
/// As for [`lstat_via_sandbox_or_fallback`], plus `ELOOP` for a leaf symlink
/// chain that does not terminate.
///
/// # Upstream Reference
///
/// - `rsync-3.5.1/syscall.c:2336-2400` `do_stat_at()` / `do_lstat_at()`
pub fn stat_via_sandbox_or_fallback(
    sandbox: Option<&crate::dir_sandbox::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    link_path: &Path,
) -> io::Result<EntryMetadata> {
    stat_outcome(sandbox, dest_dir, relative_path, link_path, true)
}

/// The three arms shared by the `stat` and `lstat` adaptors; `follow` picks
/// `fstatat` with or without `AT_SYMLINK_NOFOLLOW`.
fn stat_outcome(
    sandbox: Option<&crate::dir_sandbox::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    link_path: &Path,
    follow: bool,
) -> io::Result<EntryMetadata> {
    let stat_at = |dirfd: BorrowedFd<'_>, name: &OsStr| {
        if follow {
            fstatat_follow(dirfd, name)
        } else {
            fstatat_nofollow(dirfd, name)
        }
    };
    if let Some(meta) =
        super::nested::at_dest_leaf(sandbox, dest_dir, relative_path, link_path, stat_at)?
    {
        return Ok(EntryMetadata::At(meta));
    }
    if follow {
        std::fs::metadata(link_path)
    } else {
        std::fs::symlink_metadata(link_path)
    }
    .map(EntryMetadata::Std)
}

/// `stat`/`lstat` of `link_path` as a [`std::fs::Metadata`], for callers whose
/// consumer is typed on it.
///
/// [`std::fs::Metadata`] has no public constructor, so an `fstatat` answer
/// cannot be returned as one. On Linux the entry is opened `O_PATH` (plus
/// `O_NOFOLLOW` for `lstat`) through the same three arms as
/// [`lstat_via_sandbox_or_fallback`] and `fstat`ed: `O_PATH` grants no access
/// to the contents, so an unreadable entry, a FIFO and a device report their
/// metadata exactly as `stat` would. Elsewhere there is no `O_PATH`, and the
/// path-based call is issued; those callers stay on the path until their
/// consumer accepts [`EntryMetadata`].
///
/// # Errors
///
/// The `openat`/`fstat` error of the anchored arm, or the path-based call's.
pub fn metadata_via_sandbox_or_fallback(
    sandbox: Option<&crate::dir_sandbox::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    link_path: &Path,
    follow: bool,
) -> io::Result<std::fs::Metadata> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let flags = if follow {
            libc::O_PATH | libc::O_CLOEXEC
        } else {
            libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW
        };
        let fstat_at = |dirfd: BorrowedFd<'_>, name: &OsStr| {
            super::open::openat(dirfd, name, flags, 0).and_then(|file| file.metadata())
        };
        if let Some(meta) =
            super::nested::at_dest_leaf(sandbox, dest_dir, relative_path, link_path, fstat_at)?
        {
            return Ok(meta);
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let _ = (sandbox, dest_dir, relative_path);
    if follow {
        std::fs::metadata(link_path)
    } else {
        std::fs::symlink_metadata(link_path)
    }
}

/// Returns the leaf component of `link_path` when `link_path` is
/// exactly `dest_dir` joined with a single-component `relative_path`.
///
/// Multi-component relative paths need a per-directory dirfd stack
/// (SEC-1.f's follow-up work), so they take the path-based fallback
/// for now.
pub(super) fn single_component_leaf<'a>(
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
