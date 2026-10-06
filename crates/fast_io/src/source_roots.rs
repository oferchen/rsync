//! Explicit source roots a non-daemon sender holds by identity.
//!
//! Upstream pins every operator-named source directory when the file list is
//! built and reads file content beneath it, so a directory component swapped
//! for a symlink between the scan and the read cannot redirect the read out of
//! the tree, and a root replaced wholesale is refused with `ELOOP`.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.1/flist.c:234-306` `remember_sender_source_root()` /
//!   `remember_sender_source_arg()` - which roots are recorded
//! - `rsync-3.5.1/flist.c:308-338` `sender_source_root_fd_for()` - one held
//!   root at a time, refused with `ELOOP` on a dev/ino change
//! - `rsync-3.5.1/flist.c:345-385` `open_sender_source_path()` - the longest
//!   matching root, then `secure_relative_open_at()` beneath it
use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};
#[cfg(unix)]
use std::sync::{Arc, Mutex};

/// One recorded root: its cleaned absolute path and the identity it had when
/// the file list was built.
#[derive(Debug)]
struct SourceRoot {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

/// The explicit source roots of one transfer.
///
/// Built while the file list is scanned and consulted for every content open.
/// Only one root is held open at a time, so a long operand list cannot exhaust
/// the descriptor limit (`flist.c:308-309`).
#[derive(Debug, Default)]
pub struct SourceRoots {
    roots: Vec<SourceRoot>,
    #[cfg(unix)]
    held: Mutex<Option<(usize, Arc<File>)>>,
}

impl SourceRoots {
    /// An empty set; every [`open`](Self::open) reports no match.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether no root has been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// Records the root an operand contributes.
    ///
    /// A directory operand is its own root. Any other operand pins its
    /// parent directory, re-stated (following symlinks) so the identity is the
    /// directory the operator named. A parent that is not a directory, or that
    /// cannot be stated, records nothing.
    ///
    /// # Errors
    ///
    /// Fails only when the current directory cannot be read to make a relative
    /// operand absolute.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync-3.5.1/flist.c:284-306` `remember_sender_source_arg()`
    pub fn remember_operand(
        &mut self,
        path: &Path,
        is_dir: bool,
        dev: u64,
        ino: u64,
    ) -> io::Result<()> {
        let full = full_path(path)?;
        if is_dir {
            self.remember_root(full, dev, ino);
            return Ok(());
        }
        let Some(parent) = full.parent() else {
            return Ok(());
        };
        if let Some((dev, ino)) = directory_identity(parent) {
            self.remember_root(parent.to_path_buf(), dev, ino);
        }
        Ok(())
    }

    /// upstream: `flist.c:262-282` `remember_sender_source_root()` - the first
    /// identity recorded for a path wins.
    fn remember_root(&mut self, path: PathBuf, dev: u64, ino: u64) {
        if self.roots.iter().any(|root| root.path == path) {
            return;
        }
        self.roots.push(SourceRoot { path, dev, ino });
    }

    /// Opens `path` for reading beneath the longest recorded root containing
    /// it, with the leaf `O_NOFOLLOW`.
    ///
    /// `None` means no root contains `path`, and the caller keeps its own
    /// open. `Some(Err(ELOOP))` reports a root whose identity changed since
    /// it was recorded; a parent swapped for a symlink that leaves the root is
    /// refused by the walk beneath it.
    ///
    /// Non-Unix targets have no `*at` resolver and report no match.
    #[must_use]
    pub fn open(&self, path: &Path, noatime: bool) -> Option<io::Result<File>> {
        #[cfg(unix)]
        {
            let full = match full_path(path) {
                Ok(full) => full,
                Err(error) => return Some(Err(error)),
            };
            let (index, root) = self
                .roots
                .iter()
                .enumerate()
                .filter(|(_, root)| full.starts_with(&root.path))
                .max_by_key(|(_, root)| root.path.as_os_str().len())?;
            let relative = full.strip_prefix(&root.path).unwrap_or(Path::new(""));
            Some(self.anchor(index).and_then(|anchor| {
                use std::os::fd::AsFd;
                crate::confined_open::open_source_beneath(anchor.as_fd(), relative, noatime)
            }))
        }
        #[cfg(not(unix))]
        {
            let _ = (path, noatime);
            None
        }
    }

    /// The held descriptor for root `index`, reopened and re-checked when a
    /// different root was held.
    ///
    /// upstream: `flist.c:311-338` `sender_source_root_fd_for()`
    #[cfg(unix)]
    fn anchor(&self, index: usize) -> io::Result<Arc<File>> {
        use std::os::unix::fs::MetadataExt;
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((owner, fd)) = held.as_ref()
            && *owner == index
        {
            return Ok(Arc::clone(fd));
        }
        *held = None;
        let root = &self.roots[index];
        let dir = File::from(crate::secure_dir::open_trusted_dir(&root.path)?);
        let meta = dir.metadata()?;
        if meta.dev() != root.dev || meta.ino() != root.ino {
            return Err(io::Error::from_raw_os_error(libc::ELOOP));
        }
        let dir = Arc::new(dir);
        *held = Some((index, Arc::clone(&dir)));
        Ok(dir)
    }
}

/// The identity of `path` when it names a directory, following symlinks.
///
/// upstream: `flist.c:302-305` - `do_stat(full, &parent_st) == 0 &&
/// S_ISDIR(parent_st.st_mode)`
#[cfg(unix)]
fn directory_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    meta.is_dir().then(|| (meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn directory_identity(_path: &Path) -> Option<(u64, u64)> {
    None
}

/// `path` made absolute against the current directory with `.` dropped and
/// `..` collapsed lexically, so a root and the file paths beneath it compare
/// component-wise.
///
/// upstream: `flist.c:243-257` `sender_source_full_path()` - `pathjoin(curr_dir,
/// path)` then `clean_fname(CFN_COLLAPSE_DOT_DOT_DIRS | CFN_DROP_TRAILING_DOT_DIR)`
fn full_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut full = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                full.pop();
            }
            other => full.push(other),
        }
    }
    Ok(full)
}

#[cfg(all(test, unix))]
mod tests;
