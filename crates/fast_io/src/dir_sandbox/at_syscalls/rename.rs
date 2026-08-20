//! rename-class SEC-1.j cutover: `renameat`, optionally upgraded to
//! `renameat2(RENAME_NOREPLACE)` on Linux 3.15+.
//!
//! [`renameat`] anchors both endpoints on their respective dirfds so a
//! mid-syscall symlink swap on either leaf cannot redirect the rename.
//! [`renameat_via_sandbox_or_fallback`] drives the receiver temp-file →
//! final-name commit, selecting the sandbox fast path when both leaves
//! are single components under the same destination parent.

use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::lstat::single_component_leaf;

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
    // Nested paths: resolve each side independently. An endpoint under the root
    // is anchored by the per-component confined walk; an absolute one outside it
    // (an operator-supplied `--temp-dir`/`--partial-dir`) by the ownership walk.
    //
    // Anchoring is deliberately NOT all-or-nothing. Requiring both sides to
    // anchor let an absolute `--temp-dir` source - which can never anchor -
    // disable confinement of the *destination*, so `finish_transfer`'s
    // tmp->final rename followed a flipped `dest/sub` symlink and wrote
    // outside the tree. That is the escape upstream's
    // `rename-fullpath-symlink-race` test demonstrates.
    //
    // upstream: `rsync-3.5.0/syscall.c:1918-1923` `do_rename_at()` - "Confine
    // each side independently. [...] Doing each side independently means an
    // absolute source never disables confinement of a relative destination."
    if sandbox.is_some() {
        return confined_rename(old_dest_dir, old_link_path, new_link_path, replace);
    }
    std::fs::rename(old_link_path, new_link_path)
}

/// One endpoint of a confined commit, already reduced to a dirfd plus a name to
/// pass to the `*at` syscall.
///
/// The [`Anchored`](Self::Anchored) arm owns the walked [`DirSandbox`], so the
/// parent descriptor stays open for the duration of the syscall.
///
/// Shared by [`confined_rename`] and by the `O_TMPFILE` `linkat(2)` commit
/// ([`super::create::confined_link_anonymous`]): both publish a staged file
/// under its final name, so both must anchor that name the same way. Keeping
/// one owner of the rule is what stops the two commit strategies drifting -
/// the Linux `O_TMPFILE` path silently skipped confinement while the named-temp
/// rename enforced it, so the same escape was open on one platform only.
#[cfg(unix)]
pub(super) enum ConfinedEndpoint<'a> {
    /// The endpoint lives beneath the confinement root; `sandbox` holds the
    /// walked parent and `leaf` is the final component.
    Anchored {
        sandbox: crate::dir_sandbox::DirSandbox,
        leaf: &'a OsStr,
    },
    /// The endpoint is an **operator** path outside the root (an absolute
    /// `--temp-dir`/`--partial-dir`, say). Its parent is resolved by the
    /// ownership walk, which follows uid-0/euid symlinks and refuses any
    /// other-uid one, so a foreign-owned component flipped between the decision
    /// to commit and the syscall cannot redirect the operation.
    OwnerWalked { parent: OwnedFd, leaf: OsString },
    /// The endpoint is neither beneath the root nor absolute - a bare relative
    /// name. Resolved against `AT_FDCWD` from the whole path, exactly as a plain
    /// `rename(2)` would, mirroring upstream's slashless arm.
    Ambient(&'a OsStr),
}

#[cfg(unix)]
impl ConfinedEndpoint<'_> {
    /// Reduce the endpoint to the `(dirfd, name)` pair every `*at` syscall wants.
    ///
    /// The single owner of that mapping. Each consumer having its own `match`
    /// is what let them drift once already - the `O_TMPFILE` `linkat` commit
    /// skipped confinement while the named-temp rename enforced it - so a new
    /// arm must be impossible to handle in only some of them.
    pub(super) fn resolved(&self) -> (BorrowedFd<'_>, &OsStr) {
        match self {
            Self::Anchored { sandbox, leaf } => (sandbox.root_dirfd(), leaf),
            Self::OwnerWalked { parent, leaf } => (parent.as_fd(), leaf.as_os_str()),
            // SAFETY: `AT_FDCWD` is a well-known pseudo-descriptor accepted by
            // every `*at` syscall; it is never closed and outlives any borrow.
            #[allow(unsafe_code)]
            Self::Ambient(path) => (unsafe { BorrowedFd::borrow_raw(libc::AT_FDCWD) }, path),
        }
    }
}

