//! File entry representation for the rsync file list.
//!
//! A file entry contains all metadata needed to synchronize a single filesystem
//! object (regular file, directory, symlink, device, etc.).
//!
//! # Path Interning
//!
//! Many file entries in a transfer share the same parent directory. The `dirname`
//! field stores an `Arc<Path>` that can be shared across entries via
//! [`super::intern::PathInterner`], reducing heap allocations for directory paths.
//! This mirrors upstream rsync's `file_struct.dirname` which points into a shared
//! string pool (upstream: flist.c:f_name()).

use std::path::{Path, PathBuf};
use std::sync::Arc;

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

/// Rarely-used metadata fields for file entries.
///
/// These fields are only populated when specific flags are active (e.g.
/// `--hard-links`, `--devices`, `--acls`, `--xattrs`, `--atimes`, `--crtimes`,
/// `--checksum`). Storing them behind `Option<Box<...>>` in `FileEntry` avoids
/// ~200 bytes of inline overhead per entry when they're unused - matching
/// upstream rsync's conditional field allocation in `file_struct`
/// (upstream: flist.c:make_file()).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FileEntryExtras {
    /// Symlink target path (for symlinks).
    link_target: Option<PathBuf>,
    /// User name for cross-system ownership mapping (protocol 30+).
    user_name: Option<String>,
    /// Group name for cross-system ownership mapping (protocol 30+).
    group_name: Option<String>,
    /// Access time as seconds since Unix epoch (protocol 30+, --atimes).
    atime: i64,
    /// Creation time as seconds since Unix epoch (protocol 30+, --crtimes).
    crtime: i64,
    /// Access time nanoseconds (protocol 32+, --atimes).
    atime_nsec: u32,
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
    /// Access ACL index for --acls mode (index into access ACL list, protocol 30+).
    acl_ndx: Option<u32>,
    /// Default ACL index for directories in --acls mode (index into default ACL list).
    ///
    /// Only meaningful for directories. Corresponds to upstream's `F_DIR_DEFACL`.
    def_acl_ndx: Option<u32>,
    /// Extended attribute index for --xattrs mode (index into xattr list).
    xattr_ndx: Option<u32>,
}

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
/// the common-case inline size from ~295 bytes to ~88 bytes - matching
/// upstream rsync's conditional field allocation pattern.
///
/// # Path Interning
///
/// The `dirname` field holds a reference-counted parent directory path that can
/// be shared across entries in the same directory. When entries are built through
/// [`super::read::FileListReader`], the reader's [`super::intern::PathInterner`]
/// ensures that entries sharing a parent directory point to the same `Arc<Path>`
/// allocation. This mirrors upstream rsync's `file_struct.dirname` shared pointer
/// (upstream: flist.c).
///
/// Field order is optimized to minimize padding: 8-byte aligned fields first,
/// then 4-byte, then smaller fields.
pub struct FileEntry {
    // 8-byte aligned fields
    /// Relative path of the entry within the transfer.
    name: PathBuf,
    /// Interned parent directory path, shared across entries in the same directory.
    ///
    /// For a path like `"src/lib/foo.rs"`, dirname is `"src/lib"`. For root-level
    /// entries like `"foo.rs"`, dirname is the empty path `""`. When set by the
    /// `PathInterner`, multiple entries with the same parent share a single
    /// heap allocation via `Arc`.
    dirname: Arc<Path>,
    /// File size in bytes (0 for directories and special files).
    size: u64,
    /// Modification time as seconds since Unix epoch.
    mtime: i64,
    /// User ID (None if not preserving ownership).
    uid: Option<u32>,
    /// Group ID (None if not preserving ownership).
    gid: Option<u32>,
    /// Rarely-used fields, boxed to reduce inline size.
    ///
    /// `None` for regular files in typical transfers (no symlinks, devices,
    /// hardlinks, ACLs, xattrs, atimes, crtimes, or checksums).
    extras: Option<Box<FileEntryExtras>>,

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

/// Extracts the parent directory from a path.
///
/// Returns the parent component as `Arc<Path>`. For paths without a directory
/// separator (root-level entries), returns an `Arc` pointing to the empty path.
fn extract_dirname(path: &Path) -> Arc<Path> {
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
            uid: self.uid,
            gid: self.gid,
            extras: self.extras.clone(),
            mode: self.mode,
            mtime_nsec: self.mtime_nsec,
            flags: self.flags,
            content_dir: self.content_dir,
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
            .field("uid", &self.uid)
            .field("gid", &self.gid)
            .field("mode", &self.mode)
            .field("mtime_nsec", &self.mtime_nsec)
            .field("flags", &self.flags)
            .field("content_dir", &self.content_dir);
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
            && self.uid == other.uid
            && self.gid == other.gid
            && self.mode == other.mode
            && self.mtime_nsec == other.mtime_nsec
            && self.flags == other.flags
            && self.content_dir == other.content_dir
            && self.extras == other.extras
    }
}

impl Eq for FileEntry {}

impl FileEntry {
    /// Returns a mutable reference to the extras, allocating if needed.
    #[inline]
    fn extras_mut(&mut self) -> &mut FileEntryExtras {
        self.extras
            .get_or_insert_with(|| Box::new(FileEntryExtras::default()))
    }

