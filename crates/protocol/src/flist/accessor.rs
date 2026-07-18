//! Read-only trait abstracting the file-list entry read API.
//!
//! [`FileEntryAccessor`] captures the read-only API that file-list consumers
//! use over [`FileEntry`], letting code depend on `&dyn FileEntryAccessor`
//! (or generic `<T: FileEntryAccessor>`) instead of the concrete struct.

use std::borrow::Cow;

use super::entry::FileType;

/// Read-only accessor for file-list entry metadata.
///
/// Every method on this trait corresponds to a public read accessor on
/// `FileEntry`. Write/mutation methods are intentionally excluded - the
/// trait is for the consumer (read) side only.
///
/// # Upstream Reference
///
/// The field set mirrors upstream rsync's `struct file_struct` plus its
/// conditional `union file_extras` slots (upstream: `rsync.h:801-812`,
/// `rsync.h:786-792`).
pub trait FileEntryAccessor {
    // -- Path accessors --

    /// Returns the relative path name of the entry as a string slice.
    fn name(&self) -> &str;

    /// Returns the path as wire-format bytes (rsync filename encoding).
    ///
    /// On Unix, returns `Cow::Borrowed` from the underlying bytes. On
    /// Windows, backslash separators are translated to forward slash.
    fn name_bytes(&self) -> Cow<'_, [u8]>;

    /// Returns the parent directory path as a string slice.
    ///
    /// Returns `""` for root-level entries (no directory separator).
    fn dirname_str(&self) -> &str;

    // -- Scalar metadata --

    /// Returns the file size in bytes.
    fn size(&self) -> u64;

    /// Returns the Unix mode bits (type + permissions).
    fn mode(&self) -> u32;

    /// Returns the permission bits only (without file type).
    fn permissions(&self) -> u32 {
        self.mode() & 0o7777
    }

    /// Returns the modification time as seconds since the Unix epoch.
    fn mtime(&self) -> i64;

    /// Returns the modification time nanoseconds (protocol 31+).
    fn mtime_nsec(&self) -> u32;

    /// Returns the user ID if ownership is being preserved.
    fn uid(&self) -> Option<u32>;

    /// Returns the group ID if ownership is being preserved.
    fn gid(&self) -> Option<u32>;

    // -- Persisted wire flags --

    /// Returns true if this is a top-level directory in the transfer.
    fn top_dir(&self) -> bool;

    /// Returns true if this entry has hardlink information.
    fn hlinked(&self) -> bool;

    /// Returns true if this is the first (leader) entry in a hardlink group.
    fn hlink_first(&self) -> bool;

    // -- Type queries --

    /// Returns the file type classification.
    fn file_type(&self) -> FileType {
        FileType::from_mode(self.mode()).unwrap_or(FileType::Regular)
    }

    /// Returns true if this entry is a directory.
    fn is_dir(&self) -> bool {
        self.mode() & 0o170000 == 0o040000
    }

    /// Returns true if this entry is a regular file.
    fn is_file(&self) -> bool {
        self.mode() & 0o170000 == 0o100000
    }

    /// Returns true if this entry is a symbolic link.
    fn is_symlink(&self) -> bool {
        self.mode() & 0o170000 == 0o120000
    }

    /// Returns true if this entry is a block or character device.
    fn is_device(&self) -> bool {
        let type_bits = self.mode() & 0o170000;
        type_bits == 0o060000 || type_bits == 0o020000
    }

    /// Returns true if this entry is a block device.
    fn is_block_device(&self) -> bool {
        self.mode() & 0o170000 == 0o060000
    }

    /// Returns true if this entry is a character device.
    fn is_char_device(&self) -> bool {
        self.mode() & 0o170000 == 0o020000
    }

    /// Returns true if this entry is a special file (socket or FIFO).
    fn is_special(&self) -> bool {
        let type_bits = self.mode() & 0o170000;
        type_bits == 0o140000 || type_bits == 0o010000
    }

