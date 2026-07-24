//! Filesystem dispatch trait and implementations for the delete emitter.
//!
//! Hosts the [`DeleteFs`] trait, the production [`RealDeleteFs`] backed
//! by `std::fs`, and the [`RecordingDeleteFs`] test fake. Splitting one
//! method per upstream-distinguishable entry kind (`delete.c:144-176`)
//! lets unit tests assert the exact dispatch table even though all
//! file-like kinds currently route to `unlink(2)` in production.
//!
//! # Sandbox-anchored dispatch (SEC-1.q)
//!
//! Each unlink/rmdir method ships in two shapes:
//!
//! - **Path-based** (`unlink_file`, `rmdir`, ...): the legacy entry
//!   points that walk an absolute path through the kernel. Used when no
//!   [`fast_io::DirSandbox`] has been wired to the emitter, on Windows,
//!   and by the [`RecordingDeleteFs`] test fake which never touches the
//!   filesystem.
//! - **Dirfd-anchored** (`unlink_file_at`, `rmdir_at`, ...,
//!   `#[cfg(unix)]`): the SEC-1.q entry points that resolve a
//!   single-leaf name against a [`BorrowedFd`] for the parent directory.
//!   These close the symlink-swap TOCTOU class on every `--delete`
//!   syscall and route through the SEC-1.g / SEC-1.s sandbox helpers in
//!   [`fast_io::dir_sandbox::at_syscalls`].

use std::fs;
use std::io;
#[cfg(unix)]
use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[cfg(unix)]
use std::ffi::OsStr;

#[cfg(unix)]
use fast_io::{UnlinkFlags, UnlinkResidue};

use super::super::DeleteEntryKind;
use crate::util::poison::lock_or_recover;

/// Filesystem operations the emitter needs to issue a deletion.
///
/// The trait carves one method per upstream-distinguishable entry kind
/// (`delete.c:144-176`). Splitting `unlink_file` from `unlink_symlink` /
/// `unlink_device` / `unlink_special` lets unit tests assert the exact
/// dispatch table even though all four currently route to `unlink(2)` in
/// the production implementation. Directories use `rmdir(2)`; the
/// recursive [`Self::remove_dir_all`] hook mirrors upstream's
/// `delete_dir_contents` fallback when a directory cannot be emptied via
/// its own published plan.
///
/// All methods take `&self` so a single [`DeleteFs`] value can be shared
/// across the emitter and any future helpers. The production impl is
/// stateless; the test fake holds a `Mutex` because the recording is
/// observable from the test thread after `emit_all` returns.
///
/// # Sandbox-anchored siblings (SEC-1.q)
///
/// Every path-based method has a unix-only `*_at` sibling that takes a
/// parent dirfd plus a single-component leaf. When the emitter is built
/// with [`super::DeleteEmitter::with_sandbox`], it dispatches through
/// the `*_at` siblings; otherwise it falls back to the path-based
/// methods. The dirfd-anchored shape pins the parent at receiver setup
/// so a mid-syscall symlink swap on a mid-path component cannot redirect
/// the unlink to an attacker-chosen inode beneath a different parent.
pub trait DeleteFs: std::fmt::Debug {
    /// Unlinks a regular file by absolute path.
    ///
    /// Used in the no-sandbox fallback. The sandbox-anchored sibling
    /// [`Self::unlink_file_at`] closes the symlink-swap TOCTOU class on
    /// the parent walk; prefer it when a [`fast_io::DirSandbox`] is
    /// available.
    fn unlink_file(&self, path: &Path) -> io::Result<()>;

    /// Removes an empty directory by absolute path.
    ///
    /// See [`Self::rmdir_at`] for the sandbox-anchored sibling.
    fn rmdir(&self, path: &Path) -> io::Result<()>;

    /// Unlinks a symbolic link by absolute path.
    ///
    /// See [`Self::unlink_symlink_at`] for the sandbox-anchored sibling.
    fn unlink_symlink(&self, path: &Path) -> io::Result<()>;

    /// Unlinks a block or character device node by absolute path.
    ///
    /// See [`Self::unlink_device_at`] for the sandbox-anchored sibling.
    fn unlink_device(&self, path: &Path) -> io::Result<()>;

    /// Unlinks a FIFO or socket by absolute path.
    ///
    /// See [`Self::unlink_special_at`] for the sandbox-anchored sibling.
    fn unlink_special(&self, path: &Path) -> io::Result<()>;

    /// Recursively removes a directory and everything beneath it by
    /// absolute path.
    ///
    /// Invoked by the emitter when [`Self::rmdir`] returns
    /// [`io::ErrorKind::DirectoryNotEmpty`] and no nested
    /// [`super::super::DeletePlan`] has been published for the offending
    /// child (upstream `delete.c:48-122 delete_dir_contents`).
    ///
    /// See [`Self::remove_dir_all_at`] for the sandbox-anchored sibling.
    fn remove_dir_all(&self, path: &Path) -> io::Result<()>;

