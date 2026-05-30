use std::path::PathBuf;

use crate::xattr::XattrList;

/// Presence bit: `rdev_major` and `rdev_minor` hold meaningful values.
pub(super) const EXTRAS_PRESENT_RDEV: u16 = 1 << 0;
/// Presence bit: `hardlink_idx` holds a meaningful value.
pub(super) const EXTRAS_PRESENT_HARDLINK_IDX: u16 = 1 << 1;
/// Presence bit: `acl_ndx` holds a meaningful value.
pub(super) const EXTRAS_PRESENT_ACL_NDX: u16 = 1 << 2;
/// Presence bit: `def_acl_ndx` holds a meaningful value.
pub(super) const EXTRAS_PRESENT_DEF_ACL_NDX: u16 = 1 << 3;
/// Presence bit: `xattr_ndx` holds a meaningful value.
pub(super) const EXTRAS_PRESENT_XATTR_NDX: u16 = 1 << 4;
/// Presence bit: `hardlink_dev` and `hardlink_ino` hold meaningful values.
pub(super) const EXTRAS_PRESENT_HARDLINK_DEV: u16 = 1 << 5;

/// Rarely-used metadata fields for file entries.
///
/// These fields are only populated when specific flags are active (e.g.
/// `--hard-links`, `--devices`, `--acls`, `--xattrs`, `--atimes`, `--crtimes`,
/// `--checksum`). Storing them behind `Option<Box<...>>` in `FileEntry` avoids
/// ~200 bytes of inline overhead per entry when they're unused - matching
/// upstream rsync's conditional field allocation in `file_struct`
/// (upstream: flist.c:make_file()).
///
/// Fields that have a load-bearing `None` vs `Some(0)` distinction (rdev,
/// hardlink_idx, acl_ndx, def_acl_ndx, xattr_ndx, hardlink_dev, hardlink_ino)
/// use raw storage with a `present` bitfield instead of `Option<T>`. This
/// mirrors the `PRESENT_UID`/`PRESENT_GID` compaction on `FileEntry` and
/// saves 8-16 bytes of discriminant+padding per `Option`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct FileEntryExtras {
    // 8-byte aligned fields first.
    /// Symlink target path (for symlinks).
    pub(super) link_target: Option<PathBuf>,
    /// User name for cross-system ownership mapping (protocol 30+).
    pub(super) user_name: Option<String>,
    /// Group name for cross-system ownership mapping (protocol 30+).
    pub(super) group_name: Option<String>,
    /// Access time as seconds since Unix epoch (protocol 30+, --atimes).
    pub(super) atime: i64,
    /// Creation time as seconds since Unix epoch (protocol 30+, --crtimes).
    pub(super) crtime: i64,
    /// Hardlink device number (for protocol < 30 hardlink deduplication).
    /// Meaningful only when `EXTRAS_PRESENT_HARDLINK_DEV` is set.
    pub(super) hardlink_dev: i64,
    /// Hardlink inode number (for protocol < 30 hardlink deduplication).
    /// Meaningful only when `EXTRAS_PRESENT_HARDLINK_DEV` is set.
    pub(super) hardlink_ino: i64,
    /// File checksum for --checksum mode (variable length, up to 32 bytes).
    pub(super) checksum: Option<Vec<u8>>,
    /// Sender-side xattr data read from the filesystem.
    ///
    /// Populated by the generator when `--xattrs` is active. Names are in
    /// wire format (translated via `local_to_wire()`). The writer sends this
    /// data on the wire and does not need to know the filesystem path.
    ///
    /// On the receiver side this field is unused - the receiver uses
    /// `xattr_ndx` to look up data in the `XattrCache`.
    pub(super) xattr_list: Option<XattrList>,

    // 4-byte aligned fields.
    /// Access time nanoseconds (protocol 32+, --atimes).
    pub(super) atime_nsec: u32,
    /// Device major number (for block/char devices).
    /// Meaningful only when `EXTRAS_PRESENT_RDEV` is set.
    pub(super) rdev_major: u32,
    /// Device minor number (for block/char devices).
    /// Meaningful only when `EXTRAS_PRESENT_RDEV` is set.
    pub(super) rdev_minor: u32,
    /// Hardlink index (for hardlink preservation).
    /// Meaningful only when `EXTRAS_PRESENT_HARDLINK_IDX` is set.
    pub(super) hardlink_idx: u32,
    /// Access ACL index for --acls mode (index into access ACL list, protocol 30+).
    /// Meaningful only when `EXTRAS_PRESENT_ACL_NDX` is set.
    pub(super) acl_ndx: u32,
    /// Default ACL index for directories in --acls mode (index into default ACL list).
    /// Meaningful only when `EXTRAS_PRESENT_DEF_ACL_NDX` is set.
    /// Corresponds to upstream's `F_DIR_DEFACL`.
    pub(super) def_acl_ndx: u32,
    /// Extended attribute index for --xattrs mode (index into xattr list).
    /// Meaningful only when `EXTRAS_PRESENT_XATTR_NDX` is set.
    pub(super) xattr_ndx: u32,

    // 2-byte aligned fields.
    /// Presence bitfield for compacted Option fields.
    ///
    /// See `EXTRAS_PRESENT_*` constants.
    pub(super) present: u16,
}
