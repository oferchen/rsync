//! rename-class SEC-1.j cutover: `renameat`, optionally upgraded to
//! `renameat2(RENAME_NOREPLACE)` on Linux 3.15+.
//!
//! [`renameat`] anchors both endpoints on their respective dirfds so a
//! mid-syscall symlink swap on either leaf cannot redirect the rename.
//! [`renameat_via_sandbox_or_fallback`] drives the receiver temp-file →
//! final-name commit, selecting the sandbox fast path when both leaves
//! are single components under the same destination parent.

use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::lstat::single_component_leaf;
use super::nested::{ParentAnchor, anchor_parent};

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
    sandbox: Option<&crate::dir_sandbox::DirSandbox>,
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
    // Nested paths: anchor each endpoint's parent under RESOLVE_BENEATH.
    // Both must anchor for the rename to be confined; if either endpoint
    // is single-component or anchoring is unavailable we drop to the
    // path-based fallback rather than mixing an anchored and an ambient
    // endpoint (which would leave one side re-resolvable).
    if sandbox.is_some() {
        let old_anchor = anchor_parent(sandbox, old_dest_dir, old_relative_path, old_link_path)?;
        let new_anchor = anchor_parent(sandbox, new_dest_dir, new_relative_path, new_link_path)?;
        if let (
            ParentAnchor::Anchored {
                dirfd: old_dirfd,
                name: old_leaf,
            },
            ParentAnchor::Anchored {
                dirfd: new_dirfd,
                name: new_leaf,
            },
        ) = (old_anchor, new_anchor)
        {
            return renameat(
                old_dirfd.as_fd(),
                old_leaf,
                new_dirfd.as_fd(),
                new_leaf,
                replace,
            );
        }
    }
    std::fs::rename(old_link_path, new_link_path)
}
