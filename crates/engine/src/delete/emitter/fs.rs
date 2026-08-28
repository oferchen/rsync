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
//! - **Path-based** (`unlink_file`, `rmdir`, ...): the entry points
//!   that take a whole path rather than a parent dirfd. Used when the
//!   emitter has no dirfd for the plan directory - either no
//!   [`fast_io::DirSandbox`] was wired to it, or the per-plan
//!   `open_dir_at` refused a component - as well as on Windows and by
//!   the [`RecordingDeleteFs`] test fake, which never touches the
//!   filesystem. On unix these resolve through [`DELETE_CONFINEMENT`]
//!   rather than `std::fs`, so the path is walked under upstream's
//!   three-arm fallback contract instead of by the kernel with symlink
//!   following fully enabled.
//! - **Dirfd-anchored** (`unlink_file_at`, `rmdir_at`, ...,
//!   `#[cfg(unix)]`): the SEC-1.q entry points that resolve a
//!   single-leaf name against a [`BorrowedFd`] for the parent directory.
//!   These close the symlink-swap TOCTOU class on every `--delete`
//!   syscall and route through the SEC-1.g / SEC-1.s sandbox helpers in
//!   [`fast_io::dir_sandbox::at_syscalls`].

#[cfg(not(unix))]
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
/// All file-like kinds route to [`std::fs::remove_file`] (Unix `unlink(2)`,
/// Windows `DeleteFileW`) on the path fallback. The sandbox path routes
/// every leaf-removal through [`fast_io::unlinkat`] for the non-recursive
/// kinds and [`fast_io::recursive_unlinkat`] for the recursive fallback,
/// mirroring upstream `delete_item` (`delete.c:161-175`): `do_rmdir` for
/// `S_ISDIR`, `robust_unlink` for everything else,
/// `delete_dir_contents` for the recursive peel.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealDeleteFs;

/// The confinement policy the path-based methods resolve under.
///
/// Every entry the emitter deletes is a peer-named destination entry, so the
/// [`Confined`](fast_io::PathKind::Confined) arm is the whole of it - there is
/// no operator-supplied deletion path to spell [`ancillary`] for.
///
/// # Why upstream's fallback is this wrapper family
///
/// Upstream never falls back to a plain syscall. `delete.c:226` picks
/// `dfd >= 0 ? do_unlink_atfd(dfd, leaf, AT_REMOVEDIR) : do_rmdir_at(fbuf)`,
/// and the file arm at `delete.c:77` falls through to
/// `robust_unlink(fbuf)`, which is `do_unlink_at(fname)` on both arms of its
/// `ETXTBSY` split (`util1.c:545`). Each `do_*_at()` runs the three-arm
/// `owner_walk_parent` contract internally, so the held dirfd is an
/// optimisation and a race-window closure - not the confinement.
/// [`fast_io::ConfinedFallback`] is oc's analogue of that wrapper family, and
/// routing the six onto it is what makes this fallback structurally identical
/// to upstream's.
///
/// # Why routing here cannot weaken the delete path
///
/// The dirfd-anchored `*_at` siblings are the sandbox arm and are UNGATED -
/// deliberately stronger than upstream, which gates its equivalent to daemon
/// mode. This constant does not touch them. It governs only the path-based
/// methods, which the emitter selects whenever it holds no dirfd for the plan
/// directory (`emitter/mod.rs` dispatches `Some(fd) => *_at`,
/// `None => path-based`) and which were bare `std::fs` calls with no
/// confinement of any kind. Arm 1 of the fallback contract IS that prior
/// behaviour, so the routing is monotone: it can only add confinement, never
/// remove any.
///
/// Upstream goes further than "the fallback is confined": it refuses to let a
/// runtime errno decide the arm at all. `open_dir_secure` (`syscall.c:3481`)
/// returns `-1` with **errno deliberately cleared** when hardened resolution is
/// not in effect - its own comment says "return -1 with errno cleared so the
/// caller uses the full-path wrappers" - while a genuine refusal from
/// `secure_relative_open` returns `-1` with a real errno. Two outcomes, told
/// apart by a sentinel rather than by inspecting the failure. `delete.c:136`
/// stores the result and `del_held_dfd()` guards `>= 0`, so either way the
/// caller lands on the confined `do_*_at` family.
///
/// oc's `open_plan_dirfd` discards the errno entirely with `.ok()`, collapsing
/// policy-off, genuine failure, and confinement-refused into one `None`. The
/// errno could not carry the distinction anyway: `open_dir_at` passes
/// `O_DIRECTORY | O_NOFOLLOW`, and on Linux a refused symlink component reports
/// `ENOTDIR` - the same errno an ordinary non-directory component produces.
///
/// `open_plan_dirfd` reaches `None` by TWO routes, and the second is why this
/// routing matters on a daemon. Route one is no sandbox at all. Route two is
/// a sandbox whose per-plan `open_dir_at` fails - and `open_dir_at` walks the
/// plan directory component by component with `O_DIRECTORY | O_NOFOLLOW`, so
/// a planted symlink makes it refuse, `.ok()` discards the refusal, and the
/// delete lands here. Before this routing that meant the confinement working
/// was itself what triggered the unconfined syscall.
///
/// [`ancillary`]: fast_io::ConfinedFallback::ancillary
#[cfg(unix)]
const DELETE_CONFINEMENT: fast_io::ConfinedFallback = fast_io::ConfinedFallback::confined();

impl DeleteFs for RealDeleteFs {
    #[cfg(unix)]
    fn unlink_file(&self, path: &Path) -> io::Result<()> {
        DELETE_CONFINEMENT.unlink_at(path, UnlinkFlags::File)
    }

    #[cfg(not(unix))]
    fn unlink_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    #[cfg(unix)]
    fn rmdir(&self, path: &Path) -> io::Result<()> {
        DELETE_CONFINEMENT.rmdir_at(path)
    }

    #[cfg(not(unix))]
    fn rmdir(&self, path: &Path) -> io::Result<()> {
        fs::remove_dir(path)
    }

    #[cfg(unix)]
    fn unlink_symlink(&self, path: &Path) -> io::Result<()> {
        DELETE_CONFINEMENT.unlink_at(path, UnlinkFlags::File)
    }

    #[cfg(not(unix))]
    fn unlink_symlink(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    #[cfg(unix)]
    fn unlink_device(&self, path: &Path) -> io::Result<()> {
        DELETE_CONFINEMENT.unlink_at(path, UnlinkFlags::File)
    }

    #[cfg(not(unix))]
    fn unlink_device(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    #[cfg(unix)]
    fn unlink_special(&self, path: &Path) -> io::Result<()> {
        DELETE_CONFINEMENT.unlink_at(path, UnlinkFlags::File)
    }

    #[cfg(not(unix))]
    fn unlink_special(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    /// Recursively remove `path`.
    ///
    /// The residue [`remove_dir_all_at`](fast_io::ConfinedFallback::remove_dir_all_at)
    /// returns is dropped here because this trait method mirrors
    /// [`std::fs::remove_dir_all`]'s `()` result. The dirfd-anchored sibling
    /// [`Self::remove_dir_all_at`] is the one that surfaces the residue to the
    /// emitter's exit-code decision; that split is unchanged.
    #[cfg(unix)]
    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        DELETE_CONFINEMENT
            .remove_dir_all_at(path)
            .map(|_residue| ())
    }

    #[cfg(not(unix))]
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
