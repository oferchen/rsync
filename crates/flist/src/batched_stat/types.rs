//! Lightweight metadata result types from low-level stat syscalls.

/// Lightweight metadata result from fstatat(2).
///
/// Contains only the fields rsync needs during file list generation,
/// constructed directly from the `libc::stat` buffer without a second syscall.
/// Available on all Unix platforms.
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

/// Widens a platform-specific integer to `u32`.
///
/// `libc::mode_t` and the `S_IF*` constants are `u16` on macOS but `u32` on
/// Linux. A bare `.into()` or `u32::from()` triggers `useless_conversion` on
/// Linux while `as u32` triggers `unnecessary_cast`. This generic helper
/// avoids both lints because clippy does not resolve the concrete type through
/// the trait bound.
#[cfg(unix)]
#[inline]
pub(crate) fn to_u32<T: Into<u32>>(v: T) -> u32 {
    v.into()
}

#[cfg(unix)]
impl FstatResult {
    /// Constructs from a raw `libc::stat` buffer.
    pub(crate) fn from_stat(stat_buf: &libc::stat) -> Self {
        // dev_t is u64 on Linux, i32 on macOS - use cfg to avoid cross-platform lint.
        #[cfg(target_os = "linux")]
        let rdev = stat_buf.st_rdev;
        #[cfg(not(target_os = "linux"))]
        let rdev: u64 = stat_buf.st_rdev.try_into().unwrap_or_default();
        Self {
            mode: to_u32(stat_buf.st_mode),
            size: stat_buf.st_size as u64,
            mtime_sec: stat_buf.st_mtime,
            mtime_nsec: stat_buf.st_mtime_nsec as u32,
            uid: stat_buf.st_uid,
            gid: stat_buf.st_gid,
            ino: stat_buf.st_ino,
            nlink: stat_buf.st_nlink as u32,
            rdev_major: rdev_major(rdev),
            rdev_minor: rdev_minor(rdev),
        }
    }

    /// Returns true if this entry is a regular file.
    #[must_use]
    pub fn is_file(&self) -> bool {
        (self.mode & to_u32(libc::S_IFMT)) == to_u32(libc::S_IFREG)
    }

    /// Returns true if this entry is a directory.
    #[must_use]
    pub fn is_dir(&self) -> bool {
        (self.mode & to_u32(libc::S_IFMT)) == to_u32(libc::S_IFDIR)
    }

    /// Returns true if this entry is a symbolic link.
    #[must_use]
    pub fn is_symlink(&self) -> bool {
        (self.mode & to_u32(libc::S_IFMT)) == to_u32(libc::S_IFLNK)
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

/// Lightweight metadata result from statx(2).
///
/// Contains only the fields rsync needs during file list generation,
/// avoiding the overhead of constructing a full `fs::Metadata`. On Linux 4.11+
/// the kernel can skip computing unwanted fields when the request mask
/// excludes them.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[derive(Debug, Clone)]
pub struct StatxResult {
    /// File type and permission bits (stx_mode).
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
    /// Device ID major.
    pub rdev_major: u32,
    /// Device ID minor.
    pub rdev_minor: u32,
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
impl StatxResult {
    /// Returns true if this entry is a regular file.
    #[must_use]
    pub fn is_file(&self) -> bool {
        (self.mode & libc::S_IFMT) == libc::S_IFREG
    }

    /// Returns true if this entry is a directory.
    #[must_use]
    pub fn is_dir(&self) -> bool {
        (self.mode & libc::S_IFMT) == libc::S_IFDIR
    }

    /// Returns true if this entry is a symbolic link.
    #[must_use]
    pub fn is_symlink(&self) -> bool {
        (self.mode & libc::S_IFMT) == libc::S_IFLNK
    }

    /// Returns the permission bits (lower 12 bits of mode).
    #[must_use]
    pub fn permissions(&self) -> u32 {
        self.mode & 0o7777
    }
}
