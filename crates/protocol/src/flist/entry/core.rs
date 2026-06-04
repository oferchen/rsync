use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::extras::FileEntryExtras;

/// Presence bit: the `uid` field holds a meaningful value.
///
/// Mirrors the conditional `uid` extra in upstream rsync's `file_struct`
/// (upstream: rsync.h `F_OWNER`). Cleared when ownership is not preserved.
pub(super) const PRESENT_UID: u8 = 1 << 0;
/// Presence bit: the `gid` field holds a meaningful value.
///
/// Mirrors the conditional `gid` extra in upstream rsync's `file_struct`
/// (upstream: rsync.h `F_GROUP`). Cleared when ownership is not preserved.
pub(super) const PRESENT_GID: u8 = 1 << 1;
/// Presence bit: this directory has content to transfer (protocol 30+).
///
/// Set by default for directories; cleared for XMIT_NO_CONTENT_DIR (implied or
/// content-less) directories. Non-directories report `true` via the accessor.
pub(super) const PRESENT_CONTENT_DIR: u8 = 1 << 2;
/// Presence bit: this is a top-level directory in the transfer.
///
/// Corresponds to upstream's `FLAG_TOP_DIR` (rsync.h). Used to scope
/// `--delete` to top-level directories. Packed here instead of in a
/// separate `FileFlags` struct to save 3 bytes per entry.
pub(super) const PRESENT_TOP_DIR: u8 = 1 << 3;
/// Presence bit: this entry has hardlink information.
///
/// Corresponds to upstream's `FLAG_HLINKED` (rsync.h). Indicates the entry
/// belongs to a hardlink group. Packed here to avoid per-entry `FileFlags`.
pub(super) const PRESENT_HLINKED: u8 = 1 << 4;
/// Presence bit: this is the first (leader) entry in a hardlink group.
///
/// Corresponds to upstream's `FLAG_HLINK_FIRST` (rsync.h). Only meaningful
/// when `PRESENT_HLINKED` is also set. Packed here to avoid per-entry
/// `FileFlags`.
pub(super) const PRESENT_HLINK_FIRST: u8 = 1 << 5;

/// A single entry in the rsync file list.
///
/// Contains all metadata needed to synchronize a filesystem object, including
/// the relative path, size, modification time, mode, ownership, and optional
/// device/symlink information.
///
/// # Memory Layout
///
/// Hot-path fields (name, size, mtime, mode, uid/gid) are stored inline.
/// Rarely-used fields (symlink targets, device numbers, hardlink info,
/// ACL/xattr indices, atime/crtime) are stored in a boxed `FileEntryExtras`
/// that is only allocated when at least one such field is set. This reduces
/// the common-case inline size from ~295 bytes to ~80 bytes - matching
/// upstream rsync's conditional field allocation pattern.
///
/// `uid`, `gid`, the directory content flag, and the three persisted wire
/// flags (`top_dir`, `hlinked`, `hlink_first`) are packed into a single
/// `present: u8` bitfield. This eliminates the 3-byte `FileFlags` struct
/// that previously stored 24 bits of wire-encoding state, of which only 3
/// bits survived past decoding. The remaining XMIT flags (same_uid,
/// same_gid, same_mode, same_time, etc.) are transient wire-encoding state
/// recomputed by `FileListWriter::calculate_xflags()` during send.
///
/// The `mode` field uses `u16` to match upstream rsync's `uint16 mode`
/// (rsync.h:805), saving 2 bytes vs the prior `u32`. POSIX mode values
/// (4-bit type + 12-bit permissions) fit in 16 bits; the accessor returns
/// `u32` for API compatibility.
///
/// # Path Interning
///
/// The `dirname` field holds a reference-counted parent directory path that can
/// be shared across entries in the same directory. When entries are built through
/// [`super::super::read::FileListReader`], the reader's
/// [`super::super::intern::PathInterner`] ensures that entries sharing a parent
/// directory point to the same `Arc<Path>` allocation. This mirrors upstream
/// rsync's `file_struct.dirname` shared pointer (upstream: flist.c).
///
/// Field order is optimized to minimize padding: 8-byte aligned fields first,
/// then 4-byte, then smaller fields.
pub struct FileEntry {
    // 8-byte aligned fields
    /// Relative path of the entry within the transfer.
    pub(super) name: PathBuf,
    /// Interned parent directory path, shared across entries in the same directory.
    ///
    /// For a path like `"src/lib/foo.rs"`, dirname is `"src/lib"`. For root-level
    /// entries like `"foo.rs"`, dirname is the empty path `""`. When set by the
    /// `PathInterner`, multiple entries with the same parent share a single
    /// heap allocation via `Arc`.
    pub(super) dirname: Arc<Path>,
    /// File size in bytes (0 for directories and special files).
    pub(super) size: u64,
    /// Modification time as seconds since Unix epoch.
    pub(super) mtime: i64,
    /// Rarely-used fields, boxed to reduce inline size.
    ///
    /// `None` for regular files in typical transfers (no symlinks, devices,
    /// hardlinks, ACLs, xattrs, atimes, crtimes, or checksums).
    pub(super) extras: Option<Box<FileEntryExtras>>,

