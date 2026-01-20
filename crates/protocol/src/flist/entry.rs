//! File entry representation for the rsync file list.
//!
//! A file entry contains all metadata needed to synchronize a single filesystem
//! object (regular file, directory, symlink, device, etc.).

use std::path::PathBuf;

/// Type of filesystem entry.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum FileType {
    /// Regular file.
    Regular,
    /// Directory.
    Directory,
    /// Symbolic link.
    Symlink,
    /// Block device.
    BlockDevice,
    /// Character device.
    CharDevice,
    /// Named pipe (FIFO).
    Fifo,
    /// Unix domain socket.
    Socket,
}

impl FileType {
    /// Extracts the file type from Unix mode bits.
    ///
    /// The file type is encoded in the upper 4 bits of the mode (S_IFMT mask).
    #[must_use]
    pub const fn from_mode(mode: u32) -> Option<Self> {
        // S_IFMT = 0o170000
        match mode & 0o170000 {
            0o100000 => Some(Self::Regular),     // S_IFREG
            0o040000 => Some(Self::Directory),   // S_IFDIR
            0o120000 => Some(Self::Symlink),     // S_IFLNK
            0o060000 => Some(Self::BlockDevice), // S_IFBLK
            0o020000 => Some(Self::CharDevice),  // S_IFCHR
            0o010000 => Some(Self::Fifo),        // S_IFIFO
            0o140000 => Some(Self::Socket),      // S_IFSOCK
            _ => None,
        }
    }

    /// Returns the mode bits for this file type.
    #[must_use]
    pub const fn to_mode_bits(self) -> u32 {
        match self {
            Self::Regular => 0o100000,
            Self::Directory => 0o040000,
            Self::Symlink => 0o120000,
            Self::BlockDevice => 0o060000,
            Self::CharDevice => 0o020000,
            Self::Fifo => 0o010000,
            Self::Socket => 0o140000,
        }
    }

    /// Returns true if this type represents a regular file.
    #[must_use]
    pub const fn is_regular(self) -> bool {
        matches!(self, Self::Regular)
    }

    /// Returns true if this type represents a directory.
    #[must_use]
    pub const fn is_dir(self) -> bool {
        matches!(self, Self::Directory)
    }

    /// Returns true if this type represents a symbolic link.
    #[must_use]
    pub const fn is_symlink(self) -> bool {
        matches!(self, Self::Symlink)
    }

    /// Returns true if this type represents a device (block or character).
    #[must_use]
    pub const fn is_device(self) -> bool {
        matches!(self, Self::BlockDevice | Self::CharDevice)
    }
}

/// A single entry in the rsync file list.
///
/// Contains all metadata needed to synchronize a filesystem object, including
/// the relative path, size, modification time, mode, ownership, and optional
/// device/symlink information.
///
/// Field order is optimized to minimize padding: 8-byte aligned fields first,
/// then 4-byte, then smaller fields.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FileEntry {
    // 8-byte aligned fields (24 bytes each)
    /// Relative path of the entry within the transfer.
    name: PathBuf,
    /// Symlink target path (for symlinks).
    link_target: Option<PathBuf>,
    /// User name for cross-system ownership mapping (protocol 30+).
    user_name: Option<String>,
    /// Group name for cross-system ownership mapping (protocol 30+).
    group_name: Option<String>,

    // 8-byte aligned fields
    /// File size in bytes (0 for directories and special files).
    size: u64,
    /// Modification time as seconds since Unix epoch.
    mtime: i64,
    /// Access time as seconds since Unix epoch (protocol 30+, --atimes).
    atime: i64,
    /// Creation time as seconds since Unix epoch (protocol 30+, --crtimes).
    crtime: i64,
    /// User ID (None if not preserving ownership).
    uid: Option<u32>,
    /// Group ID (None if not preserving ownership).
    gid: Option<u32>,
    /// Device major number (for block/char devices).
    rdev_major: Option<u32>,
    /// Device minor number (for block/char devices).
    rdev_minor: Option<u32>,
    /// Hardlink index (for hardlink preservation).
    hardlink_idx: Option<u32>,
    /// Hardlink device number (for protocol < 30 hardlink deduplication).
    hardlink_dev: Option<i64>,
    /// Hardlink inode number (for protocol < 30 hardlink deduplication).
    hardlink_ino: Option<i64>,
    /// File checksum for --checksum mode (variable length, up to 32 bytes).
    checksum: Option<Vec<u8>>,
    /// ACL index for --acls mode (index into ACL list, protocol 30+).
    acl_ndx: Option<u32>,
    /// Extended attribute index for --xattrs mode (index into xattr list).
    xattr_ndx: Option<u32>,

    // 4-byte aligned fields
    /// Unix mode bits (type + permissions).
    mode: u32,
    /// Modification time nanoseconds (protocol 31+).
    mtime_nsec: u32,

    // 2-byte aligned fields
    /// Entry flags from wire format.
    flags: super::flags::FileFlags,

    // 1-byte aligned fields
    /// Whether this directory has content to transfer (protocol 30+).
    ///
    /// False indicates XMIT_NO_CONTENT_DIR - an implied or content-less directory.
    content_dir: bool,
}

