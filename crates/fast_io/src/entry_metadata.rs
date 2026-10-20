//! The metadata an anchored lookup answers with, whichever syscall answered.
//!
//! [`std::fs::Metadata`] has no public constructor, so a lookup that resolves
//! through `fstatat` cannot report its answer as one. That is the wall
//! [`AtMetadata`] was built for; [`EntryMetadata`] is the either/or it forces -
//! one variant per syscall, one accessor surface over both - shared by the
//! sender's source scan and the receiver's destination stats, and
//! [`EntryFileType`] is the same thing for the type bits.
//!
//! The accessors are named and widened after
//! [`std::os::unix::fs::MetadataExt`] on purpose: the caller is code that used
//! to hold a [`std::fs::Metadata`], and keeping the names identical is what
//! makes the cutover a change of type rather than a change of meaning.

use std::io;
#[cfg(unix)]
use std::time::Duration;
use std::time::SystemTime;

#[cfg(unix)]
use crate::dir_sandbox::AtMetadata;

/// Metadata for a source or destination entry, from whichever lookup reached it.
///
/// `Std` is the ordinary path-based [`std::fs::symlink_metadata`] /
/// [`std::fs::metadata`] answer, and on Linux it is also the anchored answer,
/// which arrives as a real [`std::fs::Metadata`] through `O_PATH` + `fstat`.
/// `At` is the anchored `fstatat` answer on the Unixes without `O_PATH`. See
/// [`crate::pinned_root`]'s Platform section for why the split falls there.
#[derive(Clone, Debug)]
pub enum EntryMetadata {
    /// A [`std::fs::Metadata`], from a path-based lookup or from `fstat` on an
    /// `O_PATH` descriptor.
    Std(std::fs::Metadata),
    /// A `struct stat` filled by `fstatat` against the pinned root.
    #[cfg(unix)]
    At(AtMetadata),
}

impl From<std::fs::Metadata> for EntryMetadata {
    fn from(metadata: std::fs::Metadata) -> Self {
        Self::Std(metadata)
    }
}

impl EntryMetadata {
    /// The entry's type bits.
    #[must_use]
    pub fn file_type(&self) -> EntryFileType {
        match self {
            Self::Std(metadata) => EntryFileType::Std(metadata.file_type()),
            #[cfg(unix)]
            Self::At(metadata) => EntryFileType::At(*metadata),
        }
    }

    /// Reports whether the entry is a directory.
    #[must_use]
    pub fn is_dir(&self) -> bool {
        match self {
            Self::Std(metadata) => metadata.is_dir(),
            #[cfg(unix)]
            Self::At(metadata) => metadata.is_dir(),
        }
    }

    /// Reports whether the entry is a regular file.
    #[must_use]
    pub fn is_file(&self) -> bool {
        match self {
            Self::Std(metadata) => metadata.is_file(),
            #[cfg(unix)]
            Self::At(metadata) => metadata.is_file(),
        }
    }

    /// Reports whether the entry is a symbolic link.
    ///
    /// Meaningful only for a lookup that did not follow the leaf; a `stat` of a
    /// symlink reports its target on both arms, exactly as
    /// [`std::fs::metadata`] does.
    #[must_use]
    pub fn is_symlink(&self) -> bool {
        match self {
            Self::Std(metadata) => metadata.is_symlink(),
            #[cfg(unix)]
            Self::At(metadata) => metadata.is_symlink(),
        }
    }

    /// Size in bytes, matching [`std::fs::Metadata::len`].
    #[must_use]
    pub fn len(&self) -> u64 {
        match self {
            Self::Std(metadata) => metadata.len(),
            #[cfg(unix)]
            Self::At(metadata) => metadata.size(),
        }
    }