    // -- Directory content flag --

    /// Returns whether this directory has content to transfer.
    ///
    /// Only meaningful for directories. Returns true for non-directories.
    fn content_dir(&self) -> bool;

    // -- Extras fields (rarely-used metadata) --

    /// Returns the symlink target bytes if this is a symlink.
    fn link_target_bytes(&self) -> Option<&[u8]>;

    /// Returns the device major number if this is a device.
    fn rdev_major(&self) -> Option<u32>;

    /// Returns the device minor number if this is a device.
    fn rdev_minor(&self) -> Option<u32>;

    /// Returns the hardlink group index (protocol 30+).
    fn hardlink_idx(&self) -> Option<u32>;

    /// Returns the hardlink device number (protocol < 30).
    fn hardlink_dev(&self) -> Option<i64>;

    /// Returns the hardlink inode number (protocol < 30).
    fn hardlink_ino(&self) -> Option<i64>;

    /// Returns the file checksum bytes (--checksum mode).
    fn checksum(&self) -> Option<&[u8]>;

    /// Returns the access ACL index (--acls mode).
    fn acl_ndx(&self) -> Option<u32>;

    /// Returns the default ACL index for directories (--acls mode).
    fn def_acl_ndx(&self) -> Option<u32>;

    /// Returns the extended attribute index (--xattrs mode).
    fn xattr_ndx(&self) -> Option<u32>;

    /// Returns the user name for cross-system ownership mapping.
    fn user_name(&self) -> Option<&str>;

    /// Returns the group name for cross-system ownership mapping.
    fn group_name(&self) -> Option<&str>;

    /// Returns the access time as seconds since the Unix epoch.
    fn atime(&self) -> i64;

    /// Returns the access time nanoseconds.
    fn atime_nsec(&self) -> u32;

    /// Returns the creation time as seconds since the Unix epoch.
    fn crtime(&self) -> i64;
}

use super::entry::FileEntry;

impl FileEntryAccessor for FileEntry {
    fn name(&self) -> &str {
        self.name()
    }