impl FileEntry {
    /// Core constructor with all parameters - Template Method pattern.
    ///
    /// All public constructors delegate to this method to ensure consistent
    /// initialization and reduce code duplication.
    #[inline]
    fn new_with_type(
        name: PathBuf,
        size: u64,
        file_type: FileType,
        permissions: u32,
        link_target: Option<PathBuf>,
    ) -> Self {
        Self {
            name,
            link_target,
            user_name: None,
            group_name: None,
            size,
            mtime: 0,
            atime: 0,
            crtime: 0,
            uid: None,
            gid: None,
            rdev_major: None,
            rdev_minor: None,
            hardlink_idx: None,
            hardlink_dev: None,
            hardlink_ino: None,
            checksum: None,
            acl_ndx: None,
            xattr_ndx: None,
            mode: file_type.to_mode_bits() | (permissions & 0o7777),
            mtime_nsec: 0,
            flags: super::flags::FileFlags::default(),
            content_dir: true, // Directories have content by default
        }
    }

    /// Creates a new regular file entry.
    #[must_use]
    pub fn new_file(name: PathBuf, size: u64, permissions: u32) -> Self {
        Self::new_with_type(name, size, FileType::Regular, permissions, None)
    }

    /// Creates a new directory entry.
    #[must_use]
    pub fn new_directory(name: PathBuf, permissions: u32) -> Self {
        Self::new_with_type(name, 0, FileType::Directory, permissions, None)
    }

    /// Creates a new symlink entry.
    #[must_use]
    pub fn new_symlink(name: PathBuf, target: PathBuf) -> Self {
        Self::new_with_type(name, 0, FileType::Symlink, 0o777, Some(target))
    }

    /// Creates a new block device entry.
    #[must_use]
    pub fn new_block_device(name: PathBuf, permissions: u32, major: u32, minor: u32) -> Self {
        let mut entry = Self::new_with_type(name, 0, FileType::BlockDevice, permissions, None);
        entry.set_rdev(major, minor);
        entry
    }

    /// Creates a new character device entry.
    #[must_use]
    pub fn new_char_device(name: PathBuf, permissions: u32, major: u32, minor: u32) -> Self {
        let mut entry = Self::new_with_type(name, 0, FileType::CharDevice, permissions, None);
        entry.set_rdev(major, minor);
        entry
    }

    /// Creates a new FIFO (named pipe) entry.
    #[must_use]
    pub fn new_fifo(name: PathBuf, permissions: u32) -> Self {
        Self::new_with_type(name, 0, FileType::Fifo, permissions, None)
    }

    /// Creates a new Unix domain socket entry.
    #[must_use]
    pub fn new_socket(name: PathBuf, permissions: u32) -> Self {
        Self::new_with_type(name, 0, FileType::Socket, permissions, None)
    }

    /// Creates a file entry from raw components (used during decoding).
    #[must_use]
    pub(crate) const fn from_raw(
        name: PathBuf,
        size: u64,
        mode: u32,
        mtime: i64,
        mtime_nsec: u32,
        flags: super::flags::FileFlags,
    ) -> Self {
        Self {
            name,
            link_target: None,
            user_name: None,
            group_name: None,
            size,
            mtime,
            atime: 0,
            crtime: 0,
            uid: None,
            gid: None,
            rdev_major: None,
            rdev_minor: None,
            hardlink_idx: None,
            hardlink_dev: None,
            hardlink_ino: None,
            checksum: None,
            acl_ndx: None,
            xattr_ndx: None,
            mode,
            mtime_nsec,
            flags,
            content_dir: true, // Default to having content
        }
    }

