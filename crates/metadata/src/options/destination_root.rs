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
    /// The daemon module that `path` lies beneath, when a daemon serves it.
    #[cfg(unix)]
    module_root: Option<PathBuf>,
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
            module_root: None,
            #[cfg(unix)]
            anchor: OnceLock::new(),
        }
    }

    /// Records `path`, a destination beneath the daemon module `module_root`.
    ///
    /// The operator named only the module; the remainder of `path` came from
    /// the peer, so it is entered beneath the module rather than trusted.
    /// upstream: util1.c change_dir() - a daemon enters the peer's relative
    /// destination through secure_relative_dirfd() from the module root.
    #[must_use]
    pub fn served(path: PathBuf, module_root: PathBuf) -> Self {
        #[cfg(unix)]
        return Self {
            module_root: Some(module_root),
            ..Self::new(path)
        };
        #[cfg(not(unix))]
        {
            drop(module_root);
            Self::new(path)
        }
    }

    /// The destination operand as the operator spelled it.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The root directory, opened on first use through [`Self::open_dir`].
    ///
    /// A failed open is not cached, so it is reported again on the next call.
    #[cfg(unix)]
    pub(crate) fn anchor(&self) -> std::io::Result<BorrowedFd<'_>> {
        if let Some(fd) = self.anchor.get() {
            return Ok(fd.as_fd());
        }
        let opened = self.open_dir(&self.path)?;
        Ok(self.anchor.get_or_init(|| opened).as_fd())
    }

    /// Opens `dir`, the root or a single-file root's parent, the way upstream
    /// enters the destination.
    ///
    /// - Operator-named: the ownership walk, which follows a symlink owned by
    ///   uid 0 or our euid and refuses any other (`ELOOP`). upstream: util1.c
    ///   change_dir() open_no_attacker_symlinks_dirfd().
    /// - Daemon-served: the module root is opened as the operator's, and the
    ///   peer's remainder is walked beneath it on every platform the way
    ///   upstream's `ds_descend()` walks it (syscall.c:3032): a relative
    ///   in-tree symlink is followed, an absolute target or a climb above the
    ///   module is refused (`ELOOP`).
    #[cfg(unix)]
    pub(crate) fn open_dir(&self, dir: &Path) -> std::io::Result<OwnedFd> {
        let Some(module_root) = &self.module_root else {
            return fast_io::operator_open_dir(dir);
        };
        let tail = dir
            .strip_prefix(module_root)
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EXDEV))?;
        fast_io::DirSandbox::open_dest_anchor_confined(
            module_root,
            tail,
            fast_io::dir_sandbox::ConfinePolicy::operator_trusted(),
        )?
        .root_dirfd()
        .try_clone_to_owned()
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