    fn name_bytes(&self) -> Cow<'_, [u8]> {
        self.name_bytes()
    }

    fn dirname_str(&self) -> &str {
        self.dirname().to_str().unwrap_or("")
    }

    fn size(&self) -> u64 {
        self.size()
    }

    fn mode(&self) -> u32 {
        self.mode()
    }

    fn mtime(&self) -> i64 {
        self.mtime()
    }

    fn mtime_nsec(&self) -> u32 {
        self.mtime_nsec()
    }

    fn uid(&self) -> Option<u32> {
        self.uid()
    }

    fn gid(&self) -> Option<u32> {
        self.gid()
    }

    fn top_dir(&self) -> bool {
        self.top_dir()
    }

    fn hlinked(&self) -> bool {
        self.hlinked()
    }

    fn hlink_first(&self) -> bool {
        self.hlink_first()
    }

    fn content_dir(&self) -> bool {
        self.content_dir()
    }

    fn link_target_bytes(&self) -> Option<&[u8]> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            self.link_target().map(|p| p.as_os_str().as_bytes())
        }
        #[cfg(not(unix))]
        {
            // On non-Unix, lossy conversion through to_str is the only
            // byte-accessible path without OsStrExt.
            None
        }
    }

    fn rdev_major(&self) -> Option<u32> {
        self.rdev_major()
    }

    fn rdev_minor(&self) -> Option<u32> {
        self.rdev_minor()
    }

    fn hardlink_idx(&self) -> Option<u32> {
        self.hardlink_idx()
    }

    fn hardlink_dev(&self) -> Option<i64> {
        self.hardlink_dev()
    }

    fn hardlink_ino(&self) -> Option<i64> {
        self.hardlink_ino()
    }

    fn checksum(&self) -> Option<&[u8]> {
        self.checksum()
    }

    fn acl_ndx(&self) -> Option<u32> {
        self.acl_ndx()
    }

    fn def_acl_ndx(&self) -> Option<u32> {
        self.def_acl_ndx()
    }

    fn xattr_ndx(&self) -> Option<u32> {
        self.xattr_ndx()
    }

    fn user_name(&self) -> Option<&str> {
        self.user_name()
    }

    fn group_name(&self) -> Option<&str> {
        self.group_name()
    }

    fn atime(&self) -> i64 {
        self.atime()
    }

    fn atime_nsec(&self) -> u32 {
        self.atime_nsec()
    }

    fn crtime(&self) -> i64 {
        self.crtime()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that `FileEntry` implements `FileEntryAccessor` and all scalar
    /// methods return the expected values.
    #[test]
    fn file_entry_accessor_scalar_fields() {
        let mut entry = FileEntry::new_file("src/main.rs".into(), 4096, 0o755);
        entry.set_uid(1000);
        entry.set_gid(500);
        entry.set_mtime(1_700_000_000, 123_456);

        let acc: &dyn FileEntryAccessor = &entry;
        assert_eq!(acc.name(), "src/main.rs");
        assert_eq!(acc.dirname_str(), "src");
        assert_eq!(acc.size(), 4096);
        assert_eq!(acc.mode(), 0o100755);
        assert_eq!(acc.permissions(), 0o755);
        assert_eq!(acc.mtime(), 1_700_000_000);
        assert_eq!(acc.mtime_nsec(), 123_456);
        assert_eq!(acc.uid(), Some(1000));
        assert_eq!(acc.gid(), Some(500));
        assert!(acc.is_file());
        assert!(!acc.is_dir());
        assert!(!acc.is_symlink());
        assert_eq!(acc.file_type(), FileType::Regular);
    }

    /// Verifies type queries for directories.
    #[test]
    fn file_entry_accessor_directory() {
        let entry = FileEntry::new_directory("docs".into(), 0o755);
        let acc: &dyn FileEntryAccessor = &entry;
        assert!(acc.is_dir());
        assert!(!acc.is_file());
        assert!(acc.content_dir());
        assert_eq!(acc.file_type(), FileType::Directory);
    }

    /// Verifies symlink target round-trips through the trait.
    #[cfg(unix)]
    #[test]
    fn file_entry_accessor_symlink() {
        use std::path::PathBuf;
        let entry = FileEntry::new_symlink("link".into(), PathBuf::from("../target"));
        let acc: &dyn FileEntryAccessor = &entry;
        assert!(acc.is_symlink());
        assert_eq!(acc.link_target_bytes(), Some(b"../target" as &[u8]));
    }

    /// Verifies device number accessors.
    #[test]
    fn file_entry_accessor_device() {
        let entry = FileEntry::new_block_device("dev/sda".into(), 0o660, 8, 0);
        let acc: &dyn FileEntryAccessor = &entry;
        assert!(acc.is_device());
        assert!(acc.is_block_device());
        assert!(!acc.is_char_device());
        assert_eq!(acc.rdev_major(), Some(8));
        assert_eq!(acc.rdev_minor(), Some(0));
    }

    /// Verifies extras fields that start as absent.
    #[test]
    fn file_entry_accessor_absent_extras() {
        let entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        let acc: &dyn FileEntryAccessor = &entry;
        assert_eq!(acc.uid(), None);
        assert_eq!(acc.gid(), None);
        assert_eq!(acc.link_target_bytes(), None);
        assert_eq!(acc.rdev_major(), None);
        assert_eq!(acc.hardlink_idx(), None);
        assert_eq!(acc.checksum(), None);
        assert_eq!(acc.acl_ndx(), None);
        assert_eq!(acc.def_acl_ndx(), None);
        assert_eq!(acc.xattr_ndx(), None);
        assert_eq!(acc.user_name(), None);
        assert_eq!(acc.group_name(), None);
        assert_eq!(acc.atime(), 0);
        assert_eq!(acc.crtime(), 0);
    }

    /// Verifies checksum round-trips through the trait.
    #[test]
    fn file_entry_accessor_checksum() {
        let mut entry = FileEntry::new_file("data.bin".into(), 1024, 0o644);
        entry.set_checksum(vec![0xAB; 16]);
        let acc: &dyn FileEntryAccessor = &entry;
        assert_eq!(acc.checksum(), Some([0xAB; 16].as_slice()));
    }

    /// Verifies ACL and xattr indices.
    #[test]
    fn file_entry_accessor_acl_xattr() {
        let mut entry = FileEntry::new_file("f".into(), 0, 0o644);
        entry.set_acl_ndx(3);
        entry.set_def_acl_ndx(4);
        entry.set_xattr_ndx(5);
        let acc: &dyn FileEntryAccessor = &entry;
        assert_eq!(acc.acl_ndx(), Some(3));
        assert_eq!(acc.def_acl_ndx(), Some(4));
        assert_eq!(acc.xattr_ndx(), Some(5));
    }

    /// Verifies user/group name accessors.
    #[test]
    fn file_entry_accessor_user_group_names() {
        let mut entry = FileEntry::new_file("f".into(), 0, 0o644);
        entry.set_user_name("alice".to_string());
        entry.set_group_name("staff".to_string());
        let acc: &dyn FileEntryAccessor = &entry;
        assert_eq!(acc.user_name(), Some("alice"));
        assert_eq!(acc.group_name(), Some("staff"));
    }

    /// Verifies name_bytes returns wire-compatible output.
    #[test]
    fn file_entry_accessor_name_bytes() {
        let entry = FileEntry::new_file("src/lib.rs".into(), 0, 0o644);
        let acc: &dyn FileEntryAccessor = &entry;
        let bytes = acc.name_bytes();
        assert_eq!(&*bytes, b"src/lib.rs");
    }

    /// Verifies the content_dir flag toggling.
    #[test]
    fn file_entry_accessor_content_dir_toggle() {
        let mut entry = FileEntry::new_directory("dir".into(), 0o755);
        assert!(entry.content_dir());
        entry.set_content_dir(false);
        let acc: &dyn FileEntryAccessor = &entry;
        assert!(!acc.content_dir());
    }

    /// Verifies special file type detection.
    #[test]
    fn file_entry_accessor_special_types() {
        let fifo = FileEntry::new_fifo("pipe".into(), 0o644);
        let acc: &dyn FileEntryAccessor = &fifo;
        assert!(acc.is_special());
        assert!(!acc.is_device());

        let sock = FileEntry::new_socket("sock".into(), 0o644);
        let acc: &dyn FileEntryAccessor = &sock;
        assert!(acc.is_special());
    }

    /// Verifies atime/crtime round-trips.
    #[test]
    fn file_entry_accessor_atime_crtime() {
        let mut entry = FileEntry::new_file("f".into(), 0, 0o644);
        entry.set_atime(1_234_567);
        entry.set_atime_nsec(999);
        entry.set_crtime(9_876_543);
        let acc: &dyn FileEntryAccessor = &entry;
        assert_eq!(acc.atime(), 1_234_567);
        assert_eq!(acc.atime_nsec(), 999);
        assert_eq!(acc.crtime(), 9_876_543);
    }

    /// Verifies hardlink accessors.
    #[test]
    fn file_entry_accessor_hardlink() {
        let mut entry = FileEntry::new_file("f".into(), 0, 0o644);
        entry.set_hardlink_idx(42);
        entry.set_hardlink_dev(100);
        entry.set_hardlink_ino(200);
        let acc: &dyn FileEntryAccessor = &entry;
        assert_eq!(acc.hardlink_idx(), Some(42));
        assert_eq!(acc.hardlink_dev(), Some(100));
        assert_eq!(acc.hardlink_ino(), Some(200));
    }
}
