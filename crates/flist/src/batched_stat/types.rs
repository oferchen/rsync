//! Lightweight metadata result types from low-level stat syscalls.
//!
//! The statx sibling (`StatxResult`) lives in `fast_io::statx` next to the
//! syscall that fills it and is re-exported from this crate's `batched_stat`
//! module.

/// POSIX file-type mask and type constants. The values are identical on every
/// supported Unix (Linux, macOS), spelled as literals so this crate needs no
/// direct libc dependency.
#[cfg(unix)]
const S_IFMT: u32 = 0o170000;
#[cfg(unix)]
const S_IFREG: u32 = 0o100000;
#[cfg(unix)]
const S_IFDIR: u32 = 0o040000;
#[cfg(unix)]
const S_IFLNK: u32 = 0o120000;

/// Lightweight metadata result from fstatat(2).
///
/// Contains only the fields rsync needs during file list generation,
/// constructed directly from `fast_io`'s fstatat result without a second
/// syscall. Available on all Unix platforms.
#[cfg(unix)]
#[derive(Debug, Clone)]
pub struct FstatResult {
    /// File type and permission bits (st_mode).
    pub mode: u32,
    /// File size in bytes.
    pub size: u64,
    /// Last modification time (seconds since epoch).
    pub mtime_sec: i64,
    /// Last modification time (nanoseconds component).
    pub mtime_nsec: u32,
    /// User ID of the owner.
    pub uid: u32,
    /// Group ID of the owner.
    pub gid: u32,
    /// Inode number.
    pub ino: u64,
    /// Number of hard links.
    pub nlink: u32,
    /// Device major number.
    pub rdev_major: u32,
    /// Device minor number.
    pub rdev_minor: u32,
}

#[cfg(unix)]
impl FstatResult {
    /// Constructs from the typed `fstatat(2)` result `fast_io` returns.
    pub(crate) fn from_at_metadata(at: &fast_io::AtMetadata) -> Self {
        // dev_t is u64 on Linux but i32 on macOS; `AtMetadata::rdev` widens by
        // sign extension, so reject a negative bit pattern the same way the
        // previous `i32::try_into().unwrap_or_default()` did.
        #[cfg(target_os = "linux")]
        let rdev = at.rdev();
        #[cfg(not(target_os = "linux"))]
        let rdev = u32::try_from(at.rdev()).map(u64::from).unwrap_or_default();
        Self {
            mode: at.mode(),
            size: at.size(),
            mtime_sec: at.mtime(),
            mtime_nsec: at.mtime_nsec() as u32,
            uid: at.uid(),
            gid: at.gid(),
            ino: at.ino(),
            nlink: at.nlink() as u32,
            rdev_major: rdev_major(rdev),
            rdev_minor: rdev_minor(rdev),
        }
    }

    /// Returns true if this entry is a regular file.
    #[must_use]
    pub fn is_file(&self) -> bool {
        (self.mode & S_IFMT) == S_IFREG
    }

    /// Returns true if this entry is a directory.
    #[must_use]
    pub fn is_dir(&self) -> bool {
        (self.mode & S_IFMT) == S_IFDIR
    }

    /// Returns true if this entry is a symbolic link.
    #[must_use]
    pub fn is_symlink(&self) -> bool {
        (self.mode & S_IFMT) == S_IFLNK
    }

    /// Returns the permission bits (lower 12 bits of mode).
    #[must_use]
    pub fn permissions(&self) -> u32 {
        self.mode & 0o7777
    }
}

/// Extracts the major device number from a combined rdev value (Linux glibc encoding).
#[cfg(all(unix, target_os = "linux"))]
fn rdev_major(rdev: u64) -> u32 {
    ((rdev >> 8) & 0xfff) as u32 | (((rdev >> 32) & !0xfff) as u32)
}

/// Extracts the major device number from a combined rdev value (BSD/macOS encoding).
#[cfg(all(unix, not(target_os = "linux")))]
fn rdev_major(rdev: u64) -> u32 {
    ((rdev >> 24) & 0xff) as u32
}

/// Extracts the minor device number from a combined rdev value (Linux glibc encoding).
#[cfg(all(unix, target_os = "linux"))]
fn rdev_minor(rdev: u64) -> u32 {
    (rdev & 0xff) as u32 | (((rdev >> 12) & !0xff) as u32)
}

/// Extracts the minor device number from a combined rdev value (BSD/macOS encoding).
#[cfg(all(unix, not(target_os = "linux")))]
fn rdev_minor(rdev: u64) -> u32 {
    (rdev & 0xffffff) as u32
}
