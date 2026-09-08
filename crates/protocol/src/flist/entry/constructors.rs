//! Constructors for [`FileEntry`].
//!
//! upstream: `flist.c:make_file()` - creates `file_struct` from stat data.

use std::path::PathBuf;

use super::FileEntry;
use super::core::{PRESENT_CONTENT_DIR, extract_dirname};
use super::extras::FileEntryExtras;
use super::file_type::FileType;

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
            extras,
            uid: 0,
            gid: 0,
            mode: (file_type.to_mode_bits() | (permissions & 0o7777)) as u16,
            mtime_nsec: 0,
            // Directories have content by default; uid/gid start absent.
            present: PRESENT_CONTENT_DIR,
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

    /// Creates a new symlink entry with `S_IFLNK` mode and the given permissions.
    ///
    /// A symlink's permission bits are *not* universally 0o777. Upstream
    /// `flist.c:1669` stores `file->mode = st.st_mode` verbatim for every file
    /// type, symlinks included, and whether that carries a meaningful value is
    /// a platform property: `rsync.h:455-456` defines `CAN_CHMOD_SYMLINK` when
    /// `HAVE_LCHMOD || HAVE_SETATTRLIST`, which holds on macOS and the BSDs,
    /// where `lchmod`/`setattrlist` give a link a real, settable mode. On Linux
    /// the kernel pins a link's `st_mode` permission bits to 0o777 and nothing
    /// can change them, so a Linux caller passing the stat mode through yields
    /// 0o777 anyway. Callers must therefore forward the stat mode rather than
    /// substitute a constant; only a synthesized entry with no underlying stat
    /// (a deletion sentinel, a decode fixture) should pass 0o777 literally.
    #[must_use]
    pub fn new_symlink(name: PathBuf, permissions: u32, target: PathBuf) -> Self {
        Self::new_with_type(name, 0, FileType::Symlink, permissions, Some(target))
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
        let mut entry = Self {
            name,
            dirname,
            size,
            mtime,
            extras: None,
            uid: 0,
            gid: 0,
            mode: mode as u16,
            mtime_nsec,
            present: PRESENT_CONTENT_DIR,
        };
        // Persist the semantically-significant wire flags into the
        // presence bitfield so tests round-trip correctly.
        entry.set_top_dir(flags.top_dir());
        entry.set_hlinked(flags.hlinked());
        entry.set_hlink_first(flags.hlink_first());
        entry
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
    /// The `flags` parameter carries wire-encoding flags. Only the three
    /// semantically persistent bits (`top_dir`, `hlinked`, `hlink_first`)
    /// are stored; the remaining delta-encoding flags are discarded.
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
        #[cfg(unix)]
        let path = {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt;
            PathBuf::from(OsStr::from_bytes(&name))
        };
        #[cfg(not(unix))]
        let path = {
            // Non-Unix targets cannot use `OsStr::from_bytes`; lossy UTF-8
            // conversion preserves displayability for non-UTF-8 names.
            PathBuf::from(String::from_utf8_lossy(&name).into_owned())
        };

        let dirname = extract_dirname(&path);
        let mut entry = Self {
            name: path,
            dirname,
            size,
            mtime,
            extras: None,
            uid: 0,
            gid: 0,
            mode: mode as u16,
            mtime_nsec,
            present: PRESENT_CONTENT_DIR,
        };
        // Persist the 3 semantically-significant wire flags into the
        // presence bitfield. The remaining XMIT flags are transient
        // delta-encoding state recomputed during send.
        entry.set_top_dir(flags.top_dir());
        entry.set_hlinked(flags.hlinked());
        entry.set_hlink_first(flags.hlink_first());
        entry
    }
}
