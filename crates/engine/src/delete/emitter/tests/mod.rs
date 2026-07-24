//! Unit tests for the delete emitter.
//!
//! Split into focused submodules:
//! - [`dispatch`] - DDP-C1 scaffold tests covering the dispatch matrix.
//! - [`error_policy`] - DDP-C3 error-classification / continue-on-error
//!   behaviour, mirroring upstream `delete.c:178-207`.
//! - [`cohort`] - DDP-D2 hardlink-cohort observer log.
//!
//! Shared helpers (synthetic plan/entry builders, the [`ScriptedDeleteFs`]
//! failure fake) live here.

#[cfg(unix)]
use std::ffi::OsStr;
use std::ffi::OsString;
use std::io;
#[cfg(unix)]
use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use protocol::flist::FileEntry;

use super::super::{DeleteEntry, DeleteEntryKind, DeletePlan};
use super::{DeleteEvent, DeleteFs, RecordingDeleteFs};
use crate::util::poison::lock_or_recover;

mod cohort;
mod dispatch;
mod error_policy;
#[cfg(unix)]
mod sandbox;

pub(super) fn entry(name: &str, kind: DeleteEntryKind) -> DeleteEntry {
    DeleteEntry::new(OsString::from(name), kind)
}

pub(super) fn plan(dir: &str, entries: Vec<DeleteEntry>) -> DeletePlan {
    DeletePlan::from_extras(PathBuf::from(dir), entries)
}

pub(super) fn dir_child(parent: &str, name: &str) -> FileEntry {
    let path = if parent.is_empty() {
        PathBuf::from(name)
    } else {
        PathBuf::from(parent).join(name)
    };
    FileEntry::new_directory(path, 0o755)
}

/// Failure plan: for each (path, errno) pair, the next call against
/// that path returns the matching error before falling back to the
/// recording behaviour.
#[derive(Debug, Default)]
pub(super) struct ScriptedDeleteFs {
    inner: RecordingDeleteFs,
    rules: Mutex<Vec<(PathBuf, io::ErrorKind)>>,
    /// Scripted [`UnlinkResidue`] values returned by `remove_dir_all_at`,
    /// keyed on the leaf name, so tests can model a recursive peel that
    /// stepped over a child error or left the root non-empty.
    #[cfg(unix)]
    peel: Mutex<Vec<(PathBuf, fast_io::UnlinkResidue)>>,
}

impl ScriptedDeleteFs {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn fail(self, path: &str, kind: io::ErrorKind) -> Self {
        lock_or_recover(&self.rules).push((PathBuf::from(path), kind));
        self
    }

    /// Scripts the [`UnlinkResidue`](fast_io::UnlinkResidue) the next
    /// `remove_dir_all_at` call against `name` returns, modelling a
    /// recursive peel outcome.
    #[cfg(unix)]
    pub(super) fn peel(self, name: &str, residue: fast_io::UnlinkResidue) -> Self {
        lock_or_recover(&self.peel).push((PathBuf::from(name), residue));
        self
    }

    #[cfg(unix)]
    fn maybe_peel(&self, name: &Path) -> Option<fast_io::UnlinkResidue> {
        let mut peel = lock_or_recover(&self.peel);
        let position = peel.iter().position(|(p, _)| p == name)?;
        Some(peel.remove(position).1)
    }

    pub(super) fn events(&self) -> Vec<DeleteEvent> {
        self.inner.events()
    }

    fn maybe_fail(&self, path: &Path) -> Option<io::Error> {
        let mut rules = lock_or_recover(&self.rules);
        let position = rules.iter().position(|(p, _)| p == path)?;
        let (_, kind) = rules.remove(position);
        Some(io::Error::new(kind, "scripted failure"))
    }
}

impl DeleteFs for ScriptedDeleteFs {
    fn unlink_file(&self, path: &Path) -> io::Result<()> {
        if let Some(err) = self.maybe_fail(path) {
            return Err(err);
        }
        self.inner.unlink_file(path)
    }

    fn rmdir(&self, path: &Path) -> io::Result<()> {
        if let Some(err) = self.maybe_fail(path) {
            return Err(err);
        }
        self.inner.rmdir(path)
    }

    fn unlink_symlink(&self, path: &Path) -> io::Result<()> {
        if let Some(err) = self.maybe_fail(path) {
            return Err(err);
        }
        self.inner.unlink_symlink(path)
    }

    fn unlink_device(&self, path: &Path) -> io::Result<()> {
        if let Some(err) = self.maybe_fail(path) {
            return Err(err);
        }
        self.inner.unlink_device(path)
    }

    fn unlink_special(&self, path: &Path) -> io::Result<()> {
        if let Some(err) = self.maybe_fail(path) {
            return Err(err);
        }
        self.inner.unlink_special(path)
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        if let Some(err) = self.maybe_fail(path) {
            return Err(err);
        }
        self.inner.remove_dir_all(path)
    }

    #[cfg(unix)]
    fn unlink_file_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
        if let Some(err) = self.maybe_fail(Path::new(name)) {
            return Err(err);
        }
        self.inner.unlink_file_at(parent_fd, name)
    }

    #[cfg(unix)]
    fn rmdir_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
        if let Some(err) = self.maybe_fail(Path::new(name)) {
            return Err(err);
        }
        self.inner.rmdir_at(parent_fd, name)
    }

    #[cfg(unix)]
    fn unlink_symlink_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
        if let Some(err) = self.maybe_fail(Path::new(name)) {
            return Err(err);
        }
        self.inner.unlink_symlink_at(parent_fd, name)
    }

    #[cfg(unix)]
    fn unlink_device_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
        if let Some(err) = self.maybe_fail(Path::new(name)) {
            return Err(err);
        }
        self.inner.unlink_device_at(parent_fd, name)
    }

    #[cfg(unix)]
    fn unlink_special_at(&self, parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
        if let Some(err) = self.maybe_fail(Path::new(name)) {
            return Err(err);
        }
        self.inner.unlink_special_at(parent_fd, name)
    }

    #[cfg(unix)]
    fn remove_dir_all_at(
        &self,
        parent_fd: BorrowedFd<'_>,
        name: &OsStr,
    ) -> io::Result<fast_io::UnlinkResidue> {
        if let Some(err) = self.maybe_fail(Path::new(name)) {
            return Err(err);
        }
        let residue = self.inner.remove_dir_all_at(parent_fd, name)?;
        Ok(self.maybe_peel(Path::new(name)).unwrap_or(residue))
    }
}
