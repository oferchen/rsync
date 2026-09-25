//! The operator-named destination root, pinned once like upstream's cwd.

use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
#[cfg(unix)]
use std::sync::OnceLock;

/// The destination operand the operator named, shared by every metadata apply
/// of one transfer.
///
/// Upstream enters the destination exactly once with `change_dir()`
/// (`main.c` `get_local_name()`) and resolves every entry relative to that
/// cwd, so the operator's path is resolved once and every later syscall is
/// anchored on the directory it reached. This carries the same pin: the root
/// directory is opened on first use and the descriptor is reused for the rest
/// of the transfer, so the per-entry cost is one `openat` beneath it rather
/// than a fresh walk of the whole operator path.
///
/// The root is opened lazily because a local copy may create it: by the time
/// any entry below it has metadata applied, it exists.
#[derive(Debug)]
pub struct DestinationRoot {
    path: PathBuf,
    #[cfg(unix)]
    anchor: OnceLock<OwnedFd>,
}

impl DestinationRoot {
    /// Records `path` as the operator's destination root; nothing is opened yet.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            #[cfg(unix)]
            anchor: OnceLock::new(),
        }
    }

    /// The destination operand as the operator spelled it.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The root directory, opened through the ownership walk on first use.
    ///
    /// A symlink owned by uid 0 or our euid anywhere in the operator's path is
    /// followed; one owned by anyone else is refused (`ELOOP`), which is
    /// stricter than upstream's plain `chdir` for a non-daemon receiver. A
    /// failed open is not cached, so it is reported again on the next call.
    #[cfg(unix)]
    pub(crate) fn anchor(&self) -> std::io::Result<BorrowedFd<'_>> {
        if let Some(fd) = self.anchor.get() {
            return Ok(fd.as_fd());
        }
        let opened = fast_io::operator_open_dir(&self.path)?;
        Ok(self.anchor.get_or_init(|| opened).as_fd())
    }
}

/// Two roots are the same root when the operator named the same path; the
/// pinned descriptor is a cache, not identity.
impl PartialEq for DestinationRoot {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

impl Eq for DestinationRoot {}