    /// Reports a zero [`len`](Self::len) - upstream's `st.st_size == 0`.
    ///
    /// The sender asks it of a device node, where a zero length means "the
    /// kernel does not report this device's size in the stat" rather than "no
    /// bytes", and the real size has to be read another way.
    ///
    /// upstream: `rsync-3.5.1/flist.c:1793-1794` - `if (st.st_size == 0) st.st_size
    /// = get_device_size(...)`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Creation time, matching [`std::fs::Metadata::created`].
    ///
    /// # Errors
    ///
    /// [`io::ErrorKind::Unsupported`] when the answering syscall does not
    /// report one - which for the `At` arm means a platform whose `struct stat`
    /// has no `st_birthtime` member. Callers already treat the `Err` arm as
    /// "this source has no creation time to send", because
    /// [`std::fs::Metadata::created`] returns it on any filesystem that does
    /// not record one.
    pub fn created(&self) -> io::Result<SystemTime> {
        match self {
            Self::Std(metadata) => metadata.created(),
            #[cfg(unix)]
            Self::At(metadata) => match metadata.birthtime() {
                Some((secs, nsecs)) => Ok(system_time(secs, nsecs)),
                None => Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "creation time is not available from fstatat on this platform",
                )),
            },
        }
    }

    /// Modification time, matching [`std::fs::Metadata::modified`].
    ///
    /// Unix callers read [`mtime`](Self::mtime) and
    /// [`mtime_nsec`](Self::mtime_nsec) instead, which is what upstream's
    /// `make_file()` puts on the wire; this is the non-Unix shape of the same
    /// field.
    ///
    /// # Errors
    ///
    /// The underlying [`std::fs::Metadata::modified`] error.
    #[cfg(not(unix))]
    pub fn modified(&self) -> io::Result<SystemTime> {
        match self {
            Self::Std(metadata) => metadata.modified(),
        }
    }

    /// Access time, matching [`std::fs::Metadata::accessed`].
    ///
    /// # Errors
    ///
    /// The underlying [`std::fs::Metadata::accessed`] error.
    #[cfg(not(unix))]
    pub fn accessed(&self) -> io::Result<SystemTime> {
        match self {
            Self::Std(metadata) => metadata.accessed(),
        }
    }

    /// Permissions, matching [`std::fs::Metadata::permissions`].
    #[cfg(not(unix))]
    #[must_use]
    pub fn permissions(&self) -> std::fs::Permissions {
        match self {
            Self::Std(metadata) => metadata.permissions(),
        }
    }

    /// Windows file attributes, matching
    /// [`std::os::windows::fs::MetadataExt::file_attributes`].
    #[cfg(windows)]
    #[must_use]
    pub fn file_attributes(&self) -> u32 {
        use std::os::windows::fs::MetadataExt as _;
        match self {
            Self::Std(metadata) => metadata.file_attributes(),
        }
    }
}

/// The `MetadataExt`-shaped half of the surface.
///
/// Split out so the `#[cfg(unix)]` gate is stated once instead of on ten
/// methods.
#[cfg(unix)]
impl EntryMetadata {
    /// Raw `st_mode`, matching [`std::os::unix::fs::MetadataExt::mode`].
    #[must_use]
    pub fn mode(&self) -> u32 {
        match self {
            Self::Std(metadata) => std::os::unix::fs::MetadataExt::mode(metadata),
            Self::At(metadata) => metadata.mode(),
        }
    }

    /// Owning user id, matching [`std::os::unix::fs::MetadataExt::uid`].
    #[must_use]
    pub fn uid(&self) -> u32 {
        match self {
            Self::Std(metadata) => std::os::unix::fs::MetadataExt::uid(metadata),
            Self::At(metadata) => metadata.uid(),
        }
    }

    /// Owning group id, matching [`std::os::unix::fs::MetadataExt::gid`].
    #[must_use]
    pub fn gid(&self) -> u32 {
        match self {
            Self::Std(metadata) => std::os::unix::fs::MetadataExt::gid(metadata),
            Self::At(metadata) => metadata.gid(),
        }
    }

    /// Device id the entry *is*, matching
    /// [`std::os::unix::fs::MetadataExt::rdev`].
    #[must_use]
    pub fn rdev(&self) -> u64 {
        match self {
            Self::Std(metadata) => std::os::unix::fs::MetadataExt::rdev(metadata),
            Self::At(metadata) => metadata.rdev(),
        }
    }

    /// Device id of the filesystem holding the entry, matching
    /// [`std::os::unix::fs::MetadataExt::dev`].
    #[must_use]
    pub fn dev(&self) -> u64 {
        match self {
            Self::Std(metadata) => std::os::unix::fs::MetadataExt::dev(metadata),
            Self::At(metadata) => metadata.dev(),
        }
    }

    /// Inode number, matching [`std::os::unix::fs::MetadataExt::ino`].
    #[must_use]
    pub fn ino(&self) -> u64 {
        match self {
            Self::Std(metadata) => std::os::unix::fs::MetadataExt::ino(metadata),
            Self::At(metadata) => metadata.ino(),
        }
    }

    /// Hard link count, matching [`std::os::unix::fs::MetadataExt::nlink`].
    #[must_use]
    pub fn nlink(&self) -> u64 {
        match self {
            Self::Std(metadata) => std::os::unix::fs::MetadataExt::nlink(metadata),
            Self::At(metadata) => metadata.nlink(),
        }
    }

    /// Whole-second modification time, matching
    /// [`std::os::unix::fs::MetadataExt::mtime`].
    #[must_use]
    pub fn mtime(&self) -> i64 {
        match self {
            Self::Std(metadata) => std::os::unix::fs::MetadataExt::mtime(metadata),
            Self::At(metadata) => metadata.mtime(),
        }
    }