    // 4-byte aligned fields
    /// User ID raw value. Meaningful only when `PRESENT_UID` is set in `present`.
    pub(super) uid: u32,
    /// Group ID raw value. Meaningful only when `PRESENT_GID` is set in `present`.
    pub(super) gid: u32,
    /// Modification time nanoseconds (protocol 31+).
    pub(super) mtime_nsec: u32,

    // 2-byte aligned fields
    /// Unix mode bits (type + permissions).
    ///
    /// Stored as `u16` matching upstream rsync's `uint16 mode` (rsync.h:805).
    /// POSIX mode values fit in 16 bits (4-bit type mask `S_IFMT` = `0o170000`
    /// + 12-bit permissions `0o7777`). The accessor returns `u32` for API
    /// compatibility with callers that use `u32` mode constants.
    pub(super) mode: u16,

    // 1-byte aligned fields
    /// Presence bitfield for metadata and persisted wire flags.
    ///
    /// Bits 0-2: metadata presence (`PRESENT_UID`, `PRESENT_GID`,
    /// `PRESENT_CONTENT_DIR`).
    /// Bits 3-5: persisted wire flags (`PRESENT_TOP_DIR`, `PRESENT_HLINKED`,
    /// `PRESENT_HLINK_FIRST`). These are the only 3 of 24 XMIT wire flags
    /// that survive past decoding - the rest are transient delta-encoding
    /// state recomputed during send.
    /// Bits 6-7: reserved.
    pub(super) present: u8,
}

/// Extracts the parent directory from a path.
///
/// Returns the parent component as `Arc<Path>`. For paths without a directory
/// separator (root-level entries), returns an `Arc` pointing to the empty path.
pub(super) fn extract_dirname(path: &Path) -> Arc<Path> {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => Arc::from(parent),
        _ => Arc::from(Path::new("")),
    }
}

impl Clone for FileEntry {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            dirname: Arc::clone(&self.dirname),
            size: self.size,
            mtime: self.mtime,
            extras: self.extras.clone(),
            uid: self.uid,
            gid: self.gid,
            mtime_nsec: self.mtime_nsec,
            mode: self.mode,
            present: self.present,
        }
    }
}

impl std::fmt::Debug for FileEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("FileEntry");
        s.field("name", &self.name)
            .field("dirname", &self.dirname)
            .field("size", &self.size)
            .field("mtime", &self.mtime)
            .field("uid", &self.uid())
            .field("gid", &self.gid())
            .field("mode", &self.mode())
            .field("mtime_nsec", &self.mtime_nsec)
            .field("present", &self.present)
            .field("content_dir", &self.content_dir())
            .field("top_dir", &self.top_dir())
            .field("hlinked", &self.hlinked())
            .field("hlink_first", &self.hlink_first());
        if let Some(extras) = &self.extras {
            s.field("extras", extras);
        }
        s.finish()
    }
}

/// `dirname` is derived from `name`, so equality ignores it.
impl PartialEq for FileEntry {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.size == other.size
            && self.mtime == other.mtime
            && self.uid() == other.uid()
            && self.gid() == other.gid()
            && self.mode == other.mode
            && self.mtime_nsec == other.mtime_nsec
            && self.present == other.present
            && self.extras == other.extras
    }
}

impl Eq for FileEntry {}
