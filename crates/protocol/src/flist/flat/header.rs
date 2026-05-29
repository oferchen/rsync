//! Fixed-size header node for the flat file-list backing store.
//!
//! See `docs/design/flat-flist-representation.md` for the full design.

/// Interned name/dirname handle for the flat file-list store.
///
/// A 4-byte index into the flat store's own name/dirname interner. This is
/// a placeholder: the real interner that resolves a handle to a string is
/// built later in RSS-A.5.c. For now the type only carries the index and a
/// null sentinel so [`FileEntryHeader`] can name its `name`/`dirname`
/// fields with the final shape.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PathHandle(pub u32);

impl PathHandle {
    /// Null/empty sentinel: the entry has no interned string for this slot.
    pub const NONE: PathHandle = PathHandle(u32::MAX);
}

/// Reference to a packed extras tail in the flat store's blob arena.
///
/// A 4-byte arena offset into the non-interned blob region that holds the
/// variable-length extras record (symlink target, device numbers, hardlink
/// data, ACL/xattr indices, checksum, user/group names). Like
/// [`PathHandle`] this is a placeholder for the phase-1 header; the arena
/// it indexes into is built later.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ExtrasRef(pub u32);

impl ExtrasRef {
    /// Sentinel for entries with no extras tail (the common case).
    pub const NO_EXTRAS: ExtrasRef = ExtrasRef(u32::MAX);
}

/// Presence bit: the [`FileEntryHeader::uid`] field is meaningful.
pub const PRESENT_UID: u16 = 1 << 0;
/// Presence bit: the [`FileEntryHeader::gid`] field is meaningful.
pub const PRESENT_GID: u16 = 1 << 1;
/// Presence bit: the [`FileEntryHeader::mtime_nsec`] field is meaningful.
pub const PRESENT_MTIME_NSEC: u16 = 1 << 2;
/// Presence bit: the entry is a directory carrying content (protocol 30+).
///
/// Mirrors the legacy `FileEntry::content_dir` flag; absence corresponds to
/// upstream's `XMIT_NO_CONTENT_DIR`.
pub const PRESENT_CONTENT_DIR: u16 = 1 << 3;
/// Presence bit: the size needs the full 64-bit `size` field.
///
/// Mirrors upstream's `FLAG_LENGTH64`; when clear, the size fits in 32 bits.
pub const PRESENT_LENGTH64: u16 = 1 << 4;

/// Fixed-size header for one file-list entry in the flat backing store.
///
/// Holds inline scalar metadata plus arena references to variable-length
/// tails (name, dirname, and optional extras blob). Headers are allocated
/// in build order and never moved after insertion; sort order is expressed
/// through a separate `index: Vec<u32>` permutation (RSS-A.6).
///
/// Field order follows the design's padding-minimizing layout, yielding a
/// 48-byte node with no tail padding on 64-bit targets - the low end of the
/// design's 48-64 byte target.
///
/// The `uid`, `gid`, and `mtime_nsec` fields are meaningful only when their
/// corresponding [`present`](FileEntryHeader::present) bit is set; read them
/// through [`uid`](FileEntryHeader::uid) / [`gid`](FileEntryHeader::gid) /
/// [`mtime_nsec`](FileEntryHeader::mtime_nsec), which return `None` when the
/// bit is clear.
#[derive(Clone, Copy)]
pub struct FileEntryHeader {
    /// Modification time, seconds since the Unix epoch.
    pub mtime: i64,
    /// File size in bytes (0 for directories and special files).
    pub size: u64,
    /// User ID; meaningful only when [`PRESENT_UID`] is set in `present`.
    pub uid: u32,
    /// Group ID; meaningful only when [`PRESENT_GID`] is set in `present`.
    pub gid: u32,
    /// Interned name handle, or [`PathHandle::NONE`].
    pub name: PathHandle,
    /// Interned dirname handle, or [`PathHandle::NONE`].
    pub dirname: PathHandle,
    /// Packed extras tail reference, or [`ExtrasRef::NO_EXTRAS`].
    pub extras: ExtrasRef,
    /// Modification time nanoseconds; meaningful only when
    /// [`PRESENT_MTIME_NSEC`] is set in `present` (protocol 31+).
    pub mtime_nsec: u32,
    /// Unix mode bits (file type + permissions).
    pub mode: u32,
    /// Wire flags (the legacy `FileFlags` bits packed into a `u16`).
    pub flags: u16,
    /// Presence bitfield: which optional inline fields are set.
    ///
    /// See the `PRESENT_*` constants ([`PRESENT_UID`], [`PRESENT_GID`],
    /// [`PRESENT_MTIME_NSEC`], [`PRESENT_CONTENT_DIR`], [`PRESENT_LENGTH64`]).
    pub present: u16,
}

// The header must fit the design's 48-64 byte target. Assert the upper
// bound rather than an exact value to stay robust to per-target padding.
const _: () = assert!(core::mem::size_of::<FileEntryHeader>() <= 64);

impl FileEntryHeader {
    /// Returns whether the given presence `bit` is set in `present`.
    #[must_use]
    pub fn has(&self, bit: u16) -> bool {
        self.present & bit != 0
    }

    /// Sets the given presence `bit` in `present`.
    pub fn set(&mut self, bit: u16) {
        self.present |= bit;
    }

    /// Returns the user ID, or `None` when [`PRESENT_UID`] is clear.
    #[must_use]
    pub fn uid(&self) -> Option<u32> {
        self.has(PRESENT_UID).then_some(self.uid)
    }

    /// Returns the group ID, or `None` when [`PRESENT_GID`] is clear.
    #[must_use]
    pub fn gid(&self) -> Option<u32> {
        self.has(PRESENT_GID).then_some(self.gid)
    }

    /// Returns the mtime nanoseconds, or `None` when [`PRESENT_MTIME_NSEC`]
    /// is clear.
    #[must_use]
    pub fn mtime_nsec(&self) -> Option<u32> {
        self.has(PRESENT_MTIME_NSEC).then_some(self.mtime_nsec)
    }
}