    /// Core constructor with all parameters - Template Method pattern.
    ///
    /// All public constructors delegate to this method to ensure consistent
    /// initialization and reduce code duplication. The dirname is extracted
    /// from the path automatically.
    #[inline]
    fn new_with_type(
        name: PathBuf,
        size: u64,
        file_type: FileType,
        permissions: u32,
        link_target: Option<PathBuf>,
    ) -> Self {
        let dirname = extract_dirname(&name);
        let extras = link_target.map(|lt| {
            Box::new(FileEntryExtras {
                link_target: Some(lt),
                ..FileEntryExtras::default()
            })
        });
        Self {
            name,
            dirname,
            size,
            mtime: 0,
            uid: None,
            gid: None,
            extras,
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
    ///
    /// This constructor is used only in tests. Production code should use
    /// `from_raw_bytes` which avoids UTF-8 validation overhead.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_raw(
        name: PathBuf,
        size: u64,
        mode: u32,
        mtime: i64,
        mtime_nsec: u32,
        flags: super::flags::FileFlags,
    ) -> Self {
        let dirname = extract_dirname(&name);
        Self {
            name,
            dirname,
            size,
            mtime,
            uid: None,
            gid: None,
            extras: None,
            mode,
            mtime_nsec,
            flags,
            content_dir: true,
        }
    }

    /// Creates a file entry from raw bytes (wire format, optimized).
    ///
    /// This avoids UTF-8 validation overhead during protocol decoding
    /// by converting bytes directly to PathBuf on Unix (zero-copy).
    /// UTF-8 validation is deferred until display via `name()`.
    ///
    /// The dirname is extracted from the path automatically. For interned
    /// dirname sharing, use [`Self::set_dirname`] after construction with a
    /// value from [`super::intern::PathInterner`].
    ///
    /// This is the preferred constructor for wire protocol decoding.
    #[must_use]
    pub fn from_raw_bytes(
        name: Vec<u8>,
        size: u64,
        mode: u32,
        mtime: i64,
        mtime_nsec: u32,
        flags: super::flags::FileFlags,
    ) -> Self {
        // Convert bytes to PathBuf without UTF-8 validation
        #[cfg(unix)]
        let path = {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt;
            PathBuf::from(OsStr::from_bytes(&name))
        };
        #[cfg(not(unix))]
        let path = {
            // On non-Unix, we need UTF-8 validation
            PathBuf::from(String::from_utf8_lossy(&name).into_owned())
        };

        let dirname = extract_dirname(&path);
        Self {
            name: path,
            dirname,
            size,
            mtime,
            uid: None,
            gid: None,
            extras: None,
            mode,
            mtime_nsec,
            flags,
            content_dir: true,
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

    /// Returns the interned parent directory path.
    ///
    /// When the entry was built through [`super::read::FileListReader`] with
    /// interning enabled, this `Arc<Path>` is shared with other entries in the
    /// same directory, avoiding redundant heap allocations.
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
    /// - `flist.c:recv_file_list()` — entries in sub-lists use basename-only paths
    /// - `flist.c:f_name()` — reconstructs full path by prepending `dirname`
    pub fn prepend_dir(&mut self, parent: &Path) {
        self.name = parent.join(&self.name);
        self.dirname = extract_dirname(&self.name);
    }

    /// Replaces the dirname with an interned `Arc<Path>`.
    ///
    /// Called by [`super::read::FileListReader`] after constructing the entry
    /// to replace the per-entry dirname allocation with a shared reference
    /// from [`super::intern::PathInterner`].
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
            let trimmed = s.trim_start_matches('/');
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

    /// Returns the path as raw bytes without UTF-8 validation.
    ///
    /// This is an optimized accessor for protocol operations that work
    /// with byte sequences. Use this for sorting, comparison, and wire
    /// encoding to avoid repeated UTF-8 validation via `name().as_bytes()`.
    ///
    /// On Unix, paths are inherently byte sequences (OsStr), so this
    /// provides zero-copy access. On other platforms, it performs UTF-8
    /// encoding once rather than validating twice (name() then as_bytes()).
    #[inline]
    #[must_use]
    pub fn name_bytes(&self) -> &[u8] {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            self.name.as_os_str().as_bytes()
        }
        #[cfg(not(unix))]
        self.name().as_bytes()
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
    pub const fn flags(&self) -> super::flags::FileFlags {
        self.flags
    }

    /// Sets the wire format flags.
    ///
    /// Used by the generator to mark top-level directories with `XMIT_TOP_DIR`.
    pub fn set_flags(&mut self, flags: super::flags::FileFlags) {
        self.flags = flags;
    }

    /// Returns a mutable reference to the wire format flags.
    ///
    /// Used by `match_hard_links()` to reassign leader/follower status in-place
    /// after sorting without copying the entire flags struct.
    pub fn flags_mut(&mut self) -> &mut super::flags::FileFlags {
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

    #[test]
    fn dirname_root_level_entry() {
        let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        assert_eq!(&**entry.dirname(), Path::new(""));
    }

    #[test]
    fn dirname_nested_entry() {
        let entry = FileEntry::new_file("src/lib/foo.rs".into(), 100, 0o644);
        assert_eq!(&**entry.dirname(), Path::new("src/lib"));
    }

    #[test]
    fn dirname_single_level() {
        let entry = FileEntry::new_file("dir/file.txt".into(), 100, 0o644);
        assert_eq!(&**entry.dirname(), Path::new("dir"));
    }

    #[test]
    fn set_dirname_replaces_existing() {
        let mut entry = FileEntry::new_file("dir/file.txt".into(), 100, 0o644);
        let shared = Arc::from(Path::new("other_dir"));
        entry.set_dirname(Arc::clone(&shared));
        assert!(Arc::ptr_eq(entry.dirname(), &shared));
    }

    #[test]
    fn dirname_shared_across_entries() {
        use crate::flist::intern::PathInterner;

        let mut interner = PathInterner::new();
        let mut entry1 = FileEntry::new_file("dir/a.txt".into(), 100, 0o644);
        let mut entry2 = FileEntry::new_file("dir/b.txt".into(), 200, 0o644);

        let dir = interner.intern(Path::new("dir"));
        entry1.set_dirname(Arc::clone(&dir));
        entry2.set_dirname(Arc::clone(&dir));

        assert!(Arc::ptr_eq(entry1.dirname(), entry2.dirname()));
    }

    /// Verifies the struct size optimization: FileEntry should be <= 96 bytes
    /// inline (down from ~295 bytes before the Box<FileEntryExtras> refactor).
    /// This guards against accidental field additions that bloat the hot path.
    #[test]
    fn file_entry_size_optimized() {
        let size = std::mem::size_of::<FileEntry>();
        // Target: ~88 bytes. Allow up to 96 for alignment padding.
        assert!(
            size <= 96,
            "FileEntry is {size} bytes; expected <= 96. \
             Did you add a field to FileEntry instead of FileEntryExtras?"
        );
    }

    /// Regular file entries should not allocate extras.
    #[test]
    fn regular_file_no_extras() {
        let entry = FileEntry::new_file("test.txt".into(), 1024, 0o644);
        assert!(
            entry.extras.is_none(),
            "Regular files should not allocate extras"
        );
    }

    /// Symlink entries should allocate extras for the link target.
    #[test]
    fn symlink_has_extras() {
        let entry = FileEntry::new_symlink("link".into(), "target".into());
        assert!(entry.extras.is_some());
        assert_eq!(
            entry.link_target().map(|p| p.as_path()),
            Some("target".as_ref())
        );
    }

    /// Extras should be lazily allocated on first setter call.
    #[test]
    fn extras_lazy_allocation() {
        let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        assert!(entry.extras.is_none());

        entry.set_atime(1700000000);
        assert!(entry.extras.is_some());
        assert_eq!(entry.atime(), 1700000000);
    }

    /// Default values for extras getters when extras is None.
    #[test]
    fn extras_default_values() {
        let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        assert_eq!(entry.atime(), 0);
        assert_eq!(entry.atime_nsec(), 0);
        assert_eq!(entry.crtime(), 0);
        assert_eq!(entry.link_target(), None);
        assert_eq!(entry.user_name(), None);
        assert_eq!(entry.group_name(), None);
        assert_eq!(entry.rdev_major(), None);
        assert_eq!(entry.rdev_minor(), None);
        assert_eq!(entry.hardlink_idx(), None);
        assert_eq!(entry.hardlink_dev(), None);
        assert_eq!(entry.hardlink_ino(), None);
        assert_eq!(entry.checksum(), None);
        assert_eq!(entry.acl_ndx(), None);
        assert_eq!(entry.def_acl_ndx(), None);
        assert_eq!(entry.xattr_ndx(), None);
    }

    /// Clone preserves extras correctly.
    #[test]
    fn clone_with_extras() {
        let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        entry.set_atime(1700000000);
        entry.set_checksum(vec![1, 2, 3]);
        entry.set_hardlink_idx(42);

        let cloned = entry.clone();
        assert_eq!(cloned.atime(), 1700000000);
        assert_eq!(cloned.checksum(), Some(&[1, 2, 3][..]));
        assert_eq!(cloned.hardlink_idx(), Some(42));
        assert_eq!(entry, cloned);
    }

    /// PartialEq handles None vs Some extras correctly.
    #[test]
    fn equality_with_different_extras() {
        let entry1 = FileEntry::new_file("test.txt".into(), 100, 0o644);
        let mut entry2 = FileEntry::new_file("test.txt".into(), 100, 0o644);
        assert_eq!(entry1, entry2);

        entry2.set_atime(1);
        assert_ne!(entry1, entry2);
    }

    // ---- FileEntryExtras accessor comprehensive tests ----

    #[test]
    fn extras_user_name_set_get() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert_eq!(entry.user_name(), None);

        entry.set_user_name("alice".to_string());
        assert_eq!(entry.user_name(), Some("alice"));
    }

    #[test]
    fn extras_group_name_set_get() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert_eq!(entry.group_name(), None);

        entry.set_group_name("staff".to_string());
        assert_eq!(entry.group_name(), Some("staff"));
    }

    #[test]
    fn extras_atime_set_get() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert_eq!(entry.atime(), 0);

        entry.set_atime(1_700_000_000);
        assert_eq!(entry.atime(), 1_700_000_000);
    }

    #[test]
    fn extras_atime_negative() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        entry.set_atime(-1);
        assert_eq!(entry.atime(), -1);
    }

