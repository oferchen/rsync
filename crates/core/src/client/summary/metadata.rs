//! Entry kind and metadata types for client transfer events.
//!
//! Provides [`ClientEntryKind`] and [`ClientEntryMetadata`] which snapshot
//! the file-system metadata of entries affected by a transfer. These types
//! are used by `ClientEvent` to report per-file details.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use engine::local_copy::{LocalCopyFileKind, LocalCopyMetadata};

/// Raw flist-entry fields needed to render one `--list-only` row.
///
/// A parameter object for [`ClientEntryMetadata::from_list_only_entry`]: it
/// keeps the constructor to a single argument and decouples the summary layer
/// from the transfer crate's receiver type, so the renderer's unit tests can
/// build it directly. Times are whole seconds plus a nanosecond component.
///
/// # Upstream Reference
///
/// - `generator.c:1249` - `list_file_entry()` renders mode/size/mtime/name plus
///   the `F_ATIME(f)` / `F_CRTIME(f)` columns when the atimes/crtimes ndx is set
pub struct ListOnlyEntryFields {
    /// POSIX mode bits (type + permissions).
    pub mode: u32,
    /// File length in bytes.
    pub size: u64,
    /// Modification time, whole seconds since the Unix epoch.
    pub mtime: i64,
    /// Modification time sub-second component, nanoseconds.
    pub mtime_nsec: u32,
    /// Access time, whole seconds since the Unix epoch.
    pub atime: i64,
    /// Access time sub-second component, nanoseconds.
    pub atime_nsec: u32,
    /// Creation time, whole seconds since the Unix epoch.
    pub crtime: i64,
    /// Creation time sub-second component, nanoseconds.
    pub crtime_nsec: u32,
    /// Symlink target, when the entry is a symlink.
    pub symlink_target: Option<PathBuf>,
    /// Whether the entry is a symlink.
    pub is_symlink: bool,
}

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
    accessed: Option<SystemTime>,
    created: Option<SystemTime>,
    mode: Option<u32>,
    uid: Option<u32>,
    gid: Option<u32>,
    nlink: Option<u64>,
    symlink_target: Option<PathBuf>,
}

/// Converts a whole-second + nanosecond timestamp into a [`SystemTime`].
///
/// Returns `None` for the zero timestamp (`secs == 0 && nsec == 0`), matching
/// the `modified` field's handling so an unset epoch value renders blank rather
/// than as `1970/01/01`. Negative seconds (pre-epoch) also yield `None`.
fn secs_nsec_to_system_time(secs: i64, nsec: u32) -> Option<SystemTime> {
    if secs > 0 || nsec > 0 {
        u64::try_from(secs)
            .ok()
            .map(|secs| SystemTime::UNIX_EPOCH + Duration::new(secs, nsec))
    } else {
        None
    }
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
            // LocalCopyMetadata does not capture atime/crtime; local-copy
            // list-only never renders the ATIME/CRTIME columns (those flow
            // from the receiver flist path via `from_list_only_entry`).
            accessed: None,
            created: None,
            mode,
            uid,
            gid,
            nlink,
            symlink_target,
        }
    }

    /// Constructs metadata for a `--list-only` flist entry.
    ///
    /// The receiver captures each entry's raw mode/size/mtime/target while
    /// rendering the file list (it never opens or stats the destination), so
    /// this builds the snapshot directly from those fields. The `mtime` is in
    /// whole seconds and `mtime_nsec` carries the sub-second component.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1249` - `list_file_entry()` renders mode/size/mtime/name,
    ///   plus the `F_ATIME(f)` / `F_CRTIME(f)` columns when the atimes/crtimes
    ///   ndx is active
    #[must_use]
    pub fn from_list_only_entry(entry: &ListOnlyEntryFields) -> Self {
        let kind = if entry.is_symlink {
            ClientEntryKind::Symlink
        } else {
            // upstream: list_file_entry() derives the type char from the mode
            // bits; mirror the POSIX S_IFMT classification here.
            match entry.mode & 0o170000 {
                0o040000 => ClientEntryKind::Directory,
                0o120000 => ClientEntryKind::Symlink,
                0o010000 => ClientEntryKind::Fifo,
                0o020000 => ClientEntryKind::CharDevice,
                0o060000 => ClientEntryKind::BlockDevice,
                0o140000 => ClientEntryKind::Socket,
                0o100000 => ClientEntryKind::File,
                _ => ClientEntryKind::Other,
            }
        };
        Self {
            kind,
            length: entry.size,
            modified: secs_nsec_to_system_time(entry.mtime, entry.mtime_nsec),
            accessed: secs_nsec_to_system_time(entry.atime, entry.atime_nsec),
            created: secs_nsec_to_system_time(entry.crtime, entry.crtime_nsec),
            mode: Some(entry.mode),
            uid: None,
            gid: None,
            nlink: None,
            symlink_target: entry.symlink_target.clone(),
        }
    }

    /// Constructs a [`ClientEntryMetadata`] for testing purposes.
    #[doc(hidden)]
    pub fn for_test(kind: ClientEntryKind) -> Self {
        Self {
            kind,
            length: 0,
            modified: None,
            accessed: None,
            created: None,
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

    /// Returns the recorded access timestamp, when available.
    ///
    /// Populated for `--list-only` flist entries so the renderer can emit the
    /// ATIME column under `-U`/`--atimes` (upstream: `generator.c`
    /// `list_file_entry()` `F_ATIME(f)`).
    pub const fn accessed(&self) -> Option<SystemTime> {
        self.accessed
    }

    /// Returns the recorded creation (birth) timestamp, when available.
    ///
    /// Populated for `--list-only` flist entries so the renderer can emit the
    /// CRTIME column under `--crtimes` (upstream: `generator.c`
    /// `list_file_entry()` `F_CRTIME(f)`).
    pub const fn created(&self) -> Option<SystemTime> {
        self.created
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