    /// Unlinks a regular file via `unlinkat(parent_fd, name, 0)`.
    ///
    /// SEC-1.q sandbox-anchored sibling of [`Self::unlink_file`]. The
    /// leaf is resolved relative to `parent_fd`; a mid-syscall symlink
    /// swap on the parent walk cannot redirect the call because the
    /// parent is pinned by the dirfd that was opened at receiver setup.
    #[cfg(unix)]
    fn unlink_file_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()>;

    /// Removes an empty directory via
    /// `unlinkat(parent_fd, name, AT_REMOVEDIR)`.
    ///
    /// SEC-1.q sandbox-anchored sibling of [`Self::rmdir`].
    #[cfg(unix)]
    fn rmdir_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()>;

    /// Unlinks a symbolic link via `unlinkat(parent_fd, name, 0)`.
    ///
    /// SEC-1.q sandbox-anchored sibling of [`Self::unlink_symlink`].
    #[cfg(unix)]
    fn unlink_symlink_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()>;

    /// Unlinks a device node via `unlinkat(parent_fd, name, 0)`.
    ///
    /// SEC-1.q sandbox-anchored sibling of [`Self::unlink_device`].
    #[cfg(unix)]
    fn unlink_device_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()>;

    /// Unlinks a FIFO or socket via `unlinkat(parent_fd, name, 0)`.
    ///
    /// SEC-1.q sandbox-anchored sibling of [`Self::unlink_special`].
    #[cfg(unix)]
    fn unlink_special_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()>;

    /// Recursively removes a directory anchored on `parent_fd`.
    ///
    /// SEC-1.q sandbox-anchored sibling of [`Self::remove_dir_all`].
    /// Routes through [`fast_io::recursive_unlinkat`] (SEC-1.s) so each
    /// per-entry descent refuses to follow a symlink at the leaf
    /// (`O_DIRECTORY | O_NOFOLLOW`) and the final
    /// `unlinkat(AT_REMOVEDIR)` is anchored on `parent_fd`.
    ///
    /// Returns an [`UnlinkResidue`] so the emitter can mirror upstream's
    /// post-peel exit-code decision: `not_empty` drives the "cannot delete
    /// non-empty directory" notice, `had_errors` drives `io_error |=
    /// IOERR_GENERAL` (exit 23) when the descent stepped over a genuine
    /// child failure.
    #[cfg(unix)]
    fn remove_dir_all_at(
        &self,
        parent_fd: BorrowedFd<'_>,
        name: &OsStr,
    ) -> io::Result<UnlinkResidue>;
}

/// Production [`DeleteFs`] implementation backed by `std::fs` (path
/// fallback) and the `fast_io` `*at` syscall wrappers (sandbox path).
///
/// All file-like kinds route to [`fs::remove_file`] (Unix `unlink(2)`,
/// Windows `DeleteFileW`) on the path fallback. The sandbox path routes
/// every leaf-removal through [`fast_io::unlinkat`] for the non-recursive
/// kinds and [`fast_io::recursive_unlinkat`] for the recursive fallback,
/// mirroring upstream `delete_item` (`delete.c:161-175`): `do_rmdir` for
/// `S_ISDIR`, `robust_unlink` for everything else,
/// `delete_dir_contents` for the recursive peel.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealDeleteFs;

impl DeleteFs for RealDeleteFs {
    fn unlink_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn rmdir(&self, path: &Path) -> io::Result<()> {
        fs::remove_dir(path)
    }

    fn unlink_symlink(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn unlink_device(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn unlink_special(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        fs::remove_dir_all(path)
    }

    #[cfg(unix)]
    fn unlink_file_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
        fast_io::unlinkat(parent_fd, name, UnlinkFlags::File)
    }

    #[cfg(unix)]
    fn rmdir_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
        fast_io::unlinkat(parent_fd, name, UnlinkFlags::Dir)
    }

    #[cfg(unix)]
    fn unlink_symlink_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
        fast_io::unlinkat(parent_fd, name, UnlinkFlags::File)
    }

    #[cfg(unix)]
    fn unlink_device_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
        fast_io::unlinkat(parent_fd, name, UnlinkFlags::File)
    }

    #[cfg(unix)]
    fn unlink_special_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
        fast_io::unlinkat(parent_fd, name, UnlinkFlags::File)
    }

    #[cfg(unix)]
    fn remove_dir_all_at(
        &self,
        parent_fd: BorrowedFd<'_>,
        name: &OsStr,
    ) -> io::Result<UnlinkResidue> {
        // SEC-1.s carrier: drives the recursive peel directly off
        // `parent_fd` with O_DIRECTORY | O_NOFOLLOW so a symlink at the
        // root is refused and the kernel anchors every per-entry
        // `unlinkat` on the descent dirfd.
        fast_io::recursive_unlinkat(parent_fd, name)
    }
}

/// Blanket impl so a shared reference behaves like the owned value. Lets
/// callers reuse a single [`RealDeleteFs`] across many emitter drains
/// without cloning, and matches the `&self` shape of every trait method.
impl<F: DeleteFs + ?Sized> DeleteFs for &F {
    fn unlink_file(&self, path: &Path) -> io::Result<()> {
        (*self).unlink_file(path)
    }

    fn rmdir(&self, path: &Path) -> io::Result<()> {
        (*self).rmdir(path)
    }

