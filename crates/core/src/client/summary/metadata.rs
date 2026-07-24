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

/// Raw itemize-row fields needed to build a remote-transfer `ClientEvent`.
///
/// A parameter object for [`ClientEntryMetadata::from_remote_itemize`] and
/// [`super::ClientEvent::from_remote_itemize`], keeping both constructors to a
/// single argument and decoupling the summary layer from the transfer crate's
/// `ItemizeRow` (mirrors [`ListOnlyEntryFields`]). `itemize` is the sender's
/// already-correct 11-character `%i` string, carried verbatim because a remote
/// transfer has no local change-set to re-derive it from.
pub struct RemoteItemizeFields {
    /// Transfer-relative path of the entry.
    pub relative_path: PathBuf,
    /// The 11-character `%i` itemize string computed by the sender.
    pub itemize: String,
    /// POSIX mode bits (type + permissions).
    pub mode: u32,
    /// File length in bytes.
    pub size: u64,
    /// Modification time, whole seconds since the Unix epoch.
    pub mtime: i64,
    /// Modification time sub-second component, nanoseconds.
    pub mtime_nsec: u32,
    /// Owner uid, when `-o`/`--owner` carried it in the file list.
    pub uid: Option<u32>,
    /// Group gid, when `-g`/`--group` carried it in the file list.
    pub gid: Option<u32>,
    /// Whether the entry is a directory.
    pub is_dir: bool,
    /// Whether the entry is a symlink.
    pub is_symlink: bool,
    /// Symlink target, when the entry is a symlink.
    pub symlink_target: Option<PathBuf>,
    /// Whether the entry is newly created (`ITEM_IS_NEW`).
    pub is_new: bool,
    /// Whether the entry is a deletion.
    pub is_deletion: bool,
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

    /// Constructs metadata for a remote-transfer itemize row.
    ///
    /// Mirrors [`Self::from_list_only_entry`]'s mode-based type classification
    /// but carries the owner/group ids the sender put in the file list (`-o`/
    /// `-g`), which the `%U`/`%G` placeholders need. `atime`/`crtime` are not
    /// carried on the itemize path, so `accessed`/`created` stay unset.
    #[must_use]
    pub fn from_remote_itemize(fields: &RemoteItemizeFields) -> Self {
        let kind = if fields.is_symlink {
            ClientEntryKind::Symlink
        } else {
            match fields.mode & 0o170000 {
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
            length: fields.size,
            modified: secs_nsec_to_system_time(fields.mtime, fields.mtime_nsec),
            accessed: None,
            created: None,
            mode: Some(fields.mode),
            uid: fields.uid,
            gid: fields.gid,
            nlink: None,
            symlink_target: fields.symlink_target.clone(),
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

    fn fields(mode: u32) -> ListOnlyEntryFields {
        ListOnlyEntryFields {
            mode,
            size: 0,
            mtime: 0,
            mtime_nsec: 0,
            atime: 0,
            atime_nsec: 0,
            crtime: 0,
            crtime_nsec: 0,
            symlink_target: None,
            is_symlink: false,
        }
    }

    #[test]
    fn from_list_only_entry_classifies_each_ifmt_type() {
        // upstream: list_file_entry() derives the type char from S_IFMT.
        for (ifmt, expected) in [
            (0o100000, ClientEntryKind::File),
            (0o040000, ClientEntryKind::Directory),
            (0o120000, ClientEntryKind::Symlink),
            (0o010000, ClientEntryKind::Fifo),
            (0o020000, ClientEntryKind::CharDevice),
            (0o060000, ClientEntryKind::BlockDevice),
            (0o140000, ClientEntryKind::Socket),
        ] {
            // Permission bits in the low octals must not affect classification.
            let meta = ClientEntryMetadata::from_list_only_entry(&fields(ifmt | 0o644));
            assert_eq!(meta.kind(), expected, "mode {ifmt:o}");
            assert_eq!(meta.mode(), Some(ifmt | 0o644));
        }
    }

    #[test]
    fn from_list_only_entry_unknown_ifmt_is_other() {
        // 0o050000 is not a defined S_IFMT type.
        assert_eq!(
            ClientEntryMetadata::from_list_only_entry(&fields(0o050000)).kind(),
            ClientEntryKind::Other
        );
    }

    #[test]
    fn from_list_only_entry_is_symlink_flag_overrides_mode() {
        // The receiver sets is_symlink from the flist regardless of mode bits;
        // it must win even when the mode says regular file.
        let mut f = fields(0o100644);
        f.is_symlink = true;
        assert_eq!(
            ClientEntryMetadata::from_list_only_entry(&f).kind(),
            ClientEntryKind::Symlink
        );
    }

    #[test]
    fn from_list_only_entry_propagates_size_and_target() {
        let mut f = fields(0o120777);
        f.size = 4096;
        f.symlink_target = Some(PathBuf::from("../target"));
        f.is_symlink = true;
        let meta = ClientEntryMetadata::from_list_only_entry(&f);
        assert_eq!(meta.length(), 4096);
        assert_eq!(meta.symlink_target(), Some(Path::new("../target")));
    }

    #[test]
    fn from_list_only_entry_blank_times_are_none() {
        // Zero seconds + zero nsec must render blank, not 1970/01/01.
        let meta = ClientEntryMetadata::from_list_only_entry(&fields(0o100644));
        assert_eq!(meta.modified(), None);
        assert_eq!(meta.accessed(), None);
        assert_eq!(meta.created(), None);
    }

    #[test]
    fn from_list_only_entry_populates_times_when_set() {
        let mut f = fields(0o100644);
        f.mtime = 1_000_000;
        f.atime = 2_000_000;
        f.crtime = 3_000_000;
        let meta = ClientEntryMetadata::from_list_only_entry(&f);
        assert_eq!(
            meta.modified(),
            Some(SystemTime::UNIX_EPOCH + Duration::new(1_000_000, 0))
        );
        assert_eq!(
            meta.accessed(),
            Some(SystemTime::UNIX_EPOCH + Duration::new(2_000_000, 0))
        );
        assert_eq!(
            meta.created(),
            Some(SystemTime::UNIX_EPOCH + Duration::new(3_000_000, 0))
        );
    }

    #[test]
    fn secs_nsec_zero_is_none() {
        assert_eq!(secs_nsec_to_system_time(0, 0), None);
    }

    #[test]
    fn secs_nsec_negative_seconds_is_none() {
        // Pre-epoch timestamps are not representable here and render blank.
        assert_eq!(secs_nsec_to_system_time(-1, 0), None);
        assert_eq!(secs_nsec_to_system_time(-1, 500), None);
    }

    #[test]
    fn secs_nsec_nanos_only_is_some() {
        // A nonzero sub-second component alone is a valid post-epoch instant.
        assert_eq!(
            secs_nsec_to_system_time(0, 1),
            Some(SystemTime::UNIX_EPOCH + Duration::new(0, 1))
        );
    }

    #[test]
    fn secs_nsec_positive_seconds_with_nanos() {
        assert_eq!(
            secs_nsec_to_system_time(42, 123),
            Some(SystemTime::UNIX_EPOCH + Duration::new(42, 123))
        );
    }
}
