use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::super::wire_path::path_bytes_to_wire;
use super::FileEntry;
use super::core::extract_dirname;
use super::extras::FileEntryExtras;
use super::file_type::FileType;

impl FileEntry {
    /// Returns a mutable reference to the extras, allocating if needed.
    #[inline]
    pub(super) fn extras_mut(&mut self) -> &mut FileEntryExtras {
        self.extras
            .get_or_insert_with(|| Box::new(FileEntryExtras::default()))
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

    /// Returns the interned parent directory path.
    ///
    /// When the entry was built through [`super::super::read::FileListReader`]
    /// with interning enabled, this `Arc<Path>` is shared with other entries in
    /// the same directory, avoiding redundant heap allocations.
    ///
    /// For root-level entries (no directory separator in the name), returns
    /// an `Arc` pointing to the empty path `""`.
    #[inline]
    #[must_use]
    pub fn dirname(&self) -> &Arc<Path> {
        &self.dirname
    }

    /// Prepends a parent directory prefix to this entry's path.
    ///
    /// Used when receiving incremental file list segments (INC_RECURSE) where
    /// entries arrive with paths relative to their parent directory. This
    /// reconstructs the full relative path by joining the parent prefix.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:recv_file_list()` - entries in sub-lists use basename-only paths
    /// - `flist.c:f_name()` - reconstructs full path by prepending `dirname`
    pub fn prepend_dir(&mut self, parent: &Path) {
        self.name = parent.join(&self.name);
        self.dirname = extract_dirname(&self.name);
    }

    /// Replaces the dirname with an interned `Arc<Path>`.
    ///
    /// Called by [`super::super::read::FileListReader`] after constructing the
    /// entry to replace the per-entry dirname allocation with a shared reference
    /// from [`super::super::intern::PathInterner`].
    #[inline]
    pub fn set_dirname(&mut self, dirname: Arc<Path>) {
        self.dirname = dirname;
    }

    /// Strips leading `/` characters from the entry path.
    ///
    /// With `--relative`, upstream sends paths with a leading slash (e.g.
    /// `/src/lib/foo.rs`). After sorting, the receiver strips these so that
    /// `dest_dir.join(path)` produces the correct destination.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:3071-3084`: `if (strip_root)` block in `flist_sort_and_clean()`
    pub fn strip_leading_slashes(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let bytes = self.name.as_os_str().as_bytes();
            let trimmed = bytes.iter().position(|&b| b != b'/').unwrap_or(bytes.len());
            if trimmed > 0 {
                let new_bytes = &bytes[trimmed..];
                if new_bytes.is_empty() {
                    self.name = PathBuf::from(".");
                } else {
                    use std::ffi::OsStr;
                    self.name = PathBuf::from(OsStr::from_bytes(new_bytes));
                }
                self.dirname = extract_dirname(&self.name);
            }
        }
        #[cfg(not(unix))]
        {
            let s = self.name.to_string_lossy();
            // Trim both POSIX `/` and native `\` so behaviour matches the
            // Unix branch when paths arrive in either form (e.g.
            // `\\share\foo` from a Windows operand). Wire-form paths
            // always use `/`, but Windows operands may carry `\`.
            let trimmed = s.trim_start_matches(|c| c == '/' || c == '\\');
            if trimmed.len() != s.len() {
                self.name = if trimmed.is_empty() {
                    PathBuf::from(".")
                } else {
                    PathBuf::from(trimmed)
                };
                self.dirname = extract_dirname(&self.name);
            }
        }
    }

