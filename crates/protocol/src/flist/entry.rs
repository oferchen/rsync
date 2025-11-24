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
            0o100000 => Some(Self::Regular),    // S_IFREG
            0o040000 => Some(Self::Directory),  // S_IFDIR
            0o120000 => Some(Self::Symlink),    // S_IFLNK
            0o060000 => Some(Self::BlockDevice), // S_IFBLK
            0o020000 => Some(Self::CharDevice), // S_IFCHR
            0o010000 => Some(Self::Fifo),       // S_IFIFO
            0o140000 => Some(Self::Socket),     // S_IFSOCK
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
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FileEntry {
    /// Relative path of the entry within the transfer.
    name: PathBuf,
    /// File size in bytes (0 for directories and special files).
    size: u64,
    /// Unix mode bits (type + permissions).
    mode: u32,
    /// Modification time as seconds since Unix epoch.
    mtime: i64,
    /// Modification time nanoseconds (protocol 31+).
    mtime_nsec: u32,
    /// User ID (None if not preserving ownership).
    uid: Option<u32>,
    /// Group ID (None if not preserving ownership).
    gid: Option<u32>,
    /// Device major number (for block/char devices).
    rdev_major: Option<u32>,
    /// Device minor number (for block/char devices).
    rdev_minor: Option<u32>,
    /// Symlink target path (for symlinks).
    link_target: Option<PathBuf>,
    /// Hardlink index (for hardlink preservation).
    hardlink_idx: Option<u32>,
    /// Entry flags from wire format.
    flags: super::flags::FileFlags,
}

impl FileEntry {
    /// Creates a new regular file entry.
    #[must_use]
    pub fn new_file(name: PathBuf, size: u64, permissions: u32) -> Self {
        Self {
            name,
            size,
            mode: FileType::Regular.to_mode_bits() | (permissions & 0o7777),
            mtime: 0,
            mtime_nsec: 0,
            uid: None,
            gid: None,
            rdev_major: None,
            rdev_minor: None,
            link_target: None,
            hardlink_idx: None,
            flags: super::flags::FileFlags::default(),
        }
    }

    /// Creates a new directory entry.
    #[must_use]
    pub fn new_directory(name: PathBuf, permissions: u32) -> Self {
        Self {
            name,
            size: 0,
            mode: FileType::Directory.to_mode_bits() | (permissions & 0o7777),
            mtime: 0,
            mtime_nsec: 0,
            uid: None,
            gid: None,
            rdev_major: None,
            rdev_minor: None,
            link_target: None,
            hardlink_idx: None,
            flags: super::flags::FileFlags::default(),
        }
    }

    /// Creates a new symlink entry.
    #[must_use]
    pub fn new_symlink(name: PathBuf, target: PathBuf) -> Self {
        Self {
            name,
            size: 0,
            mode: FileType::Symlink.to_mode_bits() | 0o777,
            mtime: 0,
            mtime_nsec: 0,
            uid: None,
            gid: None,
            rdev_major: None,
            rdev_minor: None,
            link_target: Some(target),
            hardlink_idx: None,
            flags: super::flags::FileFlags::default(),
        }
    }

    /// Creates a file entry from raw components (used during decoding).
    #[must_use]
    pub(crate) fn from_raw(
        name: PathBuf,
        size: u64,
        mode: u32,
        mtime: i64,
        mtime_nsec: u32,
        flags: super::flags::FileFlags,
    ) -> Self {
        Self {
            name,
            size,
            mode,
            mtime,
            mtime_nsec,
            uid: None,
            gid: None,
            rdev_major: None,
            rdev_minor: None,
            link_target: None,
            hardlink_idx: None,
            flags,
        }
    }

    /// Returns the relative path name of the entry.
    #[must_use]
    pub fn name(&self) -> &str {
        self.name.to_str().unwrap_or("")
    }

    /// Returns the relative path of the entry.
    #[must_use]
    pub fn path(&self) -> &PathBuf {
        &self.name
    }

    /// Returns the file size in bytes.
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// Returns the Unix mode bits (type + permissions).
    #[must_use]
    pub const fn mode(&self) -> u32 {
        self.mode
    }

    /// Returns the permission bits only (without type).
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
    #[must_use]
    pub const fn mtime(&self) -> i64 {
        self.mtime
    }

    /// Returns the modification time nanoseconds.
    #[must_use]
    pub const fn mtime_nsec(&self) -> u32 {
        self.mtime_nsec
    }

    /// Returns the user ID if set.
    #[must_use]
    pub const fn uid(&self) -> Option<u32> {
        self.uid
    }

    /// Returns the group ID if set.
    #[must_use]
    pub const fn gid(&self) -> Option<u32> {
        self.gid
    }

    /// Returns the symlink target if this is a symlink.
    #[must_use]
    pub fn link_target(&self) -> Option<&PathBuf> {
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
    #[must_use]
    pub fn is_dir(&self) -> bool {
        self.file_type().is_dir()
    }

    /// Returns true if this entry is a regular file.
    #[must_use]
    pub fn is_file(&self) -> bool {
        self.file_type().is_regular()
    }

    /// Returns true if this entry is a symbolic link.
    #[must_use]
    pub fn is_symlink(&self) -> bool {
        self.file_type().is_symlink()
    }

    /// Sets the modification time.
    pub fn set_mtime(&mut self, secs: i64, nsec: u32) {
        self.mtime = secs;
        self.mtime_nsec = nsec;
    }

    /// Sets the user ID.
    pub fn set_uid(&mut self, uid: u32) {
        self.uid = Some(uid);
    }

    /// Sets the group ID.
    pub fn set_gid(&mut self, gid: u32) {
        self.gid = Some(gid);
    }

    /// Sets the symlink target.
    pub fn set_link_target(&mut self, target: PathBuf) {
        self.link_target = Some(target);
    }

    /// Sets the device numbers.
    pub fn set_rdev(&mut self, major: u32, minor: u32) {
        self.rdev_major = Some(major);
        self.rdev_minor = Some(minor);
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
        assert_eq!(entry.link_target().map(|p| p.as_path()), Some("target".as_ref()));
    }

    #[test]
    fn entry_mtime_setting() {
        let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        entry.set_mtime(1700000000, 123456789);
        assert_eq!(entry.mtime(), 1700000000);
        assert_eq!(entry.mtime_nsec(), 123456789);
    }
}