    /// Returns the relative path name of the entry.
    #[must_use]
    pub fn name(&self) -> &str {
        self.name.to_str().unwrap_or("")
    }

    /// Returns the relative path of the entry.
    #[must_use]
    pub const fn path(&self) -> &PathBuf {
        &self.name
    }

    /// Returns the file size in bytes.
    #[inline]
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// Returns the Unix mode bits (type + permissions).
    #[inline]
    #[must_use]
    pub const fn mode(&self) -> u32 {
        self.mode
    }

    /// Returns the permission bits only (without type).
    #[inline]
    #[must_use]
    pub const fn permissions(&self) -> u32 {
        self.mode & 0o7777
    }

    /// Returns the file type.
    #[must_use]
    pub fn file_type(&self) -> FileType {
        FileType::from_mode(self.mode).unwrap_or(FileType::Regular)
    }

    /// Returns the modification time as seconds since Unix epoch.
    #[inline]
    #[must_use]
    pub const fn mtime(&self) -> i64 {
        self.mtime
    }

    /// Returns the modification time nanoseconds.
    #[inline]
    #[must_use]
    pub const fn mtime_nsec(&self) -> u32 {
        self.mtime_nsec
    }

    /// Returns the user ID if set.
    #[inline]
    #[must_use]
    pub const fn uid(&self) -> Option<u32> {
        self.uid
    }

    /// Returns the group ID if set.
    #[inline]
    #[must_use]
    pub const fn gid(&self) -> Option<u32> {
        self.gid
    }

    /// Returns the symlink target if this is a symlink.
    #[must_use]
    pub const fn link_target(&self) -> Option<&PathBuf> {
        self.link_target.as_ref()
    }

    /// Returns the device major number if this is a device.
    #[must_use]
    pub const fn rdev_major(&self) -> Option<u32> {
        self.rdev_major
    }

    /// Returns the device minor number if this is a device.
    #[must_use]
    pub const fn rdev_minor(&self) -> Option<u32> {
        self.rdev_minor
    }

    /// Returns the wire format flags.
    #[must_use]
    pub const fn flags(&self) -> super::flags::FileFlags {
        self.flags
    }

    /// Returns true if this entry is a directory.
    #[inline]
    #[must_use]
    pub const fn is_dir(&self) -> bool {
        self.mode & 0o170000 == 0o040000 // S_IFDIR
    }

    /// Returns true if this entry is a regular file.
    #[inline]
    #[must_use]
    pub const fn is_file(&self) -> bool {
        self.mode & 0o170000 == 0o100000 // S_IFREG
    }

    /// Returns true if this entry is a symbolic link.
    #[inline]
    #[must_use]
    pub const fn is_symlink(&self) -> bool {
        self.mode & 0o170000 == 0o120000 // S_IFLNK
    }

    /// Sets the modification time.
    pub const fn set_mtime(&mut self, secs: i64, nsec: u32) {
        self.mtime = secs;
        self.mtime_nsec = nsec;
    }

    /// Sets the user ID.
    pub const fn set_uid(&mut self, uid: u32) {
        self.uid = Some(uid);
    }

    /// Sets the group ID.
    pub const fn set_gid(&mut self, gid: u32) {
        self.gid = Some(gid);
    }

    /// Returns the user name if set.
    #[must_use]
    pub fn user_name(&self) -> Option<&str> {
        self.user_name.as_deref()
    }

    /// Sets the user name for cross-system ownership mapping.
    pub fn set_user_name(&mut self, name: String) {
        self.user_name = Some(name);
    }

    /// Returns the group name if set.
    #[must_use]
    pub fn group_name(&self) -> Option<&str> {
        self.group_name.as_deref()
    }

    /// Sets the group name for cross-system ownership mapping.
    pub fn set_group_name(&mut self, name: String) {
        self.group_name = Some(name);
    }

