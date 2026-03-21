//! Entry kind and metadata types for client transfer events.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use engine::local_copy::{LocalCopyFileKind, LocalCopyMetadata};

/// Kind of entry described by [`ClientEntryMetadata`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientEntryKind {
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
    /// Entry of an unknown or platform-specific type.
    Other,
}

impl ClientEntryKind {
    /// Returns whether the metadata describes a directory entry.
    #[must_use]
    pub const fn is_directory(self) -> bool {
        matches!(self, Self::Directory)
    }

    /// Returns whether the metadata describes a symbolic link entry.
    #[must_use]
    pub const fn is_symlink(self) -> bool {
        matches!(self, Self::Symlink)
    }
}

/// Metadata snapshot describing an entry affected by a client event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientEntryMetadata {
    kind: ClientEntryKind,
    length: u64,
    modified: Option<SystemTime>,
    mode: Option<u32>,
    uid: Option<u32>,
    gid: Option<u32>,
    nlink: Option<u64>,
    symlink_target: Option<PathBuf>,
}

impl ClientEntryMetadata {
    pub(crate) fn from_local_copy_metadata(metadata: &LocalCopyMetadata) -> Self {
        Self {
            kind: match metadata.kind() {
                LocalCopyFileKind::File => ClientEntryKind::File,
                LocalCopyFileKind::Directory => ClientEntryKind::Directory,
                LocalCopyFileKind::Symlink => ClientEntryKind::Symlink,
                LocalCopyFileKind::Fifo => ClientEntryKind::Fifo,
                LocalCopyFileKind::CharDevice => ClientEntryKind::CharDevice,
                LocalCopyFileKind::BlockDevice => ClientEntryKind::BlockDevice,
                LocalCopyFileKind::Socket => ClientEntryKind::Socket,
                LocalCopyFileKind::Other => ClientEntryKind::Other,
            },
            length: metadata.len(),
            modified: metadata.modified(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            nlink: metadata.nlink(),
            symlink_target: metadata.symlink_target().map(Path::to_path_buf),
        }
    }

    /// Constructs a [`ClientEntryMetadata`] for testing purposes.
    #[doc(hidden)]
    pub fn for_test(kind: ClientEntryKind) -> Self {
        Self {
            kind,
            length: 0,
            modified: None,
            mode: None,
            uid: None,
            gid: None,
            nlink: None,
            symlink_target: None,
        }
    }

    /// Returns the kind of entry represented by this metadata snapshot.
    #[must_use]
    pub const fn kind(&self) -> ClientEntryKind {
        self.kind
    }

    /// Returns the logical length of the entry in bytes.
    #[must_use]
    pub const fn length(&self) -> u64 {
        self.length
    }

    /// Returns the recorded modification timestamp, when available.
    pub const fn modified(&self) -> Option<SystemTime> {
        self.modified
    }

    /// Returns the Unix permission bits when available.
    pub const fn mode(&self) -> Option<u32> {
        self.mode
    }

    /// Returns the numeric owner identifier when available.
    pub const fn uid(&self) -> Option<u32> {
        self.uid
    }

    /// Returns the numeric group identifier when available.
    pub const fn gid(&self) -> Option<u32> {
        self.gid
    }

    /// Returns the recorded link count when available.
    pub const fn nlink(&self) -> Option<u64> {
        self.nlink
    }

    /// Returns the recorded symbolic link target when the entry represents a symlink.
    pub fn symlink_target(&self) -> Option<&Path> {
        self.symlink_target.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_entry_kind_is_directory_returns_true_for_directory() {
        assert!(ClientEntryKind::Directory.is_directory());
    }

    #[test]
    fn client_entry_kind_is_directory_returns_false_for_others() {
        assert!(!ClientEntryKind::File.is_directory());
        assert!(!ClientEntryKind::Symlink.is_directory());
        assert!(!ClientEntryKind::Fifo.is_directory());
    }

    #[test]
    fn client_entry_kind_is_symlink_returns_true_for_symlink() {
        assert!(ClientEntryKind::Symlink.is_symlink());
    }

    #[test]
    fn client_entry_kind_is_symlink_returns_false_for_others() {
        assert!(!ClientEntryKind::File.is_symlink());
        assert!(!ClientEntryKind::Directory.is_symlink());
        assert!(!ClientEntryKind::Fifo.is_symlink());
    }
}