    fn unlink_symlink(&self, path: &Path) -> io::Result<()> {
        (*self).unlink_symlink(path)
    }

    fn unlink_device(&self, path: &Path) -> io::Result<()> {
        (*self).unlink_device(path)
    }

    fn unlink_special(&self, path: &Path) -> io::Result<()> {
        (*self).unlink_special(path)
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        (*self).remove_dir_all(path)
    }

    #[cfg(unix)]
    fn unlink_file_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
        (*self).unlink_file_at(parent_fd, name)
    }

    #[cfg(unix)]
    fn rmdir_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
        (*self).rmdir_at(parent_fd, name)
    }

    #[cfg(unix)]
    fn unlink_symlink_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
        (*self).unlink_symlink_at(parent_fd, name)
    }

    #[cfg(unix)]
    fn unlink_device_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
        (*self).unlink_device_at(parent_fd, name)
    }

    #[cfg(unix)]
    fn unlink_special_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
        (*self).unlink_special_at(parent_fd, name)
    }

    #[cfg(unix)]
    fn remove_dir_all_at(
        &self,
        parent_fd: BorrowedFd<'_>,
        name: &OsStr,
    ) -> io::Result<UnlinkResidue> {
        (*self).remove_dir_all_at(parent_fd, name)
    }
}

/// Event captured by [`RecordingDeleteFs`] for each emitter dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteEvent {
    /// Path the emitter passed to [`DeleteFs`].
    pub path: PathBuf,
    /// Which trait method was invoked.
    pub kind: DeleteEntryKind,
}

/// Test fake that records every [`DeleteFs`] dispatch and never touches
/// the filesystem.
///
/// Used by the emitter unit tests to assert ordering invariants without
/// staging real files. The recorded sequence is the ground truth for the
/// "syscall order matches upstream" check that section 9.1 of the design
/// elevates to a release-gating interop test.
///
/// The SEC-1.q `*_at` impls discard `parent_fd` and record only the leaf
/// name so the existing emitter unit tests, which assert on absolute
/// paths, keep working unchanged when the emitter dispatches through the
/// dirfd-anchored siblings (the path is provided by the dispatcher).
#[derive(Debug, Default)]
pub struct RecordingDeleteFs {
    events: Mutex<Vec<DeleteEvent>>,
}

impl RecordingDeleteFs {
    /// Creates an empty recorder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a snapshot of the recorded events in dispatch order.
    ///
    /// The event log is append-only scratch data; a poisoned mutex still
    /// yields a debuggable trace, so recovery via [`lock_or_recover`] is
    /// preferred over aborting the test thread.
    #[must_use]
    pub fn events(&self) -> Vec<DeleteEvent> {
        lock_or_recover(&self.events).clone()
    }

    fn record(&self, path: &Path, kind: DeleteEntryKind) {
        lock_or_recover(&self.events).push(DeleteEvent {
            path: path.to_path_buf(),
            kind,
        });
    }
}

impl DeleteFs for RecordingDeleteFs {
    fn unlink_file(&self, path: &Path) -> io::Result<()> {
        self.record(path, DeleteEntryKind::File);
        Ok(())
    }

    fn rmdir(&self, path: &Path) -> io::Result<()> {
        self.record(path, DeleteEntryKind::Dir);
        Ok(())
    }

    fn unlink_symlink(&self, path: &Path) -> io::Result<()> {
        self.record(path, DeleteEntryKind::Symlink);
        Ok(())
    }

    fn unlink_device(&self, path: &Path) -> io::Result<()> {
        self.record(path, DeleteEntryKind::Device);
        Ok(())
    }

    fn unlink_special(&self, path: &Path) -> io::Result<()> {
        self.record(path, DeleteEntryKind::Special);
        Ok(())
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        // Mirror upstream's recursive peel as a single Dir event so the
        // unit tests can assert "the emitter fell back to recursion for
        // this path".
        self.record(path, DeleteEntryKind::Dir);
        Ok(())
    }

    #[cfg(unix)]
    fn unlink_file_at(&self, _parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
        self.record(Path::new(name), DeleteEntryKind::File);
        Ok(())
    }

    #[cfg(unix)]
    fn rmdir_at(&self, _parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
        self.record(Path::new(name), DeleteEntryKind::Dir);
        Ok(())
    }

    #[cfg(unix)]
    fn unlink_symlink_at(&self, _parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
        self.record(Path::new(name), DeleteEntryKind::Symlink);
        Ok(())
    }

    #[cfg(unix)]
    fn unlink_device_at(&self, _parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
        self.record(Path::new(name), DeleteEntryKind::Device);
        Ok(())
    }

    #[cfg(unix)]
    fn unlink_special_at(&self, _parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
        self.record(Path::new(name), DeleteEntryKind::Special);
        Ok(())
    }

    #[cfg(unix)]
    fn remove_dir_all_at(
        &self,
        _parent_fd: BorrowedFd<'_>,
        name: &OsStr,
    ) -> io::Result<UnlinkResidue> {
        self.record(Path::new(name), DeleteEntryKind::Dir);
        Ok(UnlinkResidue::default())
    }
}
