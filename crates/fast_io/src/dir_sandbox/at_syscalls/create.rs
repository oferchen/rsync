//! create-class SEC-1.h cutover: `mkdirat`, `symlinkat`, `linkat`.
//!
//! Each primitive anchors the new entry on a parent dirfd so a TOCTOU
//! swap on a mid-path component cannot redirect the create to an
//! attacker-chosen parent. The `*_via_sandbox_or_fallback` adaptors
//! pick the sandbox fast path for single-component leaves and fall back
//! to the path-based `std`/`fast_io` entry points otherwise.

use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::lstat::single_component_leaf;
use super::nested::{ParentAnchor, anchor_parent};

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
/// - In every other case the helper applies upstream's three-arm
///   `do_mkdir_at()` contract to `dir_path` through
///   [`ConfinedFallback`](crate::ConfinedFallback), so a foreign-owned symlink
///   on the parent chain is refused instead of followed.
///
/// # Errors
///
/// Surfaces either the [`mkdirat`] error or, on the
/// [`ConfinedFallback`](crate::ConfinedFallback) tail, that op's error
/// verbatim - including the ownership walk's refusal.
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
    if let ParentAnchor::Anchored { dirfd, name } =
        anchor_parent(sandbox, dest_dir, relative_path, dir_path)?
    {
        return mkdirat(dirfd.as_fd(), name, mode);
    }
    crate::ConfinedFallback::confined().mkdir_at(dir_path, mode)
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
/// - In every other case the helper applies upstream's three-arm
///   `do_symlink_at()` contract to `link_path` through
///   [`ConfinedFallback`](crate::ConfinedFallback), so a foreign-owned symlink
///   on the parent chain is refused instead of followed.
///
/// # Errors
///
/// Surfaces either the [`symlinkat`] error or, on the
/// [`ConfinedFallback`](crate::ConfinedFallback) tail, that op's error
/// verbatim - including the ownership walk's refusal.
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
    if let ParentAnchor::Anchored { dirfd, name } =
        anchor_parent(sandbox, dest_dir, relative_path, link_path)?
    {
        return symlinkat(target, dirfd.as_fd(), name);
    }
    crate::ConfinedFallback::confined().symlink_at(target, link_path)
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
///   [`fast_io::hard_link`](crate::hard_link), a direct `linkat(2)`
///   syscall that preserves [`std::fs::hard_link`] error semantics
///   (`EXDEV`, `EPERM`, ...).
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
    if let ParentAnchor::Anchored { dirfd, name } =
        anchor_parent(sandbox, dest_dir, new_relative, new_path)?
    {
        // Same shape as the single-component branch: the new parent is
        // pinned by the RESOLVE_BENEATH-resolved dirfd, the leader stays
        // on `AT_FDCWD` because it may live outside `dest_dir`.
        let leader_c = CString::new(leader_path.as_os_str().as_bytes())
            .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        let new_c = CString::new(name.as_bytes())
            .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        // SAFETY:
        // - `dirfd.as_fd()` outlives the syscall (owned by `dirfd`).
        // - Both C strings are valid NUL-terminated and borrowed for
        //   the duration of the call.
        // - `flags == 0` matches the standard rsync hardlink shape.
        #[allow(unsafe_code)]
        let rc = unsafe {
            libc::linkat(
                libc::AT_FDCWD,
                leader_c.as_ptr(),
                dirfd.as_fd().as_raw_fd(),
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

/// Publish an anonymous `O_TMPFILE` inode at `dest`, confining `dest`'s parent
/// beneath `root`.
///
/// This is the `linkat(2)` counterpart of
/// [`confined_rename`](super::rename::confined_rename). The `O_TMPFILE` write
/// strategy stages the payload in an unnamed inode and gives it a name only at
/// commit, so `linkat` - not `rename` - is the operation that publishes the
/// file. It therefore needs the same confinement: without it a destination
/// parent flipped to a symlink between the decision to commit and the syscall
/// redirects the transferred file outside the tree, which is the escape
/// upstream's `rename-fullpath-symlink-race` test demonstrates.
///
/// Only the destination is anchored. The source is the `/proc/self/fd/N` magic
/// link naming the staged inode - not a tree path - and `AT_SYMLINK_FOLLOW` is
/// required for the kernel to resolve it, so confining it is meaningless.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/syscall.c:676` `do_link_at()` - resolves the parent of each
///   side and issues `linkat` against the resulting dirfd.
/// - `rsync-3.5.0/syscall.c:2891` `ds_descend()` - the per-component walk that
///   refuses an absolute symlink target.
///
/// # Errors
///
/// - `ELOOP` when a component of the destination parent is a refused symlink
///   or a climb above `root`. Deliberately **not** `EXDEV`, which callers treat
///   as cross-device and answer with a copy+remove fallback.
/// - Otherwise the `linkat(2)` errno verbatim, including `EEXIST` when `dest`
///   already exists and a genuine `EXDEV`.
#[cfg(unix)]
pub fn confined_link_anonymous(staged: BorrowedFd<'_>, root: &Path, dest: &Path) -> io::Result<()> {
    use super::rename::anchor_confined_endpoint;

    let proc_path = format!("/proc/self/fd/{}", staged.as_raw_fd());
    let c_old = CString::new(proc_path).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;

    let endpoint = anchor_confined_endpoint(root, dest)?;
    let (new_dirfd, new_name) = endpoint.resolved();
    let c_new = CString::new(new_name.as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;

    // SAFETY:
    // - `c_old`/`c_new` are valid NUL-terminated C strings borrowed for the
    //   duration of the call; the kernel does not retain the pointers.
    // - `new_dirfd` is either the walked parent (kept open by `endpoint`) or
    //   `AT_FDCWD`; both outlive the syscall.
    // - `AT_SYMLINK_FOLLOW` is required so the kernel resolves the
    //   `/proc/self/fd/N` magic link to the staged inode.
    #[allow(unsafe_code)]
    let rc = unsafe {
        libc::linkat(
            libc::AT_FDCWD,
            c_old.as_ptr(),
            new_dirfd.as_raw_fd(),
            c_new.as_ptr(),
            libc::AT_SYMLINK_FOLLOW,
        )
    };

    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Create `dest` exclusively, with its parent confined beneath `root`.
///
/// This is the direct-write counterpart of [`confined_rename`](super::confined_rename)
/// and [`confined_link_anonymous`]: the parent is resolved by the same
/// per-component walk through `anchor_confined_endpoint`, then the leaf is
/// created with `O_CREAT | O_EXCL | O_NOFOLLOW` relative to that dirfd. A
/// destination parent flipped to a symlink between the decision to write and
/// the create therefore cannot redirect the new file out of the tree.
///
/// Upstream never needs this primitive because `receiver.c` always stages into
/// a `.name.XXXXXX` temp and commits with a rename, so its confinement comes
/// from `do_rename_at()`. oc's direct-write strategy skips that staging as an
/// optimisation for a not-yet-existing destination, which drops the invariant
/// unless the create is confined here.
///
/// `O_EXCL` is retained from the unconfined path: it is what makes a
/// concurrent writer lose the race with `EEXIST` rather than silently share
/// the file. The creation mode is `0o666` for the same reason - that is what
/// `std::fs::OpenOptions` passes, so confining the create does not also change
/// the mode a `--no-perms` copy leaves behind.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/syscall.c:2891` `ds_descend()` - the per-component walk.
/// - `rsync-3.5.0/receiver.c` - upstream's always-stage-then-rename shape.
///
/// # Errors
///
/// - `ELOOP` when a parent component is a refused symlink (an absolute target,
///   or one climbing above the anchor). Deliberately not `EXDEV`, which
///   callers treat as cross-device and retry by another route.
/// - `EEXIST` when `dest` already exists.
/// - Otherwise the underlying `openat(2)` error verbatim.
pub fn confined_create_new(root: &Path, dest: &Path) -> io::Result<std::fs::File> {
    use super::rename::anchor_confined_endpoint;
    use std::os::fd::FromRawFd;

    let endpoint = anchor_confined_endpoint(root, dest)?;
    let (dirfd, name) = endpoint.resolved();
    let c_name =
        CString::new(name.as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;

    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY:
    // - `c_name` is a valid NUL-terminated C string borrowed for the duration
    //   of the call; the kernel does not retain the pointer.
    // - `dirfd` is either the walked parent (kept open by `endpoint`) or
    //   `AT_FDCWD`; both outlive the syscall.
    // - The returned descriptor is owned here and handed to `File`, which
    //   closes it exactly once.
    #[allow(unsafe_code)]
    let fd = unsafe { libc::openat(dirfd.as_raw_fd(), c_name.as_ptr(), flags, 0o666) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    #[allow(unsafe_code)]
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

/// Outcome of an anchored copy-on-write clone attempt.
///
/// Distinguishes "the filesystem will not reflink" - a routine condition the
/// caller answers by falling back to a data copy - from a confinement refusal,
/// which is an error and must never degrade to an unconfined write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneAttempt {
    /// A true zero-copy reflink was created at the anchored destination.
    Cloned,
    /// The platform or filesystem cannot reflink here (not APFS/Btrfs/XFS,
    /// cross-device, unsupported kernel). Nothing was created.
    Unsupported,
}

/// Clone `src` to `dest` with `dest`'s parent confined beneath `root`.
///
/// The copy-on-write fast path is the one destination sink that cannot be
/// confined by anchoring the *commit*: a reflink both creates and populates
/// the destination in a single syscall, so the create IS the confinement
/// decision. Anchoring the parent and cloning through that dirfd is therefore
/// the only race-free form - a check-then-`clonefile` gate would still lose to
/// a parent flipped between the two.
///
/// Upstream has no counterpart: `receiver.c` always stages into a
/// `.name.XXXXXX` temp and commits with a rename, so it never reflinks onto
/// the final name and inherits `do_rename_at()`'s confinement. This primitive
/// exists so oc's CoW optimisation keeps the same invariant.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/syscall.c:2891` `ds_descend()` - the per-component walk.
///
/// # Errors
///
/// - `ELOOP` when a parent component is a refused symlink (an absolute target,
///   or one climbing above the anchor). The caller must propagate this, not
///   fall back to a path-based copy.
/// - `EEXIST` when `dest` already exists - the same race semantics `O_EXCL`
///   gives the plain create.
/// - Any other error from anchoring `dest`'s parent.
///
/// A filesystem that simply cannot reflink reports `Ok(CloneAttempt::Unsupported)`,
/// never an error.
///
/// # Source confinement
///
/// `source` arrives as an ALREADY-OPEN descriptor, and that is load-bearing:
/// the caller's confined open is what resolved it, so the source side inherits
/// exactly the walk `open_source_confined` performs. Taking a `&Path` here
/// instead would make this function re-resolve the source with the libc
/// resolver, which follows every parent component - the caller's confinement
/// would be bypassed rather than inherited, and a parent flipped to a symlink
/// pointing outside the transfer root would be followed. That is not
/// hypothetical: it is the defect this signature exists to prevent.
pub fn confined_clone_file(
    root: &Path,
    source: &std::fs::File,
    dest: &Path,
) -> io::Result<CloneAttempt> {
    use super::rename::anchor_confined_endpoint;

    let endpoint = anchor_confined_endpoint(root, dest)?;
    let (dirfd, name) = endpoint.resolved();
    let c_name =
        CString::new(name.as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;

    clone_at(source, dirfd, &c_name)
}

/// macOS: `fclonefileat(2)` publishes the clone under the anchored dirfd.
///
/// The source is passed as an already-open descriptor, so the flag that
/// controls source-symlink following (`CLONE_NOFOLLOW`) is not applicable;
/// whether the source may be a symlink is the source-confinement decision made
/// by the caller before it opens the file.
#[cfg(target_os = "macos")]
fn clone_at(
    source: &std::fs::File,
    dirfd: BorrowedFd<'_>,
    name: &CString,
) -> io::Result<CloneAttempt> {
    // SAFETY: `source` is borrowed for the call and outlives it; `dirfd` is the
    // walked parent held by the caller's endpoint; `name` is a valid
    // NUL-terminated string borrowed for the duration of the call.
    #[allow(unsafe_code)]
    let rc = unsafe {
        libc::fclonefileat(
            source.as_raw_fd(),
            dirfd.as_raw_fd(),
            name.as_ptr(),
            0, /* flags */
        )
    };
    if rc == 0 {
        return Ok(CloneAttempt::Cloned);
    }
    let error = io::Error::last_os_error();
    classify_clone_error(error)
}

/// Linux: create the destination confined, then reflink into it with `FICLONE`.
///
/// Unlike macOS there is no single anchored clone syscall, so the two halves
/// are composed: [`confined_create_new`] provides the anchored, `O_EXCL` inode
/// and the ioctl fills it. A failed ioctl must not leave the empty inode
/// behind - the caller is entitled to treat `Unsupported` as "nothing was
/// created" and fall through to a data copy that expects to create the file.
#[cfg(target_os = "linux")]
fn clone_at(
    source: &std::fs::File,
    dirfd: BorrowedFd<'_>,
    name: &CString,
) -> io::Result<CloneAttempt> {
    use std::os::fd::FromRawFd;

    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: `dirfd` outlives the call and `name` is a valid NUL-terminated
    // string; the returned descriptor is owned here and closed exactly once by
    // the `File` below.
    #[allow(unsafe_code)]
    let fd = unsafe { libc::openat(dirfd.as_raw_fd(), name.as_ptr(), flags, 0o666) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    #[allow(unsafe_code)]
    let destination = unsafe { std::fs::File::from_raw_fd(fd) };

    // SAFETY: both descriptors are owned and open for the duration of the
    // ioctl; `FICLONE` takes the source fd by value, not a pointer.
    #[allow(unsafe_code)]
    let rc = unsafe {
        libc::ioctl(
            destination.as_raw_fd(),
            libc::FICLONE,
            source.as_raw_fd() as libc::c_int,
        )
    };
    if rc == 0 {
        return Ok(CloneAttempt::Cloned);
    }
    let error = io::Error::last_os_error();
    drop(destination);
    // Remove the empty inode the failed reflink left anchored under `dirfd`,
    // so `Unsupported` really does mean "nothing was created".
    // SAFETY: `dirfd` and `name` are still valid; failure is ignored because
    // the reflink error is the one worth reporting.
    #[allow(unsafe_code)]
    unsafe {
        libc::unlinkat(dirfd.as_raw_fd(), name.as_ptr(), 0);
    }
    classify_clone_error(error)
}

/// Platforms with no anchored reflink primitive never take the fast path.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn clone_at(src: &Path, dirfd: BorrowedFd<'_>, name: &CString) -> io::Result<CloneAttempt> {
    let _ = (src, dirfd, name);
    Ok(CloneAttempt::Unsupported)
}

/// Split "this filesystem will not reflink" from a real failure.
///
/// Only the former may fall back to a data copy. Anything else - notably a
/// confinement refusal, which surfaces before this point - propagates.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn classify_clone_error(error: io::Error) -> io::Result<CloneAttempt> {
    match error.raw_os_error() {
        // ENOTSUP/EOPNOTSUPP: filesystem has no reflink. EXDEV: different
        // filesystems. EINVAL: same-file or an unsupported combination.
        // ENOSYS: kernel too old for the ioctl.
        Some(code)
            if code == libc::ENOTSUP
                || code == libc::EOPNOTSUPP
                || code == libc::EXDEV
                || code == libc::EINVAL
                || code == libc::ENOSYS =>
        {
            Ok(CloneAttempt::Unsupported)
        }
        _ => Err(error),
    }
}
