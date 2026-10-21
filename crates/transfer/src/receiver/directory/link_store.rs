//! The on-disk form of a received symlink.
//!
//! A real link, or under `--fake-super` on Linux a placeholder file holding
//! the target (see `metadata::fake_super_link`).

use std::io;
use std::path::{Path, PathBuf};

/// How the receiver stores a symlink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LinkStore {
    /// A real symbolic link.
    Symlink,
    /// A regular file holding the target, for `--fake-super` on Linux.
    Placeholder,
}

impl LinkStore {
    /// The store a receiver uses, given whether `--fake-super` is active.
    pub(super) const fn new(fake_super: bool) -> Self {
        if metadata::symlinks_are_placeholders(fake_super) {
            Self::Placeholder
        } else {
            Self::Symlink
        }
    }

    /// Reads the target of the link already at `link_path`.
    ///
    /// # Errors
    ///
    /// Fails when nothing at `link_path` is a link of this store's form. The
    /// caller then treats the path as an obstacle.
    pub(super) fn read(
        self,
        sandbox: Option<&fast_io::DirSandbox>,
        dest_dir: &Path,
        relative_path: &Path,
        link_path: &Path,
    ) -> io::Result<PathBuf> {
        match self {
            Self::Symlink => fast_io::readlinkat_via_sandbox_or_fallback(
                sandbox,
                dest_dir,
                relative_path,
                link_path,
            ),
            Self::Placeholder => {
                metadata::read_fake_super_link(sandbox, dest_dir, relative_path, link_path)
            }
        }
    }

    /// Creates a link to `target` at `link_path`, anchored at the sandbox
    /// dirfd or the verified parent.
    ///
    /// # Errors
    ///
    /// Returns the create, open or write error.
    pub(super) fn create(
        self,
        sandbox: Option<&fast_io::DirSandbox>,
        dest_dir: &Path,
        relative_path: &Path,
        link_path: &Path,
        target: &Path,
    ) -> io::Result<()> {
        match self {
            Self::Symlink => fast_io::symlinkat_via_sandbox_or_fallback(
                sandbox,
                dest_dir,
                relative_path,
                link_path,
                target,
            ),
            Self::Placeholder => metadata::create_fake_super_link(
                sandbox,
                dest_dir,
                relative_path,
                link_path,
                target,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_linux_fake_super_receiver_selects_the_placeholder_store() {
        let want = if cfg!(target_os = "linux") {
            LinkStore::Placeholder
        } else {
            LinkStore::Symlink
        };
        assert_eq!(LinkStore::new(true), want);
        assert_eq!(LinkStore::new(false), LinkStore::Symlink);
    }
}
