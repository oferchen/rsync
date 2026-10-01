//! Held directory descriptor for runs of per-entry `*at` operations.
//!
//! The file list is path-sorted, so consecutive entries share a parent
//! directory. Re-resolving that parent for every entry - a fresh confined
//! walk per temp create, per rename side, per metadata apply - is what made
//! the receiver issue a dozen path-walk syscalls per file. [`HeldDir`] keeps
//! the most recently resolved directory open and hands it back while the
//! caller stays inside it, so a directory is resolved once per run of
//! entries instead of once per operation.
//!
//! The held descriptor pins the inode it resolved to. A directory renamed
//! away or replaced by a symlink after it was resolved is NOT re-resolved:
//! operations keep landing in the original inode, so a swapped-in symlink is
//! never followed. That is upstream's held-dirfd race-safety property, not a
//! hazard.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.1/syscall.c:3703-3720` - the persistent ancestor-dirfd stack
//!   and why a raced ancestor resolving to the held inode is sound.
//! - `rsync-3.5.1/syscall.c:3744-3816` `dpc_dir_fd()` - reuse while the path
//!   matches, re-resolve (and release the old descriptors) when it diverges.

use std::io;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

/// One-slot cache of the directory the current run of entries lives in.
///
/// Keyed by the caller's anchor-relative path. The resolver is supplied per
/// call, so the cache never decides policy: whatever confinement the caller's
/// resolver enforces is exactly what a cached descriptor was obtained under.
/// Shared across threads; the lock is held only for the lookup and, on a
/// miss, the one resolution.
#[derive(Debug, Default)]
pub struct HeldDir {
    slot: Mutex<Option<(PathBuf, Arc<OwnedFd>)>>,
}

impl HeldDir {
    /// An empty cache.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slot: Mutex::new(None),
        }
    }

    /// Return the held descriptor for `relative`, resolving it with `open`
    /// when the cache holds a different directory (or none).
    ///
    /// The previously held directory is released before `open` runs, so the
    /// cache never holds more than one descriptor. A failed resolution leaves
    /// the cache empty.
    ///
    /// # Errors
    ///
    /// Whatever `open` returns.
    pub fn get_or_open(
        &self,
        relative: &Path,
        open: impl FnOnce() -> io::Result<OwnedFd>,
    ) -> io::Result<Arc<OwnedFd>> {
        let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((held, fd)) = slot.as_ref()
            && held == relative
        {
            return Ok(Arc::clone(fd));
        }
        *slot = None;
        let fd = Arc::new(open()?);
        *slot = Some((relative.to_path_buf(), Arc::clone(&fd)));
        Ok(fd)
    }

    /// Drop the held descriptor, so the next lookup resolves afresh.
    ///
    /// Callers invalidate after an operation through the held descriptor
    /// fails, then retry on their uncached path: the retry reports the
    /// authoritative verdict, and a directory that vanished and reappeared is
    /// picked up again.
    pub fn invalidate(&self) {
        *self.slot.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::os::fd::AsRawFd;

    fn open_dir(path: &Path) -> io::Result<OwnedFd> {
        Ok(std::fs::File::open(path)?.into())
    }

    /// The point of the cache: consecutive entries in one directory resolve
    /// it once. A cache that re-resolved on every hit would pass every
    /// behavioural test and save nothing.
    #[test]
    fn a_run_of_lookups_in_one_directory_resolves_it_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let held = HeldDir::new();
        let opens = Cell::new(0);
        let resolve = || {
            opens.set(opens.get() + 1);
            open_dir(temp.path())
        };
        let first = held.get_or_open(Path::new("a/b"), resolve).expect("first");
        for _ in 0..5 {
            let again = held
                .get_or_open(Path::new("a/b"), || {
                    opens.set(opens.get() + 1);
                    open_dir(temp.path())
                })
                .expect("hit");
            assert_eq!(again.as_raw_fd(), first.as_raw_fd());
        }
        assert_eq!(opens.get(), 1, "a hit must not re-resolve");
    }

    /// Moving to another directory must resolve it; handing back the old
    /// descriptor would apply the entry's operation in the wrong directory.
    #[test]
    fn a_different_directory_is_resolved_not_served_from_the_slot() {
        let one = tempfile::tempdir().expect("one");
        let two = tempfile::tempdir().expect("two");
        let held = HeldDir::new();
        let a = held
            .get_or_open(Path::new("one"), || open_dir(one.path()))
            .expect("one");
        let b = held
            .get_or_open(Path::new("two"), || open_dir(two.path()))
            .expect("two");
        assert_ne!(
            fstat_ino(&a),
            fstat_ino(&b),
            "the second lookup must hold the second directory"
        );
    }

    /// A failed resolution must not leave the previous directory in the slot
    /// under the new key, and must not poison later lookups.
    #[test]
    fn a_failed_resolution_leaves_the_cache_empty() {
        let dir = tempfile::tempdir().expect("dir");
        let held = HeldDir::new();
        held.get_or_open(Path::new("x"), || open_dir(dir.path()))
            .expect("x");
        let err = held
            .get_or_open(Path::new("y"), || {
                Err(io::Error::from_raw_os_error(libc::ELOOP))
            })
            .expect_err("y fails");
        assert_eq!(err.raw_os_error(), Some(libc::ELOOP));
        let opens = Cell::new(0);
        held.get_or_open(Path::new("x"), || {
            opens.set(1);
            open_dir(dir.path())
        })
        .expect("x again");
        assert_eq!(opens.get(), 1, "x was released when y was attempted");
    }

    #[test]
    fn invalidate_forces_the_next_lookup_to_resolve() {
        let dir = tempfile::tempdir().expect("dir");
        let held = HeldDir::new();
        held.get_or_open(Path::new("x"), || open_dir(dir.path()))
            .expect("x");
        held.invalidate();
        let opens = Cell::new(0);
        held.get_or_open(Path::new("x"), || {
            opens.set(1);
            open_dir(dir.path())
        })
        .expect("x again");
        assert_eq!(opens.get(), 1);
    }

    fn fstat_ino(fd: &OwnedFd) -> u64 {
        use std::os::fd::AsFd;
        use std::os::unix::fs::MetadataExt;
        let file = std::fs::File::from(fd.as_fd().try_clone_to_owned().expect("dup"));
        file.metadata().expect("fstat").ino()
    }
}
