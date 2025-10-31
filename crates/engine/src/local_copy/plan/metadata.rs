use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::super::is_fifo;

/// File type captured for [`LocalCopyMetadata`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalCopyFileKind {
    /// Regular file entry.
    File,
    /// Directory entry.
    Directory,
    /// Symbolic link entry.
    Symlink,
    /// FIFO entry.
    Fifo,
    /// Character device entry.
    CharDevice,
    /// Block device entry.
    BlockDevice,
    /// Unix domain socket entry.
    Socket,
    /// Unknown or platform specific entry.
    Other,
}

impl LocalCopyFileKind {
    pub(super) fn from_file_type(file_type: &fs::FileType) -> Self {
        if file_type.is_dir() {
            return Self::Directory;
        }
        if file_type.is_symlink() {
            return Self::Symlink;
        }
        if file_type.is_file() {
            return Self::File;
        }
        if is_fifo(file_type) {
            return Self::Fifo;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;

            if file_type.is_char_device() {
                return Self::CharDevice;
            }
            if file_type.is_block_device() {
                return Self::BlockDevice;
            }
            if file_type.is_socket() {
                return Self::Socket;
            }
        }
        Self::Other
    }

    /// Returns whether the kind represents a directory.
    #[must_use]
    pub const fn is_directory(self) -> bool {
        matches!(self, Self::Directory)
    }
}

/// Metadata snapshot recorded for events emitted by [`super::LocalCopyRecord`].
#[derive(Clone, Debug)]
pub struct LocalCopyMetadata {
    kind: LocalCopyFileKind,
    len: u64,
    modified: Option<SystemTime>,
    mode: Option<u32>,
    uid: Option<u32>,
    gid: Option<u32>,
    nlink: Option<u64>,
    symlink_target: Option<PathBuf>,
}

impl LocalCopyMetadata {
    pub(in crate::local_copy) fn from_metadata(
        metadata: &fs::Metadata,
        symlink_target: Option<PathBuf>,
    ) -> Self {
        let file_type = metadata.file_type();
        let kind = LocalCopyFileKind::from_file_type(&file_type);
        let len = metadata.len();
        let modified = metadata.modified().ok();

        #[cfg(unix)]
        let (mode, uid, gid, nlink) = {
            use std::os::unix::fs::MetadataExt;
            (
                Some(metadata.mode()),
                Some(metadata.uid()),
                Some(metadata.gid()),
                Some(metadata.nlink()),
            )
        };

        #[cfg(not(unix))]
        let (mode, uid, gid, nlink) = (None, None, None, None);

        let target = if matches!(kind, LocalCopyFileKind::Symlink) {
            symlink_target
        } else {
            None
        };

        Self {
            kind,
            len,
            modified,
            mode,
            uid,
            gid,
            nlink,
            symlink_target: target,
        }
    }

    /// Returns the entry kind associated with the metadata.
    #[must_use]
    pub const fn kind(&self) -> LocalCopyFileKind {
        self.kind
    }

    /// Returns the entry length in bytes.
    #[must_use]
    pub const fn len(&self) -> u64 {
        self.len
    }

    /// Returns whether the metadata describes an empty entry.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the recorded modification time, when available.
    #[must_use]
    pub const fn modified(&self) -> Option<SystemTime> {
        self.modified
    }

    /// Returns the Unix permission bits when available.
    #[must_use]
    pub const fn mode(&self) -> Option<u32> {
        self.mode
    }

    /// Returns the numeric owner identifier when available.
    #[must_use]
    pub const fn uid(&self) -> Option<u32> {
        self.uid
    }

    /// Returns the numeric group identifier when available.
    #[must_use]
    pub const fn gid(&self) -> Option<u32> {
        self.gid
    }

    /// Returns the hard link count when available.
    #[must_use]
    pub const fn nlink(&self) -> Option<u64> {
        self.nlink
    }

    /// Returns the recorded symbolic link target when the metadata describes a symlink.
    #[must_use]
    pub fn symlink_target(&self) -> Option<&Path> {
        self.symlink_target.as_deref()
    }
}