/// Reduce one endpoint to `(dirfd, name)` for `renameat(2)`.
///
/// Three arms, mirroring upstream's three:
///
/// | endpoint | resolver | upstream |
/// |---|---|---|
/// | beneath `root` | confined per-component walk | `secure_relative_open()`, `syscall.c:1945` |
/// | absolute, outside `root` | ownership walk | `owner_walk_parent()`, `syscall.c:1926` |
/// | otherwise | `AT_FDCWD` + whole path | `syscall.c:1949` |
///
/// The split matters because location cannot be the trust signal for an
/// operator path - it may legitimately point outside the tree - so authority
/// (ownership) is used instead. Keying the first arm on "beneath `root`" rather
/// than on "relative" is oc's equivalent of upstream's test: upstream has
/// already `chdir`'d the receiver into the destination, so its transfer paths
/// are relative where oc's are absolute-but-beneath-root.
///
/// Deliberately **not** applied to the destination of a transfer: the ownership
/// rule is more permissive than the confined walk, so widening it to a path
/// beneath `root` would weaken that side rather than strengthen this one.
#[cfg(unix)]
pub(super) fn anchor_confined_endpoint<'a>(
    root: &Path,
    path: &'a Path,
) -> io::Result<ConfinedEndpoint<'a>> {
    use crate::dir_sandbox::{ConfinePolicy, DirSandbox, NoExclude};

    let Ok(relative) = path.strip_prefix(root) else {
        if path.is_absolute() {
            let (parent, leaf) = crate::owner_walk::owner_trusted_parent(path)?;
            return Ok(ConfinedEndpoint::OwnerWalked { parent, leaf });
        }
        return Ok(ConfinedEndpoint::Ambient(path.as_os_str()));
    };
    let Some(leaf) = relative.file_name() else {
        return Ok(ConfinedEndpoint::Ambient(path.as_os_str()));
    };
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    let sandbox =
        DirSandbox::open_dest_anchor_confined(root, parent, ConfinePolicy::confined(NoExclude))?;
    Ok(ConfinedEndpoint::Anchored { sandbox, leaf })
}

/// Rename `old_path` to `new_path`, resolving **each side independently** by
/// the resolver its provenance calls for (see [`anchor_confined_endpoint`]).
///
/// A side beneath `root` has its parent resolved by the confined per-component
/// walk; an absolute side outside `root` - an operator path - by the ownership
/// walk. Either way no component can be flipped to a symlink between the
/// decision to commit and the syscall and redirect the rename out of the tree.
///
/// Per-side independence is the whole point, in both directions: an
/// operator-supplied absolute `--temp-dir` puts the *source* outside the tree,
/// and an all-or-nothing rule would let that disable confinement of the
/// *destination* - the escape upstream's `rename-fullpath-symlink-race` test
/// demonstrates. Leaving that source unresolved is the mirror-image escape,
/// which `temp-dir-symlink-injection` demonstrates: the temp file's *creation*
/// is confined, but re-resolving its path at commit time lets a flipped
/// foreign-owned parent substitute attacker content into the destination.
///
/// `replace` mirrors the [`renameat`] knob: `true` overwrites the destination
/// atomically, matching [`std::fs::rename`] and upstream `do_rename()`.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/syscall.c:1866` `do_rename_at()` - "Confine each side
///   independently. [...] Doing each side independently means an absolute
///   source never disables confinement of a relative destination."
/// - `rsync-3.5.0/syscall.c:2891` `ds_descend()` - the per-component walk.
///
/// # Errors
///
/// - `ELOOP` when a component of either confined side is a refused symlink (an
///   absolute target, or one landing outside the anchor) or a climb above the
///   anchor. Deliberately **not** `EXDEV`: callers treat `EXDEV` as
///   cross-device and fall back to copy+remove, which would defeat the refusal.
/// - Otherwise the `renameat(2)` error verbatim, including a genuine `EXDEV`.
#[cfg(unix)]
pub fn confined_rename(
    root: &Path,
    old_path: &Path,
    new_path: &Path,
    replace: bool,
) -> io::Result<()> {
    let old = anchor_confined_endpoint(root, old_path)?;
    let new = anchor_confined_endpoint(root, new_path)?;

    let (old_dirfd, old_name) = old.resolved();
    let (new_dirfd, new_name) = new.resolved();

    renameat(old_dirfd, old_name, new_dirfd, new_name, replace)
}