    /// Returns the path as wire-format bytes (rsync filename encoding).
    ///
    /// Used for sorting, comparison, and wire encoding. On Unix, paths are
    /// already `/`-separated and the underlying `OsStr` bytes are returned
    /// without copying (`Cow::Borrowed`). On Windows, native `\` separators
    /// are translated to `/` to match the rsync wire-format invariant
    /// (upstream `flist.c:send_file_entry()` writes filename bytes
    /// verbatim, and upstream's only Windows port runs under Cygwin which
    /// presents `/`-separated paths). When the input already contains no
    /// `\` byte, the borrow path is taken without allocation.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:534-570` `send_file_entry()` filename emission.
    /// - `util1.c:955-961` Cygwin POSIX boundary - the only `\` handling
    ///   in upstream lives there, which oc-rsync does not run under.
    #[inline]
    #[must_use]
    pub fn name_bytes(&self) -> Cow<'_, [u8]> {
        path_bytes_to_wire(&self.name)
    }

    /// Returns the file size in bytes.
    #[inline]
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// Sets the file size in bytes.
    #[inline]
    pub fn set_size(&mut self, size: u64) {
        self.size = size;
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
    pub const fn uid(&self) -> Option<u32> {
        self.uid
    }

    /// Returns the group ID if set.
    #[inline]
    pub const fn gid(&self) -> Option<u32> {
        self.gid
    }

    /// Returns the symlink target if this is a symlink.
    pub fn link_target(&self) -> Option<&PathBuf> {
        self.extras.as_ref().and_then(|e| e.link_target.as_ref())
    }

    /// Returns the device major number if this is a device.
    pub fn rdev_major(&self) -> Option<u32> {
        self.extras.as_ref().and_then(|e| e.rdev_major)
    }

    /// Returns the device minor number if this is a device.
    pub fn rdev_minor(&self) -> Option<u32> {
        self.extras.as_ref().and_then(|e| e.rdev_minor)
    }

    /// Returns the wire format flags.
    #[must_use]
    pub const fn flags(&self) -> super::super::flags::FileFlags {
        self.flags
    }

    /// Sets the wire format flags.
    ///
    /// Used by the generator to mark top-level directories with `XMIT_TOP_DIR`.
    pub fn set_flags(&mut self, flags: super::super::flags::FileFlags) {
        self.flags = flags;
    }

    /// Returns a mutable reference to the wire format flags.
    ///
    /// Used by `match_hard_links()` to reassign leader/follower status in-place
    /// after sorting without copying the entire flags struct.
    pub fn flags_mut(&mut self) -> &mut super::super::flags::FileFlags {
        &mut self.flags
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
    pub fn user_name(&self) -> Option<&str> {
        self.extras.as_ref().and_then(|e| e.user_name.as_deref())
    }

    /// Sets the user name for cross-system ownership mapping.
    pub fn set_user_name(&mut self, name: String) {
        self.extras_mut().user_name = Some(name);
    }

    /// Returns the group name if set.
    pub fn group_name(&self) -> Option<&str> {
        self.extras.as_ref().and_then(|e| e.group_name.as_deref())
    }

    /// Sets the group name for cross-system ownership mapping.
    pub fn set_group_name(&mut self, name: String) {
        self.extras_mut().group_name = Some(name);
    }

    /// Sets the symlink target.
    pub fn set_link_target(&mut self, target: PathBuf) {
        self.extras_mut().link_target = Some(target);
    }

    /// Sets the device numbers.
    pub fn set_rdev(&mut self, major: u32, minor: u32) {
        let e = self.extras_mut();
        e.rdev_major = Some(major);
        e.rdev_minor = Some(minor);
    }

    /// Returns the hardlink index if this entry is a hardlink.
    pub fn hardlink_idx(&self) -> Option<u32> {
        self.extras.as_ref().and_then(|e| e.hardlink_idx)
    }

    /// Sets the hardlink index for this entry.
    pub fn set_hardlink_idx(&mut self, idx: u32) {
        self.extras_mut().hardlink_idx = Some(idx);
    }

    /// Returns the access time as seconds since Unix epoch.
    #[inline]
    #[must_use]
    pub fn atime(&self) -> i64 {
        self.extras.as_ref().map_or(0, |e| e.atime)
    }

    /// Sets the access time (seconds only, nanoseconds unchanged).
    pub fn set_atime(&mut self, secs: i64) {
        self.extras_mut().atime = secs;
    }

    /// Returns the access time nanoseconds.
    #[inline]
    #[must_use]
    pub fn atime_nsec(&self) -> u32 {
        self.extras.as_ref().map_or(0, |e| e.atime_nsec)
    }

    /// Sets the access time nanoseconds.
    pub fn set_atime_nsec(&mut self, nsec: u32) {
        self.extras_mut().atime_nsec = nsec;
    }

    /// Returns the creation time as seconds since Unix epoch.
    #[inline]
    #[must_use]
    pub fn crtime(&self) -> i64 {
        self.extras.as_ref().map_or(0, |e| e.crtime)
    }

    /// Sets the creation time.
    pub fn set_crtime(&mut self, secs: i64) {
        self.extras_mut().crtime = secs;
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
    pub fn hardlink_dev(&self) -> Option<i64> {
        self.extras.as_ref().and_then(|e| e.hardlink_dev)
    }

    /// Sets the hardlink device number (for protocol < 30).
    pub fn set_hardlink_dev(&mut self, dev: i64) {
        self.extras_mut().hardlink_dev = Some(dev);
    }

    /// Returns the hardlink inode number (for protocol < 30).
    #[inline]
    pub fn hardlink_ino(&self) -> Option<i64> {
        self.extras.as_ref().and_then(|e| e.hardlink_ino)
    }

    /// Sets the hardlink inode number (for protocol < 30).
    pub fn set_hardlink_ino(&mut self, ino: i64) {
        self.extras_mut().hardlink_ino = Some(ino);
    }

    /// Returns the file checksum if set (for --checksum mode).
    #[inline]
    pub fn checksum(&self) -> Option<&[u8]> {
        self.extras.as_ref().and_then(|e| e.checksum.as_deref())
    }

    /// Sets the file checksum (for --checksum mode).
    pub fn set_checksum(&mut self, sum: Vec<u8>) {
        self.extras_mut().checksum = Some(sum);
    }

    /// Returns the access ACL index if set (for --acls mode).
    #[inline]
    pub fn acl_ndx(&self) -> Option<u32> {
        self.extras.as_ref().and_then(|e| e.acl_ndx)
    }

    /// Sets the access ACL index (for --acls mode).
    pub fn set_acl_ndx(&mut self, ndx: u32) {
        self.extras_mut().acl_ndx = Some(ndx);
    }

    /// Returns the default ACL index if set (for directories in --acls mode).
    ///
    /// Corresponds to upstream's `F_DIR_DEFACL`. Only meaningful for directories.
    #[inline]
    pub fn def_acl_ndx(&self) -> Option<u32> {
        self.extras.as_ref().and_then(|e| e.def_acl_ndx)
    }

    /// Sets the default ACL index (for directories in --acls mode).
    ///
    /// Corresponds to upstream's `F_DIR_DEFACL`. Only meaningful for directories.
    pub fn set_def_acl_ndx(&mut self, ndx: u32) {
        self.extras_mut().def_acl_ndx = Some(ndx);
    }

    /// Returns the extended attribute index if set (for --xattrs mode).
    #[inline]
    pub fn xattr_ndx(&self) -> Option<u32> {
        self.extras.as_ref().and_then(|e| e.xattr_ndx)
    }

    /// Sets the extended attribute index (for --xattrs mode).
    pub fn set_xattr_ndx(&mut self, ndx: u32) {
        self.extras_mut().xattr_ndx = Some(ndx);
    }

    /// Returns the sender-side xattr list if set (for --xattrs mode).
    ///
    /// Populated by the generator from filesystem xattr data.
    /// Names are in wire format.
    #[inline]
    pub fn xattr_list(&self) -> Option<&crate::xattr::XattrList> {
        self.extras.as_ref().and_then(|e| e.xattr_list.as_ref())
    }

    /// Sets the sender-side xattr list (for --xattrs mode).
    ///
    /// The list should contain entries with wire-format names
    /// (translated via `local_to_wire()`).
    pub fn set_xattr_list(&mut self, list: crate::xattr::XattrList) {
        self.extras_mut().xattr_list = Some(list);
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
