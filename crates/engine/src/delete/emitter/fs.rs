//! Filesystem dispatch trait and implementations for the delete emitter.
//!
//! Hosts the [`DeleteFs`] trait, the production [`RealDeleteFs`] backed
//! by `std::fs`, and the [`RecordingDeleteFs`] test fake. Splitting one
//! method per upstream-distinguishable entry kind (`delete.c:144-176`)
//! lets unit tests assert the exact dispatch table even though all
//! file-like kinds currently route to `unlink(2)` in production.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::super::DeleteEntryKind;

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
pub trait DeleteFs {
    /// Unlinks a regular file.
    fn unlink_file(&self, path: &Path) -> io::Result<()>;

    /// Removes an empty directory.
    fn rmdir(&self, path: &Path) -> io::Result<()>;

    /// Unlinks a symbolic link.
    fn unlink_symlink(&self, path: &Path) -> io::Result<()>;

    /// Unlinks a block or character device node.
    fn unlink_device(&self, path: &Path) -> io::Result<()>;

    /// Unlinks a FIFO or socket.
    fn unlink_special(&self, path: &Path) -> io::Result<()>;

    /// Recursively removes a directory and everything beneath it.
    ///
    /// Invoked by the emitter when [`Self::rmdir`] returns
    /// [`io::ErrorKind::DirectoryNotEmpty`] and no nested
    /// [`super::super::DeletePlan`] has been published for the offending
    /// child (upstream `delete.c:48-122 delete_dir_contents`).
    fn remove_dir_all(&self, path: &Path) -> io::Result<()>;
}

/// Production [`DeleteFs`] implementation backed by `std::fs`.
///
/// All file-like kinds route to [`fs::remove_file`] (Unix `unlink(2)`,
/// Windows `DeleteFileW`). Directories route to [`fs::remove_dir`]
/// (`rmdir(2)`); the recursive fallback routes to [`fs::remove_dir_all`]
/// to match upstream `delete_dir_contents`. This mirrors upstream
/// `delete_item` (`delete.c:161-175`): `do_rmdir` for `S_ISDIR`,
/// `robust_unlink` for everything else.
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
    #[must_use]
    pub fn events(&self) -> Vec<DeleteEvent> {
        self.events.lock().expect("recorder mutex poisoned").clone()
    }

    fn record(&self, path: &Path, kind: DeleteEntryKind) {
        self.events
            .lock()
            .expect("recorder mutex poisoned")
            .push(DeleteEvent {
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
}