    /// Sub-second modification time, matching
    /// [`std::os::unix::fs::MetadataExt::mtime_nsec`].
    #[must_use]
    pub fn mtime_nsec(&self) -> i64 {
        match self {
            Self::Std(metadata) => std::os::unix::fs::MetadataExt::mtime_nsec(metadata),
            Self::At(metadata) => metadata.mtime_nsec(),
        }
    }

    /// Whole-second access time, matching
    /// [`std::os::unix::fs::MetadataExt::atime`].
    #[must_use]
    pub fn atime(&self) -> i64 {
        match self {
            Self::Std(metadata) => std::os::unix::fs::MetadataExt::atime(metadata),
            Self::At(metadata) => metadata.atime(),
        }
    }
    /// Sub-second access time, matching
    /// [`std::os::unix::fs::MetadataExt::atime_nsec`].
    #[must_use]
    pub fn atime_nsec(&self) -> i64 {
        match self {
            Self::Std(metadata) => std::os::unix::fs::MetadataExt::atime_nsec(metadata),
            Self::At(metadata) => metadata.atime_nsec(),
        }
    }
}

impl EntryMetadata {
    /// Modification time as a [`filetime::FileTime`], matching
    /// [`filetime::FileTime::from_last_modification_time`].
    #[must_use]
    pub fn modified_filetime(&self) -> filetime::FileTime {
        match self {
            Self::Std(metadata) => filetime::FileTime::from_last_modification_time(metadata),
            #[cfg(unix)]
            Self::At(metadata) => {
                filetime::FileTime::from_unix_time(metadata.mtime(), metadata.mtime_nsec() as u32)
            }
        }
    }
    /// Access time as a [`filetime::FileTime`], matching
    /// [`filetime::FileTime::from_last_access_time`].
    #[must_use]
    pub fn accessed_filetime(&self) -> filetime::FileTime {
        match self {
            Self::Std(metadata) => filetime::FileTime::from_last_access_time(metadata),
            #[cfg(unix)]
            Self::At(metadata) => {
                filetime::FileTime::from_unix_time(metadata.atime(), metadata.atime_nsec() as u32)
            }
        }
    }
}

/// The type bits of a [`EntryMetadata`], from whichever lookup answered.
///
/// Mirrors [`std::fs::FileType`] plus the
/// [`std::os::unix::fs::FileTypeExt`] predicates, for the same reason
/// [`EntryMetadata`] mirrors `MetadataExt`.
#[derive(Clone, Copy, Debug)]
pub enum EntryFileType {
    /// Type bits carried by a [`std::fs::Metadata`].
    Std(std::fs::FileType),
    /// Type bits read out of a `struct stat`.
    #[cfg(unix)]
    At(AtMetadata),
}

impl EntryFileType {
    /// Reports whether the entry is a regular file.
    #[must_use]
    pub fn is_file(&self) -> bool {
        match self {
            Self::Std(file_type) => file_type.is_file(),
            #[cfg(unix)]
            Self::At(metadata) => metadata.is_file(),
        }
    }

    /// Reports whether the entry is a directory.
    #[must_use]
    pub fn is_dir(&self) -> bool {
        match self {
            Self::Std(file_type) => file_type.is_dir(),
            #[cfg(unix)]
            Self::At(metadata) => metadata.is_dir(),
        }
    }

    /// Reports whether the entry is a symbolic link.
    #[must_use]
    pub fn is_symlink(&self) -> bool {
        match self {
            Self::Std(file_type) => file_type.is_symlink(),
            #[cfg(unix)]
            Self::At(metadata) => metadata.is_symlink(),
        }
    }
}

/// The [`std::os::unix::fs::FileTypeExt`] half of the surface.
#[cfg(unix)]
impl EntryFileType {
    /// Reports whether the entry is a block device.
    #[must_use]
    pub fn is_block_device(&self) -> bool {
        match self {
            Self::Std(file_type) => std::os::unix::fs::FileTypeExt::is_block_device(file_type),
            Self::At(metadata) => metadata.is_block_device(),
        }
    }

    /// Reports whether the entry is a character device.
    #[must_use]
    pub fn is_char_device(&self) -> bool {
        match self {
            Self::Std(file_type) => std::os::unix::fs::FileTypeExt::is_char_device(file_type),
            Self::At(metadata) => metadata.is_char_device(),
        }
    }

    /// Reports whether the entry is a FIFO.
    #[must_use]
    pub fn is_fifo(&self) -> bool {
        match self {
            Self::Std(file_type) => std::os::unix::fs::FileTypeExt::is_fifo(file_type),
            Self::At(metadata) => metadata.is_fifo(),
        }
    }

    /// Reports whether the entry is a unix-domain socket.
    #[must_use]
    pub fn is_socket(&self) -> bool {
        match self {
            Self::Std(file_type) => std::os::unix::fs::FileTypeExt::is_socket(file_type),
            Self::At(metadata) => metadata.is_socket(),
        }
    }
}

