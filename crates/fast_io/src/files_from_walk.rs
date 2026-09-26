//! Ownership-walk resolution for a sender's `--files-from` entries.
//!
//! The `--files-from` source base is operator-selected, but every entry in the
//! list may not be: the list can come from the peer or from a file another user
//! controls. Upstream therefore resolves each entry with the ownership walk -
//! follow a symlink owned by uid 0 or the euid, refuse any other - and, under
//! `--confine-root`, refuses an entry whose resolved path lands outside the
//! root. A trusted-owned in-tree symlink pointing OUTSIDE the root is the case
//! the ownership rule alone lets through; the confinement judgement closes it.
//!
//! Every walk starts at the held base directory, the way upstream's walk starts
//! at the directory its sender `chdir`ed into, so the base itself is followed
//! as the operator named it and only the entry's own components are judged.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.1/syscall.c:149` `filesfrom_owner_walk_active()` - the gate.
//! - `rsync-3.5.1/flist.c:400-433` `filesfrom_link_stat()` - the entry stat.
//! - `rsync-3.5.1/flist.c:2262-2265` `secure_opendir()` - the directory scan.
//! - `rsync-3.5.1/syscall.c:3617-3640` `do_open_checklinks()` - the content open.

use std::ffi::OsString;
use std::fs::{File, Metadata};
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use rustix::fs::{Mode, OFlags};

use crate::confinement::PathKind;
use crate::owner_walk::{WalkBase, owner_walk_open_tracked, traversal_dir_flags, walk_openat};

/// A `--files-from` base directory held open for the entries resolved beneath
/// it.
#[derive(Debug)]
pub struct FilesFromBase {
    /// The base as the operand spells it; entries are named relative to it.
    path: PathBuf,
    /// Its physical absolute path, the confinement tracker's seed.
    physical: PathBuf,
    /// The held base directory every walk starts from.
    dir: OwnedFd,
}

impl FilesFromBase {
    /// Open the base directory, following symlinks the way upstream's
    /// `chdir()` into it does.
    ///
    /// # Errors
    ///
    /// The `canonicalize` or `openat` error for the base.
    pub fn open(path: &Path) -> io::Result<Self> {
        let physical = std::fs::canonicalize(path)?;
        let dir = walk_openat(
            rustix::fs::CWD,
            physical.as_os_str(),
            traversal_dir_flags(),
            Mode::empty(),
        )?;
        Ok(Self {
            path: path.to_path_buf(),
            physical,
            dir,
        })
    }