    /// Sets the symlink target.
    pub fn set_link_target(&mut self, target: PathBuf) {
        self.link_target = Some(target);
    }

    /// Sets the device numbers.
    pub const fn set_rdev(&mut self, major: u32, minor: u32) {
        self.rdev_major = Some(major);
        self.rdev_minor = Some(minor);
    }

    /// Returns the hardlink index if this entry is a hardlink.
    #[must_use]
    pub const fn hardlink_idx(&self) -> Option<u32> {
        self.hardlink_idx
    }

    /// Sets the hardlink index for this entry.
    pub const fn set_hardlink_idx(&mut self, idx: u32) {
        self.hardlink_idx = Some(idx);
    }

    /// Returns the access time as seconds since Unix epoch.
    #[inline]
    #[must_use]
    pub const fn atime(&self) -> i64 {
        self.atime
    }

    /// Sets the access time.
    pub const fn set_atime(&mut self, secs: i64) {
        self.atime = secs;
    }

    /// Returns the creation time as seconds since Unix epoch.
    #[inline]
    #[must_use]
    pub const fn crtime(&self) -> i64 {
        self.crtime
    }

    /// Sets the creation time.
    pub const fn set_crtime(&mut self, secs: i64) {
        self.crtime = secs;
    }

    /// Returns whether this directory has content to transfer.
    ///
    /// Only meaningful for directories. Returns true for non-directories.
    #[inline]
    #[must_use]
    pub const fn content_dir(&self) -> bool {
        self.content_dir
    }

    /// Sets whether this directory has content to transfer.
    ///
    /// When false, XMIT_NO_CONTENT_DIR flag is set on wire.
    pub const fn set_content_dir(&mut self, has_content: bool) {
        self.content_dir = has_content;
    }

    /// Returns the hardlink device number (for protocol < 30).
    #[inline]
    #[must_use]
    pub const fn hardlink_dev(&self) -> Option<i64> {
        self.hardlink_dev
    }

    /// Sets the hardlink device number (for protocol < 30).
    pub const fn set_hardlink_dev(&mut self, dev: i64) {
        self.hardlink_dev = Some(dev);
    }

    /// Returns the hardlink inode number (for protocol < 30).
    #[inline]
    #[must_use]
    pub const fn hardlink_ino(&self) -> Option<i64> {
        self.hardlink_ino
    }

    /// Sets the hardlink inode number (for protocol < 30).
    pub const fn set_hardlink_ino(&mut self, ino: i64) {
        self.hardlink_ino = Some(ino);
    }

    /// Returns the file checksum if set (for --checksum mode).
    #[inline]
    #[must_use]
    pub fn checksum(&self) -> Option<&[u8]> {
        self.checksum.as_deref()
    }

    /// Sets the file checksum (for --checksum mode).
    pub fn set_checksum(&mut self, sum: Vec<u8>) {
        self.checksum = Some(sum);
    }

    /// Returns the ACL index if set (for --acls mode).
    #[inline]
    #[must_use]
    pub const fn acl_ndx(&self) -> Option<u32> {
        self.acl_ndx
    }

    /// Sets the ACL index (for --acls mode).
    pub const fn set_acl_ndx(&mut self, ndx: u32) {
        self.acl_ndx = Some(ndx);
    }

    /// Returns the extended attribute index if set (for --xattrs mode).
    #[inline]
    #[must_use]
    pub const fn xattr_ndx(&self) -> Option<u32> {
        self.xattr_ndx
    }

    /// Sets the extended attribute index (for --xattrs mode).
    pub const fn set_xattr_ndx(&mut self, ndx: u32) {
        self.xattr_ndx = Some(ndx);
    }

    /// Returns true if this entry is a block or character device.
    ///
    /// Checks for S_ISBLK (0o060000) or S_ISCHR (0o020000) mode bits.
    #[inline]
    #[must_use]
    pub const fn is_device(&self) -> bool {
        let type_bits = self.mode & 0o170000;
        type_bits == 0o060000 || type_bits == 0o020000 // S_IFBLK or S_IFCHR
    }

    /// Returns true if this entry is a block device.
    #[inline]
    #[must_use]
    pub const fn is_block_device(&self) -> bool {
        self.mode & 0o170000 == 0o060000 // S_IFBLK
    }

