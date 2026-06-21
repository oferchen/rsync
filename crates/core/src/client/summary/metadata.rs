//! Entry kind and metadata types for client transfer events.
//!
//! Provides [`ClientEntryKind`] and [`ClientEntryMetadata`] which snapshot
//! the file-system metadata of entries affected by a transfer. These types
//! are used by `ClientEvent` to report per-file details.

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
    /// Creates metadata by consuming a [`LocalCopyMetadata`], moving heap fields.
    pub(crate) fn from_local_copy_metadata_owned(metadata: LocalCopyMetadata) -> Self {
        let kind = match metadata.kind() {
            LocalCopyFileKind::File => ClientEntryKind::File,
            LocalCopyFileKind::Directory => ClientEntryKind::Directory,
            LocalCopyFileKind::Symlink => ClientEntryKind::Symlink,
            LocalCopyFileKind::Fifo => ClientEntryKind::Fifo,
            LocalCopyFileKind::CharDevice => ClientEntryKind::CharDevice,
            LocalCopyFileKind::BlockDevice => ClientEntryKind::BlockDevice,
            LocalCopyFileKind::Socket => ClientEntryKind::Socket,
            LocalCopyFileKind::Other => ClientEntryKind::Other,
        };
        let length = metadata.len();
        let modified = metadata.modified();
        let mode = metadata.mode();
        let uid = metadata.uid();
        let gid = metadata.gid();
        let nlink = metadata.nlink();
        let symlink_target = metadata.into_symlink_target();
        Self {
            kind,
            length,
            modified,
            mode,
            uid,
            gid,
            nlink,
            symlink_target,
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

    /// Attaches a symlink/xname target for testing purposes.
    ///
    /// Used by renderer tests to model a relinked hardlink alias, whose `%L`
    /// placeholder renders ` => target`.
    #[doc(hidden)]
    #[must_use]
    pub fn with_symlink_target_for_test(mut self, target: &str) -> Self {
        self.symlink_target = Some(std::path::PathBuf::from(target));
        self
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
