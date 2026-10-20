//! Anchored copy-on-write clone: macOS `fclonefileat(2)`, Linux `FICLONE`.
//!
//! The clone both creates and populates the destination in one syscall, so
//! the destination parent is anchored first and the clone is published
//! through that dirfd. Platforms without a reflink primitive report
//! [`CloneAttempt::Unsupported`] and the caller falls back to a data copy.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

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

/// Timestamps a clone takes from its own creation instead of the source.
///
/// A newly written file carries creation-time stamps, and upstream replaces
/// each one from the source only under its option: mtime under `--times`,
/// atime under `--atimes`, crtime under `--crtimes`. macOS `clonefile(2)`
/// copies all three from the source, so every stamp the transfer does not
/// preserve is reset to "now" before the clone is handed back.
///
/// upstream: `rsync.c:723-765` `set_file_attrs()` - each timestamp is taken
/// from the source only under its option.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CloneFreshTimes {
    /// Reset the creation time.
    pub crtime: bool,
    /// Reset the modification time.
    pub mtime: bool,
    /// Reset the access time.
    pub atime: bool,
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
/// - `rsync-3.5.1/syscall.c:3032` `ds_descend()` - the per-component walk.
///
/// # Errors
///
/// - `ELOOP` when a parent component is a refused symlink (an absolute target,
///   or one climbing above the anchor). The caller must propagate this, not
///   fall back to a path-based copy.
/// - `EEXIST` when `dest` already exists - the same race semantics `O_EXCL`
///   gives the plain create.
/// - Any other error from anchoring `dest`'s parent.
/// - Any error resetting a `fresh` stamp. The clone is removed first, so an
///   error never leaves a destination carrying the source's timestamps.
///
/// A filesystem that simply cannot reflink reports `Ok(CloneAttempt::Unsupported)`,
/// never an error.
///
/// # Timestamps
///
/// Each stamp `fresh` selects is reset through the same anchored parent the
/// clone was published under, so the reset cannot be redirected either. On
/// Linux `FICLONE` fills an inode this call has just created, whose stamps are
/// already the creation's, so there is nothing to reset.
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
    fresh: CloneFreshTimes,
) -> io::Result<CloneAttempt> {
    use super::rename::anchor_confined_endpoint;

    let endpoint = anchor_confined_endpoint(root, dest)?;
    let (dirfd, name) = endpoint.resolved();
    let c_name =
        CString::new(name.as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;

    clone_at(source, dirfd, &c_name, fresh)
}

/// macOS: `fclonefileat(2)` publishes the clone under the anchored dirfd.
///
/// The source is passed as an already-open descriptor, so the flag that
/// controls source-symlink following (`CLONE_NOFOLLOW`) is not applicable;
/// whether the source may be a symlink is the source-confinement decision made
/// by the caller before it opens the file.
///
/// No `CLONE_ACL`: without it the clone takes the target directory's
/// inheritable ACLs, as an `openat(2)`-created file does, and `-A` applies the
/// source ACL afterwards. xnu: `bsd/vfs/vfs_syscalls.c` `clonefile_internal()`
/// requests the source `va_acl` only under `CLONE_ACL`.
#[cfg(target_os = "macos")]
fn clone_at(
    source: &std::fs::File,
    dirfd: BorrowedFd<'_>,
    name: &CString,
    fresh: CloneFreshTimes,
) -> io::Result<CloneAttempt> {
    // SAFETY: `source` is borrowed for the call and outlives it; `dirfd` is the
    // walked parent held by the caller's endpoint; `name` is a valid
    // NUL-terminated string borrowed for the duration of the call.
    #[allow(unsafe_code)]
    let rc = unsafe { libc::fclonefileat(source.as_raw_fd(), dirfd.as_raw_fd(), name.as_ptr(), 0) };
    if rc != 0 {
        return classify_clone_error(io::Error::last_os_error());
    }
    if let Err(error) = stamp_fresh_times(dirfd, name, fresh) {
        // SAFETY: `dirfd` and `name` are still valid; an unlink failure is
        // ignored because the stamp error is the one worth reporting.
        #[allow(unsafe_code)]
        unsafe {
            libc::unlinkat(dirfd.as_raw_fd(), name.as_ptr(), 0);
        }
        return Err(error);
    }
    Ok(CloneAttempt::Cloned)
}

/// Resets each `fresh` stamp on the clone `name` under `dirfd` to the current
/// time with one `setattrlistat(2)`.
///
/// `FSOPT_NOFOLLOW` keeps the update on the clone itself. Upstream's
/// `do_setattrlist_crtime()` is path-based and accepts the parent-symlink race
/// as a residual (`syscall.c:2465-2488`), stating that macOS has no at-aware
/// variant targeting `ATTR_CMN_CRTIME`. oc deliberately goes beyond upstream
/// here: `setattrlistat(2)` is that at-aware variant, so resolving through the
/// walked parent closes the race without an extra syscall.
#[cfg(target_os = "macos")]
fn stamp_fresh_times(
    dirfd: BorrowedFd<'_>,
    name: &CString,
    fresh: CloneFreshTimes,
) -> io::Result<()> {
    let commonattr = [
        (fresh.crtime, libc::ATTR_CMN_CRTIME),
        (fresh.mtime, libc::ATTR_CMN_MODTIME),
        (fresh.atime, libc::ATTR_CMN_ACCTIME),
    ]
    .into_iter()
    .filter(|&(wanted, _)| wanted)
    .fold(0, |bits, (_, bit)| bits | bit);
    if commonattr == 0 {
        return Ok(());
    }
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?;
    let now = libc::timespec {
        tv_sec: libc::time_t::try_from(since_epoch.as_secs())
            .map_err(|_| io::Error::from_raw_os_error(libc::EOVERFLOW))?,
        tv_nsec: libc::c_long::from(since_epoch.subsec_nanos()),
    };
    // setattrlist(2) reads one timespec per requested attribute. Every slot
    // carries the same instant, so the packing order does not matter.
    let mut values = [now; 3];
    let size = commonattr.count_ones() as usize * std::mem::size_of::<libc::timespec>();
    let mut attrlist = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    // SAFETY: `dirfd` is the walked parent held by the caller's endpoint and
    // `name` a valid NUL-terminated string, both borrowed for the call;
    // `attrlist` is a fully initialised `repr(C)` value and `values` holds at
    // least `size` bytes of `timespec`s, both live for the call.
    #[allow(unsafe_code)]
    let rc = unsafe {
        libc::setattrlistat(
            dirfd.as_raw_fd(),
            name.as_ptr(),
            (&raw mut attrlist).cast(),
            values.as_mut_ptr().cast(),
            size,
            libc::FSOPT_NOFOLLOW,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
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
    _fresh: CloneFreshTimes,
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
fn clone_at(
    source: &std::fs::File,
    dirfd: BorrowedFd<'_>,
    name: &CString,
    _fresh: CloneFreshTimes,
) -> io::Result<CloneAttempt> {
    let _ = (source, dirfd, name);
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

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::{CloneAttempt, classify_clone_error};
    use std::io;

    /// "This filesystem will not reflink" is the only outcome allowed to fall
    /// back to a data copy, so each such errno must map to `Unsupported`.
    #[test]
    fn no_reflink_errnos_fall_back_to_a_copy() {
        for code in [
            libc::ENOTSUP,
            libc::EOPNOTSUPP,
            libc::EXDEV,
            libc::EINVAL,
            libc::ENOSYS,
        ] {
            let outcome = classify_clone_error(io::Error::from_raw_os_error(code))
                .unwrap_or_else(|e| panic!("errno {code} must not be an error: {e}"));
            assert_eq!(outcome, CloneAttempt::Unsupported, "errno {code}");
        }
    }

    /// A confinement refusal or a real failure must propagate with its errno:
    /// degrading it to `Unsupported` would let the caller retry the write
    /// through the unconfined copy path.
    #[test]
    fn refusals_and_real_failures_propagate() {
        for code in [
            libc::ELOOP,
            libc::EEXIST,
            libc::EACCES,
            libc::ENOENT,
            libc::ENOSPC,
        ] {
            let error = classify_clone_error(io::Error::from_raw_os_error(code))
                .expect_err("a refusal must not be reported as Unsupported");
            assert_eq!(error.raw_os_error(), Some(code));
        }
    }
}
