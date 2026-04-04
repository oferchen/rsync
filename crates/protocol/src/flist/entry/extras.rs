use std::path::PathBuf;

use crate::xattr::XattrList;

/// Rarely-used metadata fields for file entries.
///
/// These fields are only populated when specific flags are active (e.g.
/// `--hard-links`, `--devices`, `--acls`, `--xattrs`, `--atimes`, `--crtimes`,
/// `--checksum`). Storing them behind `Option<Box<...>>` in `FileEntry` avoids
/// ~200 bytes of inline overhead per entry when they're unused - matching
/// upstream rsync's conditional field allocation in `file_struct`
/// (upstream: flist.c:make_file()).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct FileEntryExtras {
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
    /// Access time nanoseconds (protocol 32+, --atimes).
    pub(super) atime_nsec: u32,
    /// Device major number (for block/char devices).
    pub(super) rdev_major: Option<u32>,
    /// Device minor number (for block/char devices).
    pub(super) rdev_minor: Option<u32>,
    /// Hardlink index (for hardlink preservation).
    pub(super) hardlink_idx: Option<u32>,
    /// Hardlink device number (for protocol < 30 hardlink deduplication).
    pub(super) hardlink_dev: Option<i64>,
    /// Hardlink inode number (for protocol < 30 hardlink deduplication).
    pub(super) hardlink_ino: Option<i64>,
    /// File checksum for --checksum mode (variable length, up to 32 bytes).
    pub(super) checksum: Option<Vec<u8>>,
    /// Access ACL index for --acls mode (index into access ACL list, protocol 30+).
    pub(super) acl_ndx: Option<u32>,
    /// Default ACL index for directories in --acls mode (index into default ACL list).
    ///
    /// Only meaningful for directories. Corresponds to upstream's `F_DIR_DEFACL`.
    pub(super) def_acl_ndx: Option<u32>,
    /// Extended attribute index for --xattrs mode (index into xattr list).
    pub(super) xattr_ndx: Option<u32>,
    /// Sender-side xattr data read from the filesystem.
    ///
    /// Populated by the generator when `--xattrs` is active. Names are in
    /// wire format (translated via `local_to_wire()`). The writer sends this
    /// data on the wire and does not need to know the filesystem path.
    ///
    /// On the receiver side this field is unused - the receiver uses
    /// `xattr_ndx` to look up data in the `XattrCache`.
    pub(super) xattr_list: Option<XattrList>,
}