    /// The base path this handle was opened for, as the operand spells it.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The part of `path` below the base, which is what the walk resolves.
    fn entry<'a>(&self, path: &'a Path) -> &'a Path {
        path.strip_prefix(&self.path).unwrap_or(path)
    }

    fn walk(&self, entry: &Path, flags: OFlags) -> io::Result<(OwnedFd, Option<PathBuf>)> {
        let base = WalkBase {
            dir: self.dir.as_fd(),
            physical: &self.physical,
        };
        let mut resolved = None;
        let fd = owner_walk_open_tracked(
            entry,
            flags,
            Mode::empty(),
            PathKind::Confined,
            Some(&base),
            &mut resolved,
        )?;
        Ok((fd, resolved))
    }

    /// Open the parent of `entry` through the walk and judge the leaf's
    /// would-be absolute path against the confinement root.
    ///
    /// upstream: `rsync-3.5.1/syscall.c:704-737` `owner_walk_parent()`.
    fn parent(&self, entry: &Path) -> io::Result<(OwnedFd, OsString)> {
        // The base itself (an entry such as `dir/./`) is the leaf `.` of an
        // empty parent, as upstream's `strrchr()` split makes it.
        let (parent, leaf) = match entry.file_name() {
            Some(leaf) => (
                entry.parent().unwrap_or_else(|| Path::new("")),
                OsString::from(leaf),
            ),
            None if entry.as_os_str().is_empty() || entry == Path::new(".") => {
                (Path::new(""), OsString::from("."))
            }
            None => return Err(io::Error::from_raw_os_error(libc::EINVAL)),
        };
        let (dirfd, parent_abs) = self.walk(parent, traversal_dir_flags())?;
        if let Some(abs) = parent_abs
            && crate::confinement::outside_session_root(
                &abs.join(&leaf),
                PathKind::Confined,
                crate::confinement::Arrival::Final,
            )
        {
            return Err(io::Error::from_raw_os_error(libc::ELOOP));
        }
        Ok((dirfd, leaf))
    }

    /// Stat a list entry the way upstream's sender does.
    ///
    /// The leaf is `lstat`ed through the walked parent. A leaf symlink is
    /// followed only when `follow_dirlinks` holds, and then through the walk
    /// itself, so a list-selected directory link is judged like any other
    /// path component. A follow that fails with `ENOENT`, `ENOTDIR` or
    /// `EACCES` keeps the symlink's own `lstat`, as upstream does.
    ///
    /// The returned [`Metadata`] comes from a path-based stat that must name
    /// the same inode the walk reached; a mismatch reads as `NotFound`.
    ///
    /// upstream: `rsync-3.5.1/flist.c:400-433` `filesfrom_link_stat()`.
    ///
    /// # Errors
    ///
    /// `ELOOP` when an untrusted-owned symlink is met or the entry resolves
    /// outside the confinement root; otherwise the walk's or the stat's errno.
    pub fn symlink_metadata(&self, path: &Path, follow_dirlinks: bool) -> io::Result<Metadata> {
        let entry = self.entry(path);
        let (parent, leaf) = self.parent(entry)?;
        let lstat = crate::fstatat_nofollow(parent.as_fd(), &leaf)?;
        if lstat.is_symlink() && follow_dirlinks {
            match self.walk(entry, traversal_dir_flags()) {
                Ok((target, _)) => {
                    let walked = File::from(target).metadata()?;
                    return same_inode(std::fs::metadata(path)?, walked.dev(), walked.ino());
                }
                Err(error)
                    if matches!(
                        error.raw_os_error(),
                        Some(libc::ENOENT | libc::ENOTDIR | libc::EACCES)
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        same_inode(std::fs::symlink_metadata(path)?, lstat.dev(), lstat.ino())
    }

    /// Open a list entry's content: the parent through the walk, the leaf
    /// `O_NOFOLLOW`.
    ///
    /// upstream: `rsync-3.5.1/syscall.c:3623-3640` - the `files_from` arm of
    /// `do_open_checklinks()`.
    ///
    /// # Errors
    ///
    /// As [`Self::symlink_metadata`], plus `ELOOP` for a leaf symlink.
    pub fn open_read(&self, path: &Path, noatime: bool) -> io::Result<File> {
        let (parent, leaf) = self.parent(self.entry(path))?;
        crate::confined_open::open_source_leaf(parent.as_fd(), &leaf, noatime)
    }

    /// List a directory reached through a list entry.
    ///
    /// upstream: `rsync-3.5.1/flist.c:2262-2265` - `secure_opendir()` opens a
    /// `--files-from` directory with `open_no_attacker_symlinks(fbuf,
    /// O_RDONLY | O_DIRECTORY, 0)`.
    ///
    /// # Errors
    ///
    /// As [`Self::symlink_metadata`], plus the `readdir` setup error.
    pub fn read_dir(&self, path: &Path) -> io::Result<Vec<OsString>> {
        let (dir, _) = self.walk(self.entry(path), OFlags::RDONLY | OFlags::DIRECTORY)?;
        crate::confined_readdir::read_dir_names_at(dir.as_fd())
    }
}

/// Hand back the path-based `meta` only when it names the inode the walk
/// reached.
fn same_inode(meta: Metadata, dev: u64, ino: u64) -> io::Result<Metadata> {
    if meta.dev() == dev && meta.ino() == ino {
        Ok(meta)
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "files-from entry changed inode between the walk and the path stat",
        ))
    }
}