    #[test]
    fn extras_atime_nsec_set_get() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert_eq!(entry.atime_nsec(), 0);

        entry.set_atime_nsec(999_999_999);
        assert_eq!(entry.atime_nsec(), 999_999_999);
    }

    #[test]
    fn extras_crtime_set_get() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert_eq!(entry.crtime(), 0);

        entry.set_crtime(1_600_000_000);
        assert_eq!(entry.crtime(), 1_600_000_000);
    }

    #[test]
    fn extras_crtime_negative() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        entry.set_crtime(-100);
        assert_eq!(entry.crtime(), -100);
    }

    #[test]
    fn extras_hardlink_idx_set_get() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert_eq!(entry.hardlink_idx(), None);

        entry.set_hardlink_idx(42);
        assert_eq!(entry.hardlink_idx(), Some(42));
    }

    #[test]
    fn extras_hardlink_idx_zero() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        entry.set_hardlink_idx(0);
        assert_eq!(entry.hardlink_idx(), Some(0));
    }

    #[test]
    fn extras_hardlink_dev_set_get() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert_eq!(entry.hardlink_dev(), None);

        entry.set_hardlink_dev(0xFD00);
        assert_eq!(entry.hardlink_dev(), Some(0xFD00));
    }

    #[test]
    fn extras_hardlink_dev_negative() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        entry.set_hardlink_dev(-1);
        assert_eq!(entry.hardlink_dev(), Some(-1));
    }

    #[test]
    fn extras_hardlink_ino_set_get() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert_eq!(entry.hardlink_ino(), None);

        entry.set_hardlink_ino(123_456);
        assert_eq!(entry.hardlink_ino(), Some(123_456));
    }

    #[test]
    fn extras_hardlink_ino_negative() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        entry.set_hardlink_ino(-999);
        assert_eq!(entry.hardlink_ino(), Some(-999));
    }

    #[test]
    fn extras_checksum_set_get() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert_eq!(entry.checksum(), None);

        let sum = vec![0xDE, 0xAD, 0xBE, 0xEF];
        entry.set_checksum(sum.clone());
        assert_eq!(entry.checksum(), Some(sum.as_slice()));
    }

    #[test]
    fn extras_checksum_empty() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        entry.set_checksum(vec![]);
        assert_eq!(entry.checksum(), Some(&[][..]));
    }

    #[test]
    fn extras_checksum_max_length() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        let sum = vec![0xFF; 32]; // max 32 bytes (e.g., SHA-256 / XXH128+MD5)
        entry.set_checksum(sum.clone());
        assert_eq!(entry.checksum(), Some(sum.as_slice()));
    }

    #[test]
    fn extras_acl_ndx_set_get() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert_eq!(entry.acl_ndx(), None);

        entry.set_acl_ndx(7);
        assert_eq!(entry.acl_ndx(), Some(7));
    }

    #[test]
    fn extras_acl_ndx_zero() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        entry.set_acl_ndx(0);
        assert_eq!(entry.acl_ndx(), Some(0));
    }

    #[test]
    fn extras_def_acl_ndx_set_get() {
        let mut entry = FileEntry::new_directory("d".into(), 0o755);
        assert_eq!(entry.def_acl_ndx(), None);

        entry.set_def_acl_ndx(3);
        assert_eq!(entry.def_acl_ndx(), Some(3));
    }

    #[test]
    fn extras_xattr_ndx_set_get() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert_eq!(entry.xattr_ndx(), None);

        entry.set_xattr_ndx(99);
        assert_eq!(entry.xattr_ndx(), Some(99));
    }

    #[test]
    fn extras_xattr_ndx_zero() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        entry.set_xattr_ndx(0);
        assert_eq!(entry.xattr_ndx(), Some(0));
    }

    #[test]
    fn extras_link_target_empty_path() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        entry.set_link_target(PathBuf::new());
        assert_eq!(
            entry.link_target().map(|p| p.as_path()),
            Some(Path::new(""))
        );
    }

    #[test]
    fn extras_user_name_empty_string() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        entry.set_user_name(String::new());
        assert_eq!(entry.user_name(), Some(""));
    }

    #[test]
    fn extras_group_name_empty_string() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        entry.set_group_name(String::new());
        assert_eq!(entry.group_name(), Some(""));
    }

    /// Setting multiple independent extras fields allocates once and stores all.
    #[test]
    fn extras_multiple_fields_independent() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert!(entry.extras.is_none());

        entry.set_atime(100);
        assert!(entry.extras.is_some());

        entry.set_crtime(200);
        entry.set_user_name("root".to_string());
        entry.set_group_name("wheel".to_string());
        entry.set_hardlink_idx(5);
        entry.set_acl_ndx(10);
        entry.set_xattr_ndx(20);
        entry.set_checksum(vec![0xAA]);
        entry.set_rdev(1, 2);
        entry.set_hardlink_dev(300);
        entry.set_hardlink_ino(400);
        entry.set_atime_nsec(500);
        entry.set_def_acl_ndx(15);
        entry.set_link_target("/target".into());

        // All fields should be independently readable.
        assert_eq!(entry.atime(), 100);
        assert_eq!(entry.crtime(), 200);
        assert_eq!(entry.user_name(), Some("root"));
        assert_eq!(entry.group_name(), Some("wheel"));
        assert_eq!(entry.hardlink_idx(), Some(5));
        assert_eq!(entry.acl_ndx(), Some(10));
        assert_eq!(entry.xattr_ndx(), Some(20));
        assert_eq!(entry.checksum(), Some(&[0xAA][..]));
        assert_eq!(entry.rdev_major(), Some(1));
        assert_eq!(entry.rdev_minor(), Some(2));
        assert_eq!(entry.hardlink_dev(), Some(300));
        assert_eq!(entry.hardlink_ino(), Some(400));
        assert_eq!(entry.atime_nsec(), 500);
        assert_eq!(entry.def_acl_ndx(), Some(15));
        assert_eq!(
            entry.link_target().map(|p| p.as_path()),
            Some(Path::new("/target"))
        );
    }

    /// Overwriting an extras field replaces the old value.
    #[test]
    fn extras_overwrite_value() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        entry.set_atime(1);
        assert_eq!(entry.atime(), 1);

        entry.set_atime(2);
        assert_eq!(entry.atime(), 2);
    }

    #[test]
    fn extras_overwrite_checksum() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        entry.set_checksum(vec![1, 2, 3]);
        entry.set_checksum(vec![4, 5]);
        assert_eq!(entry.checksum(), Some(&[4, 5][..]));
    }

    #[test]
    fn extras_overwrite_user_name() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        entry.set_user_name("alice".to_string());
        entry.set_user_name("bob".to_string());
        assert_eq!(entry.user_name(), Some("bob"));
    }

    #[test]
    fn extras_overwrite_link_target() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        entry.set_link_target("/old".into());
        entry.set_link_target("/new".into());
        assert_eq!(
            entry.link_target().map(|p| p.as_path()),
            Some(Path::new("/new"))
        );
    }

    /// Clone preserves all 15 extras fields.
    #[test]
    fn clone_preserves_all_extras() {
        let mut entry = FileEntry::new_file("f.txt".into(), 100, 0o644);
        entry.set_link_target("/tgt".into());
        entry.set_user_name("u".to_string());
        entry.set_group_name("g".to_string());
        entry.set_atime(10);
        entry.set_crtime(20);
        entry.set_atime_nsec(30);
        entry.set_rdev(40, 50);
        entry.set_hardlink_idx(60);
        entry.set_hardlink_dev(70);
        entry.set_hardlink_ino(80);
        entry.set_checksum(vec![0x90]);
        entry.set_acl_ndx(100);
        entry.set_def_acl_ndx(110);
        entry.set_xattr_ndx(120);

        let c = entry.clone();
        assert_eq!(
            c.link_target().map(|p| p.as_path()),
            Some(Path::new("/tgt"))
        );
        assert_eq!(c.user_name(), Some("u"));
        assert_eq!(c.group_name(), Some("g"));
        assert_eq!(c.atime(), 10);
        assert_eq!(c.crtime(), 20);
        assert_eq!(c.atime_nsec(), 30);
        assert_eq!(c.rdev_major(), Some(40));
        assert_eq!(c.rdev_minor(), Some(50));
        assert_eq!(c.hardlink_idx(), Some(60));
        assert_eq!(c.hardlink_dev(), Some(70));
        assert_eq!(c.hardlink_ino(), Some(80));
        assert_eq!(c.checksum(), Some(&[0x90][..]));
        assert_eq!(c.acl_ndx(), Some(100));
        assert_eq!(c.def_acl_ndx(), Some(110));
        assert_eq!(c.xattr_ndx(), Some(120));
        assert_eq!(entry, c);
    }

    /// Clone without extras produces None extras.
    #[test]
    fn clone_without_extras() {
        let entry = FileEntry::new_file("f.txt".into(), 100, 0o644);
        let c = entry.clone();
        assert!(c.extras.is_none());
        assert_eq!(entry, c);
    }

    /// PartialEq: both have extras with same values.
    #[test]
    fn equality_both_extras_same() {
        let mut a = FileEntry::new_file("f.txt".into(), 0, 0o644);
        let mut b = FileEntry::new_file("f.txt".into(), 0, 0o644);
        a.set_atime(99);
        a.set_checksum(vec![1]);
        b.set_atime(99);
        b.set_checksum(vec![1]);
        assert_eq!(a, b);
    }

    /// PartialEq: both have extras with different values.
    #[test]
    fn equality_both_extras_different() {
        let mut a = FileEntry::new_file("f.txt".into(), 0, 0o644);
        let mut b = FileEntry::new_file("f.txt".into(), 0, 0o644);
        a.set_atime(1);
        b.set_atime(2);
        assert_ne!(a, b);
    }

    /// PartialEq: one has extras, other does not.
    #[test]
    fn equality_one_has_extras() {
        let a = FileEntry::new_file("f.txt".into(), 0, 0o644);
        let mut b = FileEntry::new_file("f.txt".into(), 0, 0o644);
        b.set_hardlink_idx(1);
        assert_ne!(a, b);
        assert_ne!(b, a);
    }

    /// PartialEq: extras with all-default values vs None extras.
    /// These are NOT equal because Option<Box<...>> Some(default) != None.
    #[test]
    fn equality_default_extras_vs_none() {
        let a = FileEntry::new_file("f.txt".into(), 0, 0o644);
        let mut b = FileEntry::new_file("f.txt".into(), 0, 0o644);
        // Force extras allocation with a zero/default value.
        b.set_atime(0);
        // a.extras is None, b.extras is Some(default) - not structurally equal.
        assert_ne!(a, b);
    }

    /// Device constructors allocate extras for rdev fields.
    #[test]
    fn block_device_constructor_extras() {
        let entry = FileEntry::new_block_device("dev/sda".into(), 0o660, 8, 0);
        assert!(entry.extras.is_some());
        assert_eq!(entry.rdev_major(), Some(8));
        assert_eq!(entry.rdev_minor(), Some(0));
        assert!(entry.is_block_device());
        assert!(entry.is_device());
        assert!(!entry.is_char_device());
    }

    #[test]
    fn char_device_constructor_extras() {
        let entry = FileEntry::new_char_device("dev/null".into(), 0o666, 1, 3);
        assert!(entry.extras.is_some());
        assert_eq!(entry.rdev_major(), Some(1));
        assert_eq!(entry.rdev_minor(), Some(3));
        assert!(entry.is_char_device());
        assert!(entry.is_device());
        assert!(!entry.is_block_device());
    }

    /// FIFO and socket constructors do not allocate extras.
    #[test]
    fn fifo_no_extras() {
        let entry = FileEntry::new_fifo("pipe".into(), 0o644);
        assert!(entry.extras.is_none());
        assert!(entry.is_special());
    }

    #[test]
    fn socket_no_extras() {
        let entry = FileEntry::new_socket("sock".into(), 0o755);
        assert!(entry.extras.is_none());
        assert!(entry.is_special());
    }

    /// Symlink constructor allocates extras; other extras fields default.
    #[test]
    fn symlink_extras_other_fields_default() {
        let entry = FileEntry::new_symlink("lnk".into(), "/dest".into());
        assert!(entry.extras.is_some());
        assert_eq!(
            entry.link_target().map(|p| p.as_path()),
            Some(Path::new("/dest"))
        );
        // All other extras fields remain at default.
        assert_eq!(entry.user_name(), None);
        assert_eq!(entry.group_name(), None);
        assert_eq!(entry.atime(), 0);
        assert_eq!(entry.atime_nsec(), 0);
        assert_eq!(entry.crtime(), 0);
        assert_eq!(entry.rdev_major(), None);
        assert_eq!(entry.rdev_minor(), None);
        assert_eq!(entry.hardlink_idx(), None);
        assert_eq!(entry.hardlink_dev(), None);
        assert_eq!(entry.hardlink_ino(), None);
        assert_eq!(entry.checksum(), None);
        assert_eq!(entry.acl_ndx(), None);
        assert_eq!(entry.def_acl_ndx(), None);
        assert_eq!(entry.xattr_ndx(), None);
    }

    /// u32::MAX boundary for Option<u32> extras fields.
    #[test]
    fn extras_u32_max_values() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        entry.set_hardlink_idx(u32::MAX);
        entry.set_acl_ndx(u32::MAX);
        entry.set_def_acl_ndx(u32::MAX);
        entry.set_xattr_ndx(u32::MAX);
        entry.set_rdev(u32::MAX, u32::MAX);
        entry.set_atime_nsec(u32::MAX);

        assert_eq!(entry.hardlink_idx(), Some(u32::MAX));
        assert_eq!(entry.acl_ndx(), Some(u32::MAX));
        assert_eq!(entry.def_acl_ndx(), Some(u32::MAX));
        assert_eq!(entry.xattr_ndx(), Some(u32::MAX));
        assert_eq!(entry.rdev_major(), Some(u32::MAX));
        assert_eq!(entry.rdev_minor(), Some(u32::MAX));
        assert_eq!(entry.atime_nsec(), u32::MAX);
    }

    /// i64::MIN / i64::MAX boundary for i64 extras fields.
    #[test]
    fn extras_i64_boundary_values() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        entry.set_atime(i64::MAX);
        entry.set_crtime(i64::MIN);
        entry.set_hardlink_dev(i64::MAX);
        entry.set_hardlink_ino(i64::MIN);

        assert_eq!(entry.atime(), i64::MAX);
        assert_eq!(entry.crtime(), i64::MIN);
        assert_eq!(entry.hardlink_dev(), Some(i64::MAX));
        assert_eq!(entry.hardlink_ino(), Some(i64::MIN));
    }

    /// Setting rdev sets both major and minor atomically.
    #[test]
    fn extras_rdev_atomic_set() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        entry.set_rdev(0, 0);
        assert_eq!(entry.rdev_major(), Some(0));
        assert_eq!(entry.rdev_minor(), Some(0));
    }

    /// Debug output includes extras when present.
    #[test]
    fn debug_includes_extras() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        let debug_no_extras = format!("{entry:?}");
        assert!(!debug_no_extras.contains("extras"));

        entry.set_atime(42);
        let debug_with_extras = format!("{entry:?}");
        assert!(debug_with_extras.contains("extras"));
    }

    /// content_dir default and setter (not in extras, but part of FileEntry).
    #[test]
    fn content_dir_default_and_set() {
        let mut entry = FileEntry::new_directory("d".into(), 0o755);
        assert!(entry.content_dir());

        entry.set_content_dir(false);
        assert!(!entry.content_dir());

        entry.set_content_dir(true);
        assert!(entry.content_dir());
    }

    /// flags_mut allows in-place mutation.
    #[test]
    fn flags_mut_accessor() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        let original = entry.flags();
        let _flags_mut = entry.flags_mut();
        // Verify we can obtain a mutable ref (compile-time check).
        assert_eq!(entry.flags(), original);
    }

    /// set_flags replaces flags.
    #[test]
    fn set_flags_replaces() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        let flags = super::super::flags::FileFlags::default();
        entry.set_flags(flags);
        assert_eq!(entry.flags(), flags);
    }

    // ---- FileEntryExtras accessor comprehensive tests (task #1036) ----

    /// Getter roundtrip: set each extras field individually from a fresh entry,
    /// verify the value, and confirm extras was allocated.
    #[test]
    fn extras_roundtrip_link_target() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert!(entry.extras.is_none());
        entry.set_link_target("/absolute/target".into());
        assert!(entry.extras.is_some());
        assert_eq!(
            entry.link_target().map(|p| p.as_path()),
            Some(Path::new("/absolute/target"))
        );
    }

    #[test]
    fn extras_roundtrip_rdev() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert!(entry.extras.is_none());
        entry.set_rdev(259, 17);
        assert!(entry.extras.is_some());
        assert_eq!(entry.rdev_major(), Some(259));
        assert_eq!(entry.rdev_minor(), Some(17));
    }

    #[test]
    fn extras_roundtrip_user_name() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert!(entry.extras.is_none());
        entry.set_user_name("nobody".to_string());
        assert!(entry.extras.is_some());
        assert_eq!(entry.user_name(), Some("nobody"));
    }

    #[test]
    fn extras_roundtrip_group_name() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert!(entry.extras.is_none());
        entry.set_group_name("nogroup".to_string());
        assert!(entry.extras.is_some());
        assert_eq!(entry.group_name(), Some("nogroup"));
    }

    #[test]
    fn extras_roundtrip_crtime() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert!(entry.extras.is_none());
        entry.set_crtime(1_500_000_000);
        assert!(entry.extras.is_some());
        assert_eq!(entry.crtime(), 1_500_000_000);
    }

    #[test]
    fn extras_roundtrip_atime_nsec() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert!(entry.extras.is_none());
        entry.set_atime_nsec(123_456);
        assert!(entry.extras.is_some());
        assert_eq!(entry.atime_nsec(), 123_456);
    }

    #[test]
    fn extras_roundtrip_hardlink_dev() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert!(entry.extras.is_none());
        entry.set_hardlink_dev(0x1234_5678);
        assert!(entry.extras.is_some());
        assert_eq!(entry.hardlink_dev(), Some(0x1234_5678));
    }

    #[test]
    fn extras_roundtrip_hardlink_ino() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert!(entry.extras.is_none());
        entry.set_hardlink_ino(98765);
        assert!(entry.extras.is_some());
        assert_eq!(entry.hardlink_ino(), Some(98765));
    }

    #[test]
    fn extras_roundtrip_checksum() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert!(entry.extras.is_none());
        let sum = vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        entry.set_checksum(sum.clone());
        assert!(entry.extras.is_some());
        assert_eq!(entry.checksum(), Some(sum.as_slice()));
    }

    #[test]
    fn extras_roundtrip_acl_ndx() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert!(entry.extras.is_none());
        entry.set_acl_ndx(42);
        assert!(entry.extras.is_some());
        assert_eq!(entry.acl_ndx(), Some(42));
    }

    #[test]
    fn extras_roundtrip_def_acl_ndx() {
        let mut entry = FileEntry::new_directory("d".into(), 0o755);
        assert!(entry.extras.is_none());
        entry.set_def_acl_ndx(77);
        assert!(entry.extras.is_some());
        assert_eq!(entry.def_acl_ndx(), Some(77));
    }

    #[test]
    fn extras_roundtrip_xattr_ndx() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert!(entry.extras.is_none());
        entry.set_xattr_ndx(255);
        assert!(entry.extras.is_some());
        assert_eq!(entry.xattr_ndx(), Some(255));
    }

    #[test]
    fn extras_roundtrip_hardlink_idx() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert!(entry.extras.is_none());
        entry.set_hardlink_idx(1000);
        assert!(entry.extras.is_some());
        assert_eq!(entry.hardlink_idx(), Some(1000));
    }

    #[test]
    fn extras_roundtrip_atime() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        assert!(entry.extras.is_none());
        entry.set_atime(1_700_000_000);
        assert!(entry.extras.is_some());
        assert_eq!(entry.atime(), 1_700_000_000);
    }

    /// Clone of entry with extras produces independent copy - mutating clone
    /// does not affect the original.
    #[test]
    fn clone_extras_independence() {
        let mut entry = FileEntry::new_file("f.txt".into(), 100, 0o644);
        entry.set_atime(500);
        entry.set_user_name("alice".to_string());

        let mut cloned = entry.clone();
        cloned.set_atime(999);
        cloned.set_user_name("bob".to_string());

        // Original unchanged.
        assert_eq!(entry.atime(), 500);
        assert_eq!(entry.user_name(), Some("alice"));
        // Clone has new values.
        assert_eq!(cloned.atime(), 999);
        assert_eq!(cloned.user_name(), Some("bob"));
    }

    /// PartialEq: entries differ only by an extras field deep inside.
    #[test]
    fn equality_differs_by_single_extras_field() {
        let mut a = FileEntry::new_file("f.txt".into(), 100, 0o644);
        let mut b = FileEntry::new_file("f.txt".into(), 100, 0o644);
        a.set_xattr_ndx(1);
        b.set_xattr_ndx(2);
        assert_ne!(a, b);
    }

    /// PartialEq: entries with different extras fields set are not equal.
    #[test]
    fn equality_different_extras_fields_set() {
        let mut a = FileEntry::new_file("f.txt".into(), 100, 0o644);
        let mut b = FileEntry::new_file("f.txt".into(), 100, 0o644);
        a.set_acl_ndx(1);
        b.set_xattr_ndx(1);
        assert_ne!(a, b);
    }

    /// PartialEq: entries with identical multiple extras fields are equal.
    #[test]
    fn equality_multiple_extras_fields_match() {
        let mut a = FileEntry::new_file("f.txt".into(), 100, 0o644);
        let mut b = FileEntry::new_file("f.txt".into(), 100, 0o644);
        for entry in [&mut a, &mut b] {
            entry.set_atime(100);
            entry.set_crtime(200);
            entry.set_user_name("root".to_string());
            entry.set_checksum(vec![0xAB, 0xCD]);
            entry.set_hardlink_idx(42);
        }
        assert_eq!(a, b);
    }

    /// Setters on different entry types (directory, symlink) allocate extras.
    #[test]
    fn extras_on_directory_entry() {
        let mut entry = FileEntry::new_directory("mydir".into(), 0o755);
        assert!(entry.extras.is_none());
        entry.set_acl_ndx(5);
        entry.set_def_acl_ndx(6);
        entry.set_xattr_ndx(7);
        assert!(entry.extras.is_some());
        assert_eq!(entry.acl_ndx(), Some(5));
        assert_eq!(entry.def_acl_ndx(), Some(6));
        assert_eq!(entry.xattr_ndx(), Some(7));
    }

    #[test]
    fn extras_on_symlink_entry() {
        let mut entry = FileEntry::new_symlink("lnk".into(), "/target".into());
        assert!(entry.extras.is_some()); // Already allocated for link_target.
        entry.set_user_name("owner".to_string());
        assert_eq!(entry.user_name(), Some("owner"));
        // link_target preserved after setting another field.
        assert_eq!(
            entry.link_target().map(|p| p.as_path()),
            Some(Path::new("/target"))
        );
    }

    /// from_raw constructor starts with no extras.
    #[test]
    fn from_raw_no_extras() {
        let flags = super::super::flags::FileFlags::default();
        let entry = FileEntry::from_raw("file.rs".into(), 512, 0o100644, 1000, 0, flags);
        assert!(entry.extras.is_none());
        assert_eq!(entry.atime(), 0);
        assert_eq!(entry.link_target(), None);
        assert_eq!(entry.checksum(), None);
    }

    /// from_raw_bytes constructor starts with no extras.
    #[test]
    fn from_raw_bytes_no_extras() {
        let flags = super::super::flags::FileFlags::default();
        let entry =
            FileEntry::from_raw_bytes(b"data.bin".to_vec(), 2048, 0o100755, 5000, 100, flags);
        assert!(entry.extras.is_none());
        assert_eq!(entry.atime(), 0);
        assert_eq!(entry.crtime(), 0);
        assert_eq!(entry.user_name(), None);
    }

    /// from_raw_bytes entry can have extras set after construction.
    #[test]
    fn from_raw_bytes_then_set_extras() {
        let flags = super::super::flags::FileFlags::default();
        let mut entry = FileEntry::from_raw_bytes(b"file.dat".to_vec(), 100, 0o100644, 0, 0, flags);
        entry.set_checksum(vec![0xFF; 16]);
        entry.set_hardlink_idx(7);
        assert!(entry.extras.is_some());
        assert_eq!(entry.checksum(), Some(&[0xFF; 16][..]));
        assert_eq!(entry.hardlink_idx(), Some(7));
    }

    /// prepend_dir preserves extras on the entry.
    #[test]
    fn prepend_dir_preserves_extras() {
        let mut entry = FileEntry::new_file("file.txt".into(), 100, 0o644);
        entry.set_atime(42);
        entry.set_checksum(vec![0xBE, 0xEF]);

        entry.prepend_dir(Path::new("parent/dir"));

        assert_eq!(entry.name(), "parent/dir/file.txt");
        assert_eq!(entry.atime(), 42);
        assert_eq!(entry.checksum(), Some(&[0xBE, 0xEF][..]));
    }

    /// strip_leading_slashes preserves extras on the entry.
    #[cfg(unix)]
    #[test]
    fn strip_leading_slashes_preserves_extras() {
        let flags = super::super::flags::FileFlags::default();
        let mut entry = FileEntry::from_raw("/leading/file.txt".into(), 100, 0o100644, 0, 0, flags);
        entry.set_user_name("root".to_string());
        entry.set_xattr_ndx(3);

        entry.strip_leading_slashes();

        assert_eq!(entry.name(), "leading/file.txt");
        assert_eq!(entry.user_name(), Some("root"));
        assert_eq!(entry.xattr_ndx(), Some(3));
    }

    /// name_bytes returns correct byte representation.
    #[test]
    fn name_bytes_accessor() {
        let entry = FileEntry::new_file("hello.txt".into(), 100, 0o644);
        assert_eq!(entry.name_bytes(), b"hello.txt");
    }

    /// Extras with unicode user/group names.
    #[test]
    fn extras_unicode_names() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        entry.set_user_name("\u{00E9}mile".to_string()); // emile with accent
        entry.set_group_name("\u{00FC}sers".to_string()); // users with umlaut
        assert_eq!(entry.user_name(), Some("\u{00E9}mile"));
        assert_eq!(entry.group_name(), Some("\u{00FC}sers"));
    }

    /// Extras checksum with 16-byte MD5-sized value.
    #[test]
    fn extras_checksum_md5_sized() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        let md5 = vec![
            0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8,
            0x42, 0x7e,
        ];
        entry.set_checksum(md5.clone());
        assert_eq!(entry.checksum(), Some(md5.as_slice()));
        assert_eq!(entry.checksum().unwrap().len(), 16);
    }

    /// Multiple setters called, then clone, then verify independence.
    #[test]
    fn clone_all_extras_then_mutate() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        entry.set_link_target("/a".into());
        entry.set_user_name("u1".to_string());
        entry.set_group_name("g1".to_string());
        entry.set_atime(1);
        entry.set_crtime(2);
        entry.set_atime_nsec(3);
        entry.set_rdev(4, 5);
        entry.set_hardlink_idx(6);
        entry.set_hardlink_dev(7);
        entry.set_hardlink_ino(8);
        entry.set_checksum(vec![9]);
        entry.set_acl_ndx(10);
        entry.set_def_acl_ndx(11);
        entry.set_xattr_ndx(12);

        let mut cloned = entry.clone();

        // Mutate every field on the clone.
        cloned.set_link_target("/b".into());
        cloned.set_user_name("u2".to_string());
        cloned.set_group_name("g2".to_string());
        cloned.set_atime(100);
        cloned.set_crtime(200);
        cloned.set_atime_nsec(300);
        cloned.set_rdev(400, 500);
        cloned.set_hardlink_idx(600);
        cloned.set_hardlink_dev(700);
        cloned.set_hardlink_ino(800);
        cloned.set_checksum(vec![90]);
        cloned.set_acl_ndx(1000);
        cloned.set_def_acl_ndx(1100);
        cloned.set_xattr_ndx(1200);

        // Original untouched.
        assert_eq!(
            entry.link_target().map(|p| p.as_path()),
            Some(Path::new("/a"))
        );
        assert_eq!(entry.user_name(), Some("u1"));
        assert_eq!(entry.group_name(), Some("g1"));
        assert_eq!(entry.atime(), 1);
        assert_eq!(entry.crtime(), 2);
        assert_eq!(entry.atime_nsec(), 3);
        assert_eq!(entry.rdev_major(), Some(4));
        assert_eq!(entry.rdev_minor(), Some(5));
        assert_eq!(entry.hardlink_idx(), Some(6));
        assert_eq!(entry.hardlink_dev(), Some(7));
        assert_eq!(entry.hardlink_ino(), Some(8));
        assert_eq!(entry.checksum(), Some(&[9][..]));
        assert_eq!(entry.acl_ndx(), Some(10));
        assert_eq!(entry.def_acl_ndx(), Some(11));
        assert_eq!(entry.xattr_ndx(), Some(12));

        // Clone has new values.
        assert_eq!(
            cloned.link_target().map(|p| p.as_path()),
            Some(Path::new("/b"))
        );
        assert_eq!(cloned.user_name(), Some("u2"));
        assert_eq!(cloned.atime(), 100);
        assert_eq!(cloned.xattr_ndx(), Some(1200));

        assert_ne!(entry, cloned);
    }
}