/// Rebuild a [`SystemTime`] from a `struct stat` time pair.
///
/// A pre-epoch timestamp is legal on disk and `st_*time` is signed, so the
/// negative case subtracts rather than saturating to the epoch - the same
/// answer `std::fs` gives for the same inode.
#[cfg(unix)]
fn system_time(secs: i64, nsecs: i64) -> SystemTime {
    let nsecs = u32::try_from(nsecs).unwrap_or_default();
    if secs >= 0 {
        SystemTime::UNIX_EPOCH + Duration::new(secs.unsigned_abs(), nsecs)
    } else {
        SystemTime::UNIX_EPOCH - Duration::new(secs.unsigned_abs(), 0) + Duration::new(0, nsecs)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::ffi::OsStr;
    use std::fs::File;
    use std::os::fd::AsFd;
    use std::os::unix::fs::{FileTypeExt, MetadataExt, symlink};
    use std::path::Path;

    use filetime::FileTime;

    use super::EntryMetadata;
    use crate::dir_sandbox::at_syscalls::mkfifoat;
    use crate::dir_sandbox::fstatat_nofollow;

    const ATIME: (i64, u32) = (1_600_000_000, 123_456_789);
    const MTIME: (i64, u32) = (1_650_000_000, 987_654_321);

    /// Asserts both variants report exactly what `lstat` reports for `leaf`.
    ///
    /// The quick-check compares mtime to the nanosecond, so a variant that
    /// truncated or swapped the sub-second field would turn every unchanged
    /// file into a transfer; the stamps are set with distinct non-zero
    /// nanoseconds so such a slip cannot pass.
    fn assert_variants_agree(root: &Path, leaf: &str) {
        let path = root.join(leaf);
        filetime::set_symlink_file_times(
            &path,
            FileTime::from_unix_time(ATIME.0, ATIME.1),
            FileTime::from_unix_time(MTIME.0, MTIME.1),
        )
        .expect("stamp times");
        let raw = std::fs::symlink_metadata(&path).expect("lstat");
        assert_eq!(
            raw.mtime_nsec(),
            i64::from(MTIME.1),
            "{leaf}: fs keeps nanoseconds"
        );

        let dir = File::open(root).expect("open root");
        let at =
            EntryMetadata::At(fstatat_nofollow(dir.as_fd(), OsStr::new(leaf)).expect("fstatat"));
        let std = EntryMetadata::Std(raw.clone());
        for meta in [&at, &std] {
            let fields = [
                ("mode", u64::from(meta.mode()), u64::from(raw.mode())),
                ("uid", u64::from(meta.uid()), u64::from(raw.uid())),
                ("gid", u64::from(meta.gid()), u64::from(raw.gid())),
                ("dev", meta.dev(), raw.dev()),
                ("ino", meta.ino(), raw.ino()),
                ("nlink", meta.nlink(), raw.nlink()),
                ("rdev", meta.rdev(), raw.rdev()),
                ("size", meta.len(), raw.len()),
            ];
            for (name, got, want) in fields {
                assert_eq!(got, want, "{leaf}: {name} of {meta:?}");
            }
            assert_eq!(
                (meta.mtime(), meta.mtime_nsec()),
                (raw.mtime(), raw.mtime_nsec()),
                "{leaf}: mtime"
            );
            assert_eq!(
                (meta.atime(), meta.atime_nsec()),
                (raw.atime(), raw.atime_nsec()),
                "{leaf}: atime"
            );
            assert_eq!(
                meta.modified_filetime(),
                FileTime::from_last_modification_time(&raw),
                "{leaf}"
            );
            assert_eq!(
                meta.accessed_filetime(),
                FileTime::from_last_access_time(&raw),
                "{leaf}"
            );
            let (kind, want) = (meta.file_type(), raw.file_type());
            assert_eq!(
                [
                    kind.is_file(),
                    kind.is_dir(),
                    kind.is_symlink(),
                    kind.is_fifo()
                ],
                [
                    want.is_file(),
                    want.is_dir(),
                    want.is_symlink(),
                    want.is_fifo()
                ],
                "{leaf}: file type"
            );
        }
    }

    #[test]
    fn at_and_std_variants_report_identical_fields_for_every_entry_kind() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        std::fs::write(root.join("file"), b"seven b").expect("write file");
        std::fs::create_dir(root.join("dir")).expect("mkdir");
        symlink("file", root.join("link")).expect("symlink");
        let dir = File::open(root).expect("open root");
        mkfifoat(dir.as_fd(), OsStr::new("fifo"), 0o640).expect("mkfifo");

        for leaf in ["file", "dir", "link", "fifo"] {
            assert_variants_agree(root, leaf);
        }
    }
}