    /// Returns true if this entry is a character device.
    #[inline]
    #[must_use]
    pub const fn is_char_device(&self) -> bool {
        self.mode & 0o170000 == 0o020000 // S_IFCHR
    }

    /// Returns true if this entry is a special file (socket or FIFO).
    #[inline]
    #[must_use]
    pub const fn is_special(&self) -> bool {
        let type_bits = self.mode & 0o170000;
        type_bits == 0o140000 || type_bits == 0o010000 // S_IFSOCK or S_IFIFO
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_type_from_mode() {
        assert_eq!(FileType::from_mode(0o100644), Some(FileType::Regular));
        assert_eq!(FileType::from_mode(0o040755), Some(FileType::Directory));
        assert_eq!(FileType::from_mode(0o120777), Some(FileType::Symlink));
        assert_eq!(FileType::from_mode(0o060660), Some(FileType::BlockDevice));
        assert_eq!(FileType::from_mode(0o020666), Some(FileType::CharDevice));
        assert_eq!(FileType::from_mode(0o010644), Some(FileType::Fifo));
        assert_eq!(FileType::from_mode(0o140755), Some(FileType::Socket));
    }

    #[test]
    fn file_type_from_mode_invalid() {
        // Invalid mode (bits that don't match any file type)
        assert_eq!(FileType::from_mode(0o000644), None);
        assert_eq!(FileType::from_mode(0o050000), None);
        assert_eq!(FileType::from_mode(0o070000), None);
    }

    #[test]
    fn file_type_round_trip() {
        for ft in [
            FileType::Regular,
            FileType::Directory,
            FileType::Symlink,
            FileType::BlockDevice,
            FileType::CharDevice,
            FileType::Fifo,
            FileType::Socket,
        ] {
            let mode = ft.to_mode_bits() | 0o644;
            assert_eq!(FileType::from_mode(mode), Some(ft));
        }
    }

    #[test]
    fn file_type_predicates() {
        assert!(FileType::Regular.is_regular());
        assert!(!FileType::Directory.is_regular());

        assert!(FileType::Directory.is_dir());
        assert!(!FileType::Regular.is_dir());

        assert!(FileType::Symlink.is_symlink());
        assert!(!FileType::Regular.is_symlink());

        assert!(FileType::BlockDevice.is_device());
        assert!(FileType::CharDevice.is_device());
        assert!(!FileType::Regular.is_device());
        assert!(!FileType::Directory.is_device());
        assert!(!FileType::Fifo.is_device());
        assert!(!FileType::Socket.is_device());
    }

    #[test]
    fn file_type_clone_and_eq() {
        let ft = FileType::Regular;
        let cloned = ft;
        assert_eq!(ft, cloned);
    }

    #[test]
    fn file_type_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(FileType::Regular);
        set.insert(FileType::Directory);
        assert!(set.contains(&FileType::Regular));
        assert!(set.contains(&FileType::Directory));
        assert!(!set.contains(&FileType::Symlink));
    }

    #[test]
    fn file_type_debug() {
        let debug = format!("{:?}", FileType::Regular);
        assert_eq!(debug, "Regular");
    }

    #[test]
    fn new_file_entry() {
        let entry = FileEntry::new_file("test.txt".into(), 1024, 0o644);
        assert_eq!(entry.name(), "test.txt");
        assert_eq!(entry.size(), 1024);
        assert_eq!(entry.permissions(), 0o644);
        assert_eq!(entry.file_type(), FileType::Regular);
        assert!(entry.is_file());
        assert!(!entry.is_dir());
    }

    #[test]
    fn new_file_entry_permissions_masked() {
        // Permissions should be masked to 0o7777
        let entry = FileEntry::new_file("test.txt".into(), 100, 0o177777);
        assert_eq!(entry.permissions(), 0o7777);
    }

    #[test]
    fn new_directory_entry() {
        let entry = FileEntry::new_directory("subdir".into(), 0o755);
        assert_eq!(entry.name(), "subdir");
        assert_eq!(entry.size(), 0);
        assert_eq!(entry.permissions(), 0o755);
        assert_eq!(entry.file_type(), FileType::Directory);
        assert!(entry.is_dir());
        assert!(!entry.is_file());
    }

