//! unlink-class SEC-1.g cutover plus the SEC-1.s recursive
//! `unlinkat` descent.
//!
//! [`UnlinkFlags`] encodes the file-vs-empty-directory choice
//! `unlinkat(2)` overloads onto a single syscall. [`unlinkat`] and
//! [`unlink_via_sandbox_or_fallback`] cover the single-leaf removal; the
//! [`recursive_unlinkat`] family drives the dirfd-anchored descent that
//! mirrors upstream `delete.c`'s `delete_dir_contents` + `delete_item`
//! pair while pinning every level on its own dirfd.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::fd::AsFd;
use std::os::fd::AsRawFd;
use std::os::fd::BorrowedFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::sync::OnceLock;

use super::errno_location;
use super::lstat::single_component_leaf;
use super::metadata::{AtMetadata, fstatat_nofollow};
use super::metadata_ops::fchmodat;
use super::nested::{ParentAnchor, anchor_parent};
use super::open::openat;
use std::ffi::CString;

/// Owner write bit (`S_IWUSR`). A plain `u32` constant rather than
/// `libc::S_IWUSR` (whose type varies by target) sidesteps a
/// platform-conditional cast, matching the existing convention in
/// `metadata::chmod`.
const S_IWUSR: u32 = 0o200;

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

/// Outcome of a recursive `unlinkat` descent, mirroring upstream
/// `delete.c`'s `delete_dir_contents` return contract (`delete.c:86-210`).
///
/// The descent never aborts on a per-child failure - it logs the entry and
/// steps over it - so these two flags let the caller reconstruct the
/// exit-code decision upstream makes after the pass finishes:
///
/// - `not_empty` mirrors `DR_NOT_EMPTY`: the final `rmdir` on the descent
///   root reported `ENOTEMPTY`, so entries survived the peel (a stepped-over
///   child error, a concurrent writer, or filtered/protected content the
///   caller deliberately left in place). On its own this is benign (exit 0).
/// - `had_errors` mirrors `rsyserr(FERROR_XFER, ...)` + `io_error |=
///   IOERR_GENERAL`: the descent stepped over at least one genuine child
///   unlink/rmdir error (`EACCES` and friends). The pass still deletes the
///   remaining siblings, but the run must finish `RERR_PARTIAL` (23).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UnlinkResidue {
    /// The final `rmdir` on the descent root reported `ENOTEMPTY`: entries
    /// survived the peel.
    pub not_empty: bool,
    /// A genuine child unlink/rmdir error was logged and stepped over.
    pub had_errors: bool,
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
    sandbox: Option<&crate::dir_sandbox::DirSandbox>,
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
    if let ParentAnchor::Anchored { dirfd, name } =
        anchor_parent(sandbox, dest_dir, relative_path, link_path)?
    {
        return unlinkat(dirfd.as_fd(), name, flags);
    }
    match flags {
        UnlinkFlags::File => std::fs::remove_file(link_path),
        UnlinkFlags::Dir => std::fs::remove_dir(link_path),
    }
}

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
///    a [`DirSandbox`](crate::dir_sandbox::DirSandbox)).
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
    sandbox: Option<&crate::dir_sandbox::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    target_path: &Path,
) -> io::Result<()> {
    if let Some(sandbox) = sandbox
        && let Some(leaf) = single_component_leaf(dest_dir, relative_path, target_path)
    {
        return recursive_unlinkat(sandbox.current_dirfd(), leaf)
            .and_then(residue_to_legacy_result);
    }
    if let ParentAnchor::Anchored { dirfd, name } =
        anchor_parent(sandbox, dest_dir, relative_path, target_path)?
    {
        return recursive_unlinkat(dirfd.as_fd(), name).and_then(residue_to_legacy_result);
    }
    remove_dir_all_with_uid_write_fix(target_path)
}

/// Collapses a recursive-descent [`UnlinkResidue`] back to the historical
/// `io::Result<()>` contract the receiver call sites expect: a residual
/// non-empty root surfaces as `ENOTEMPTY` (upstream `DR_NOT_EMPTY`), matching
/// the pre-[`UnlinkResidue`] behaviour exactly. The stepped-over-error flag is
/// dropped here because those callers do not yet consume it; the local-copy
/// delete emitter consumes it through the dirfd-anchored
/// [`recursive_unlinkat`] entry point instead.
fn residue_to_legacy_result(residue: UnlinkResidue) -> io::Result<()> {
    if residue.not_empty {
        Err(io::Error::from_raw_os_error(libc::ENOTEMPTY))
    } else {
        Ok(())
    }
}

