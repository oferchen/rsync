// upstream: flist.c:make_file() - creates file_struct from stat data

use std::path::PathBuf;

use super::core::extract_dirname;
use super::extras::FileEntryExtras;
use super::file_type::FileType;
use super::FileEntry;

impl FileEntry {
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
            flags: super::super::flags::FileFlags::default(),
            content_dir: true, // Directories have content by default
        }
    }

    /// Creates a new regular file entry with `S_IFREG` mode.
    #[must_use]
    pub fn new_file(name: PathBuf, size: u64, permissions: u32) -> Self {
        Self::new_with_type(name, size, FileType::Regular, permissions, None)
    }

    /// Creates a new directory entry with `S_IFDIR` mode and zero size.
    #[must_use]
    pub fn new_directory(name: PathBuf, permissions: u32) -> Self {
        Self::new_with_type(name, 0, FileType::Directory, permissions, None)
    }

    /// Creates a new symlink entry with `S_IFLNK` mode and 0o777 permissions.
    ///
    /// Symlinks always have 0o777 permissions per POSIX convention.
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
        flags: super::super::flags::FileFlags,
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
    /// value from [`super::super::intern::PathInterner`].
    ///
    /// This is the preferred constructor for wire protocol decoding.
    #[must_use]
    pub fn from_raw_bytes(
        name: Vec<u8>,
        size: u64,
        mode: u32,
        mtime: i64,
        mtime_nsec: u32,
        flags: super::super::flags::FileFlags,
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
}