    #[test]
    fn new_symlink_entry() {
        let entry = FileEntry::new_symlink("link".into(), "target".into());
        assert_eq!(entry.name(), "link");
        assert!(entry.is_symlink());
        assert_eq!(
            entry.link_target().map(|p| p.as_path()),
            Some("target".as_ref())
        );
    }

    #[test]
    fn entry_mtime_setting() {
        let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        entry.set_mtime(1700000000, 123456789);
        assert_eq!(entry.mtime(), 1700000000);
        assert_eq!(entry.mtime_nsec(), 123456789);
    }

    #[test]
    fn entry_uid_gid_setting() {
        let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        assert_eq!(entry.uid(), None);
        assert_eq!(entry.gid(), None);

        entry.set_uid(1000);
        entry.set_gid(1001);

        assert_eq!(entry.uid(), Some(1000));
        assert_eq!(entry.gid(), Some(1001));
    }

    #[test]
    fn entry_link_target_setting() {
        let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        assert!(entry.link_target().is_none());

        entry.set_link_target("/some/target".into());
        assert_eq!(
            entry.link_target().map(|p| p.as_path()),
            Some("/some/target".as_ref())
        );
    }

    #[test]
    fn entry_rdev_setting() {
        let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        assert_eq!(entry.rdev_major(), None);
        assert_eq!(entry.rdev_minor(), None);

        entry.set_rdev(8, 1);

        assert_eq!(entry.rdev_major(), Some(8));
        assert_eq!(entry.rdev_minor(), Some(1));
    }

    #[test]
    fn entry_path_accessor() {
        let entry = FileEntry::new_file("some/nested/path.txt".into(), 100, 0o644);
        assert_eq!(entry.path(), &PathBuf::from("some/nested/path.txt"));
    }

    #[test]
    fn entry_mode_accessor() {
        let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        let mode = entry.mode();
        // Mode should include both type and permissions
        assert_eq!(mode & 0o7777, 0o644);
        assert_eq!(mode & 0o170000, 0o100000); // Regular file type
    }

    #[test]
    fn entry_clone_and_eq() {
        let entry = FileEntry::new_file("test.txt".into(), 1024, 0o644);
        let cloned = entry.clone();
        assert_eq!(entry, cloned);
    }

    #[test]
    fn entry_debug_format() {
        let entry = FileEntry::new_file("test.txt".into(), 1024, 0o644);
        let debug = format!("{entry:?}");
        assert!(debug.contains("FileEntry"));
        assert!(debug.contains("test.txt"));
    }

    #[test]
    fn entry_from_raw() {
        let flags = super::super::flags::FileFlags::default();
        let entry = FileEntry::from_raw(
            "raw_file.txt".into(),
            2048,
            0o100755,
            1700000000,
            999999,
            flags,
        );

        assert_eq!(entry.name(), "raw_file.txt");
        assert_eq!(entry.size(), 2048);
        assert_eq!(entry.mode(), 0o100755);
        assert_eq!(entry.mtime(), 1700000000);
        assert_eq!(entry.mtime_nsec(), 999999);
        assert!(entry.is_file());
    }

    #[test]
    fn entry_file_type_fallback() {
        // Create an entry with invalid mode via from_raw
        let flags = super::super::flags::FileFlags::default();
        let entry = FileEntry::from_raw(
            "unknown.txt".into(),
            100,
            0o000644, // Invalid mode type bits
            0,
            0,
            flags,
        );

        // Should fall back to Regular
        assert_eq!(entry.file_type(), FileType::Regular);
    }

    #[test]
    fn symlink_not_file() {
        let entry = FileEntry::new_symlink("link".into(), "target".into());
        assert!(!entry.is_file());
        assert!(!entry.is_dir());
        assert!(entry.is_symlink());
    }

    #[test]
    fn directory_size_is_zero() {
        let entry = FileEntry::new_directory("dir".into(), 0o755);
        assert_eq!(entry.size(), 0);
    }

    #[test]
    fn file_entry_flags_accessor() {
        let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        let _flags = entry.flags(); // Just ensure the accessor works
    }
}