/// Path-based sibling of the dirfd-anchored recursive removal above, used
/// only when no sandbox could resolve `target_path`.
///
/// Tries the plain `remove_dir_all` first so the common (already-writable)
/// case pays no extra syscalls. On `EACCES` - which upstream avoids by
/// proactively chmodding `DEL_NO_UID_WRITE` candidates before ever
/// attempting the unlink (`delete.c:100-101`/`141-142`) - this walks the
/// subtree once, grants owner-write on every directory this process owns
/// but cannot write to, and retries exactly once. Best-effort: a failed
/// chmod during the walk is swallowed and surfaces later as the retry's
/// own `EACCES`.
fn remove_dir_all_with_uid_write_fix(target_path: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(target_path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
            fix_uid_write_recursive(target_path);
            match std::fs::remove_dir_all(target_path) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(err) => Err(err),
            }
        }
        Err(err) => Err(err),
    }
}

/// Grants owner-write on `path` and every directory beneath it that this
/// process owns but cannot write to, mirroring the dirfd-anchored check in
/// [`recursive_unlinkat_inner`]. A symlink is never followed (`is_dir()` on
/// [`std::fs::symlink_metadata`] is `false` for a symlink regardless of its
/// target), matching the dirfd path's `O_NOFOLLOW` descent.
fn fix_uid_write_recursive(path: &Path) {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if !meta.is_dir() {
        return;
    }
    if meta.mode() & S_IWUSR == 0 && effective_uid() != 0 && meta.uid() == effective_uid() {
        let mode = meta.mode() | S_IWUSR;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        fix_uid_write_recursive(&entry.path());
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
/// Returns an [`UnlinkResidue`] describing whether the root was left
/// non-empty (`ENOTEMPTY` on the final `rmdir`) and whether any genuine
/// child unlink/rmdir error was logged and stepped over, so the caller can
/// mirror upstream's post-pass exit-code decision.
///
/// # Errors
///
/// Only hard failures that abort the descent propagate as `Err`: a symlink
/// at the root or a hardlink cycle (`ELOOP`), a non-directory root
/// (`ENOTDIR`), or an unexpected `openat`/`fstatat`/`rmdir` errno. `ENOENT`
/// on the descent root is folded into a clean [`UnlinkResidue`], and a
/// residual `ENOTEMPTY` on the final `rmdir` is reported via
/// [`UnlinkResidue::not_empty`] rather than as an error.
pub fn recursive_unlinkat(parent_dirfd: BorrowedFd<'_>, leaf: &OsStr) -> io::Result<UnlinkResidue> {
    let mut visited: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    recursive_unlinkat_inner(parent_dirfd, leaf, &mut visited)
}

/// Caches the process effective uid (`geteuid(2)` never changes for the
/// life of the process absent a `setuid` call this codebase never makes).
fn effective_uid() -> u32 {
    static EUID: OnceLock<u32> = OnceLock::new();
    *EUID.get_or_init(|| {
        // SAFETY: `geteuid(2)` is a pure accessor with no arguments and no
        // failure mode.
        #[allow(unsafe_code)]
        unsafe {
            libc::geteuid()
        }
    })
}

/// Reports whether `meta` needs the upstream `DEL_NO_UID_WRITE` chmod: it
/// lacks the owner-write bit, we own it, and we are not root (root can
/// always write regardless of the mode bit, so upstream never bothers).
///
/// # Upstream Reference
///
/// - `delete.c:100` / `generator.c:342` -
///   `!(fp->mode & S_IWUSR) && !am_root && fp->flags & FLAG_OWNED_BY_US`
/// - `flist.c:1513-1514` - `FLAG_OWNED_BY_US` is set when
///   `am_generator && st.st_uid == our_uid`.
fn needs_uid_write_fix(meta: &AtMetadata) -> bool {
    meta.mode() & S_IWUSR == 0 && effective_uid() != 0 && meta.uid() == effective_uid()
}

/// Inner recursive walker shared by the public entry point and the
/// per-entry subdir recursion. Threads the cycle-detection set through
/// each descent level so a `(dev, ino)` we have already entered aborts
/// the recursion with `ELOOP`.
fn recursive_unlinkat_inner(
    parent_dirfd: BorrowedFd<'_>,
    leaf: &OsStr,
    visited: &mut std::collections::HashSet<(u64, u64)>,
) -> io::Result<UnlinkResidue> {
    // Step 1: open the descent root with O_DIRECTORY | O_NOFOLLOW so a
    // symlink at the leaf is refused (`ELOOP`) rather than followed.
    let listing_handle = match openat(
        parent_dirfd,
        leaf,
        libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_RDONLY | libc::O_CLOEXEC,
        0,
    ) {
        Ok(file) => file,
        Err(err) if err.raw_os_error() == Some(libc::ENOENT) => {
            return Ok(UnlinkResidue::default());
        }
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

    // upstream: delete.c:100-101 `delete_dir_contents()` chmods a doomed
    // directory it owns but cannot write to (mode lacks S_IWUSR) before
    // descending into it, and delete.c:141-142 `delete_item()` does the
    // same for the top-level candidate before recursing. This function
    // serves both call sites (it is entered once for the top-level
    // directory and again for every nested subdirectory), so a single
    // check here covers both. The result is ignored, matching upstream's
    // unchecked `do_chmod_at()` call - a failed chmod just means the
    // subsequent unlinks fail with their own `EACCES`, same as if this
    // were never attempted. Only directories are load-bearing: unlink(2)
    // permission is governed by the *containing* directory's mode, not
    // the removed entry's own mode, so a read-only file being deleted
    // does not need this fix (upstream's identical file-side chmod is a
    // no-op under POSIX unlink semantics). `leaf` is always a directory
    // here - the `openat` above used `O_DIRECTORY` and would have failed
    // with `ENOTDIR` otherwise.
    if needs_uid_write_fix(&leaf_meta) {
        let _ = fchmodat(parent_dirfd, leaf, leaf_meta.mode() | S_IWUSR, false);
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
    let mut had_errors = false;
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
            had_errors |= recursive_unlinkat_inner(dirfd, &name, visited)?.had_errors;
        } else {
            had_errors |= unlink_child_entry(dirfd, &name)?;
        }
    }

    // Step 4: close the descent dirfd before issuing rmdir against the
    // parent. Some filesystems (notably NFS) reject `unlinkat(.., AT_REMOVEDIR)`
    // while the target is still open through a separate fd.
    drop(dir_handle);

    // Step 5: rmdir the now-empty directory. ENOENT is idempotent success
    // (matches upstream `delete_item` line 201-206). ENOTEMPTY means an
    // entry survived the peel - a stepped-over `EACCES`, a concurrent
    // writer, or filtered content - and is reported via
    // `UnlinkResidue::not_empty` (upstream `DR_NOT_EMPTY`) rather than as an
    // error; any other errno aborts the descent verbatim.
    let not_empty = match unlinkat(parent_dirfd, leaf, UnlinkFlags::Dir) {
        Ok(()) => false,
        Err(err) if err.raw_os_error() == Some(libc::ENOENT) => false,
        Err(err) if matches!(err.raw_os_error(), Some(libc::ENOTEMPTY | libc::EEXIST)) => true,
        Err(err) => return Err(err),
    };
    Ok(UnlinkResidue {
        not_empty,
        had_errors,
    })
}

/// Remove a single non-directory child entry, retrying the TOCTOU
/// classify-vs-act race once with [`UnlinkFlags::Dir`] when the kernel
/// reports a swapped-to-directory entry (`EISDIR` on Linux, `EPERM`
/// elsewhere). `EACCES` is logged and stepped over per upstream
/// `delete.c:48-176`; `ENOENT` is treated as idempotent success since
/// the entry already vanished.
///
/// Returns `Ok(true)` when a genuine child error (`EACCES`) was logged and
/// stepped over, so the caller can raise [`UnlinkResidue::had_errors`] and
/// mirror upstream's `FERROR_XFER` + `io_error |= IOERR_GENERAL` while the
/// descent keeps deleting siblings. `Ok(false)` covers a clean unlink, an
/// already-vanished entry, and the benign classify-vs-act TOCTOU race.
fn unlink_child_entry(dirfd: BorrowedFd<'_>, name: &OsStr) -> io::Result<bool> {
    match unlinkat(dirfd, name, UnlinkFlags::File) {
        Ok(()) => Ok(false),
        Err(err) => match err.raw_os_error() {
            Some(libc::ENOENT) => Ok(false),
            Some(libc::EISDIR | libc::EPERM) => match unlinkat(dirfd, name, UnlinkFlags::Dir) {
                Ok(()) => Ok(false),
                Err(retry) => match retry.raw_os_error() {
                    Some(libc::ENOENT | libc::ENOTEMPTY) => {
                        tracing::debug!(
                            target: "fast_io::dir_sandbox",
                            name = ?name,
                            os_error = retry.raw_os_error(),
                            "recursive_unlinkat: classify-vs-act race left entry unremovable, stepping over"
                        );
                        Ok(false)
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
                Ok(true)
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
    use std::os::fd::{FromRawFd, IntoRawFd};

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
