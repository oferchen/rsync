//! Platform-abstracted file copy trait with automatic optimization selection.
//!
//! This module provides the [`PlatformCopy`] trait that abstracts platform-specific
//! file copy optimizations behind a unified interface. Each platform selects the
//! best available mechanism at runtime, falling back through increasingly portable
//! options.
//!
//! # Platform Optimization Chain
//!
//! | Platform | Primary | Secondary | Fallback |
//! |----------|---------|-----------|----------|
//! | Linux | `FICLONE` (CoW reflink, Btrfs/XFS) | `copy_file_range` (zero-copy) | `std::fs::copy` |
//! | macOS | `clonefile` (CoW, APFS) | `fcopyfile` (kernel-accelerated) | `std::fs::copy` |
//! | Windows | ReFS `FSCTL_DUPLICATE_EXTENTS` (CoW) | `CopyFileExW` (no-buffering) | `std::fs::copy` |
//!
//! # Design
//!
//! The trait follows the **Strategy Pattern** - implementations are interchangeable
//! at runtime. A [`DefaultPlatformCopy`] is provided that auto-selects the best
//! mechanism for the current platform. Callers can also inject custom implementations
//! for testing or specialized behavior.
//!
//! # Standalone Functions
//!
//! In addition to the trait, two standalone functions are provided for direct
//! access to macOS copy primitives:
//!
//! - [`try_clonefile`] - Uses `clonefile(2)` for APFS copy-on-write reflinks.
//! - [`try_fcopyfile`] - Uses `fcopyfile(3)` for kernel-accelerated file copies.
//!
//! Both return `ErrorKind::Unsupported` on non-macOS platforms, enabling callers
//! to build a fallback chain without `#[cfg]` branching at every call site.
//!
//! # Example
//!
//! ```no_run
//! use fast_io::platform_copy::{DefaultPlatformCopy, PlatformCopy};
//! use std::path::Path;
//!
//! let copier = DefaultPlatformCopy::new();
//! let result = copier.copy_file(
//!     Path::new("source.bin"),
//!     Path::new("dest.bin"),
//!     1024 * 1024,
//! ).expect("copy succeeds");
//! println!("Copied {} bytes via {:?}", result.bytes_copied, result.method);
//! ```

#[cfg(target_os = "linux")]
mod cow_detect;
#[cfg(not(target_os = "linux"))]
#[path = "cow_detect_stub.rs"]
mod cow_detect;
mod dispatch;
mod no_zero_copy;
#[cfg(test)]
mod tests;
mod types;

use std::io;
use std::path::Path;

pub use no_zero_copy::NoZeroCopyPlatformCopy;
pub use types::{CopyMethod, CopyResult, PlatformCopy};

/// Default platform copy implementation with automatic mechanism selection.
///
/// Selects the best available copy mechanism for the current platform:
///
/// - **Linux**: `FICLONE` (CoW reflink) then `copy_file_range` then `std::fs::copy`
/// - **macOS**: `clonefile` (CoW) then `fcopyfile` (kernel-accelerated) then `std::fs::copy`
/// - **Windows**: ReFS `FSCTL_DUPLICATE_EXTENTS` reflink, then `CopyFileExW` with no-buffering for files > 4MB
///
/// All paths automatically fall back to standard copy on failure.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultPlatformCopy;

impl DefaultPlatformCopy {
    /// Creates a new `DefaultPlatformCopy` instance.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl PlatformCopy for DefaultPlatformCopy {
    fn copy_file(&self, src: &Path, dst: &Path, size_hint: u64) -> io::Result<CopyResult> {
        dispatch::platform_copy_impl(src, dst, size_hint)
    }

    fn supports_reflink(&self) -> bool {
        dispatch::platform_supports_reflink()
    }

    fn preferred_method(&self, size: u64) -> CopyMethod {
        dispatch::platform_preferred_method(size)
    }
}

/// Platform copy strategy that disables copy-on-write reflinks.
///
/// Forces every whole-file copy through the portable `std::fs::copy`
/// fallback, bypassing `FICLONE`, `copy_file_range`, `clonefile`,
/// `fcopyfile`, `FSCTL_DUPLICATE_EXTENTS`, and `CopyFileExW`. The result
/// always reports [`CopyMethod::StandardCopy`] so callers that probe
/// [`CopyResult::is_zero_copy`] (such as the macOS clonefile fast path)
/// transparently fall through to the regular copy code path.
///
/// Selected by the `--no-cow` CLI flag via [`super::CowPolicy::Disabled`].
#[derive(Debug, Clone, Copy, Default)]
pub struct NoCowPlatformCopy;

impl NoCowPlatformCopy {
    /// Creates a new `NoCowPlatformCopy` instance.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl PlatformCopy for NoCowPlatformCopy {
    fn copy_file(&self, src: &Path, dst: &Path, _size_hint: u64) -> io::Result<CopyResult> {
        let bytes = std::fs::copy(src, dst)?;
        Ok(CopyResult::new(bytes, CopyMethod::StandardCopy))
    }

    fn supports_reflink(&self) -> bool {
        false
    }

    fn preferred_method(&self, _size: u64) -> CopyMethod {
        CopyMethod::StandardCopy
    }
}

/// Attempts a copy-on-write block clone using Windows ReFS `FSCTL_DUPLICATE_EXTENTS_TO_FILE`.
///
/// On ReFS volumes, creates an instant reflink where source and destination share
/// storage blocks until either file is modified. The operation is O(1) regardless
/// of file size when both files are on the same ReFS volume.
///
/// The ioctl requires cluster-aligned offsets and byte count. This function
/// queries the cluster size at runtime via `GetDiskFreeSpaceW`, rounds the file
/// size up to the nearest cluster boundary for the ioctl, then truncates the
/// destination to the actual file size afterward.
///
/// # Constraints
///
/// - Source and destination must be on the same ReFS volume.
/// - The volume must be formatted as ReFS (NTFS does not support block cloning).
/// - Windows Server 2016+ or Windows 10+ with a ReFS-formatted volume.
///
/// # Platform Support
///
/// - **Windows**: Calls `DeviceIoControl` with `FSCTL_DUPLICATE_EXTENTS_TO_FILE`.
/// - **Other platforms**: Returns `ErrorKind::Unsupported`.
///
/// # Errors
///
/// Returns an error if:
/// - The platform does not support ReFS reflink (non-Windows)
/// - The filesystem is not ReFS (NTFS, FAT32, exFAT)
/// - Cross-volume (source and destination on different volumes)
/// - Source does not exist or is not readable
/// - Cluster size query fails
/// - The ioctl itself fails
pub fn try_refs_reflink(src: &Path, dst: &Path) -> io::Result<()> {
    dispatch::try_refs_reflink_impl(src, dst)
}

/// Attempts a partial copy-on-write block clone using Windows ReFS
/// `FSCTL_DUPLICATE_EXTENTS_TO_FILE`.
///
/// Clones `byte_count` bytes from `src` starting at `src_offset` into `dst`
/// at `dst_offset`. All offsets and the byte count are rounded to the volume's
/// cluster size internally (the ioctl requires cluster-aligned parameters).
///
/// The destination file must already exist and be large enough to hold the
/// cloned range at the target offset. Unlike [`try_refs_reflink`], this
/// function does not create or resize the destination.
///
/// # Constraints
///
/// - Source and destination must be on the same ReFS volume.
/// - The volume must be formatted as ReFS (NTFS does not support block cloning).
/// - The destination must be pre-created and pre-sized by the caller.
/// - Windows Server 2016+ or Windows 10+ with a ReFS-formatted volume.
///
/// # Platform Support
///
/// - **Windows**: Calls `DeviceIoControl` with `FSCTL_DUPLICATE_EXTENTS_TO_FILE`.
/// - **Other platforms**: Returns `ErrorKind::Unsupported`.
///
/// # Errors
///
/// Returns an error if:
/// - The platform does not support ReFS reflink (non-Windows)
/// - The filesystem is not ReFS (NTFS, FAT32, exFAT)
/// - Cross-volume (source and destination on different volumes)
/// - Source or destination does not exist or is not accessible
/// - Cluster size query fails
/// - The ioctl itself fails
pub fn try_refs_reflink_range(
    src: &Path,
    dst: &Path,
    src_offset: u64,
    dst_offset: u64,
    byte_count: u64,
) -> io::Result<()> {
    dispatch::try_refs_reflink_range_impl(src, dst, src_offset, dst_offset, byte_count)
}

/// Attempts a copy-on-write clone using Linux `FICLONE` ioctl.
///
/// On Btrfs, XFS (with reflink enabled), and bcachefs, `FICLONE` creates an
/// instant reflink where source and destination share storage blocks until
/// either file is modified. The operation is O(1) regardless of file size.
///
/// Uses `rustix::fs::ioctl_ficlone` internally - fully safe, no raw FFI.
///
/// # Constraints
///
/// - Source and destination must be on the same filesystem.
/// - The filesystem must support reflinks.
/// - Only regular files are supported.
///
/// # Platform Support
///
/// - **Linux**: Calls `FICLONE` ioctl via rustix.
/// - **Other platforms**: Returns `ErrorKind::Unsupported`.
///
/// # Errors
///
/// Returns an error if:
/// - The platform does not support `FICLONE` (non-Linux)
/// - The filesystem does not support reflinks (ext4, tmpfs, NFS, FUSE)
/// - Cross-filesystem (source and destination on different mounts)
/// - Source does not exist or is not readable
/// - I/O error during the clone operation
pub fn try_ficlone(src: &Path, dst: &Path) -> io::Result<()> {
    dispatch::try_ficlone_impl(src, dst)
}

/// Attempts a copy-on-write clone using `clonefile(2)`.
///
/// On APFS, `clonefile` creates an instant reflink where source and destination
/// share storage blocks until either file is modified. The operation is O(1)
/// regardless of file size.
///
/// # Constraints
///
/// - Source and destination must be on the same APFS volume.
/// - Destination must not already exist (`clonefile` does not overwrite).
/// - Only regular files and directories are supported.
///
/// # Platform Support
///
/// - **macOS**: Calls `clonefile(2)` with flags=0 (follows symlinks).
/// - **Other platforms**: Returns `ErrorKind::Unsupported`.
///
/// # Errors
///
/// Returns an error if:
/// - The platform does not support `clonefile` (non-macOS)
/// - Source does not exist or is not readable
/// - Destination already exists
/// - Cross-filesystem (different mount points)
/// - Filesystem does not support CoW (e.g., HFS+)
pub fn try_clonefile(src: &Path, dst: &Path) -> io::Result<()> {
    dispatch::clonefile_impl(src, dst)
}

/// Copies a file using `fcopyfile(3)` with kernel-accelerated transfer.
///
/// `fcopyfile` operates on open file descriptors and copies file data using
/// the `COPYFILE_DATA` flag. The kernel may use server-side copy, CoW, or
/// optimized buffer transfer internally depending on the filesystem.
///
/// Unlike `clonefile`, this function:
/// - Works across different filesystems
/// - Works on non-APFS volumes (HFS+, NFS, SMB)
/// - Overwrites the destination (caller must create/truncate the destination file)
///
/// # Platform Support
///
/// - **macOS**: Opens both files and calls `fcopyfile(3)` with `COPYFILE_DATA`.
/// - **Other platforms**: Returns `ErrorKind::Unsupported`.
///
/// # Errors
///
/// Returns an error if:
/// - The platform does not support `fcopyfile` (non-macOS)
/// - Source does not exist or is not readable
/// - Destination cannot be created or written to
/// - I/O error during the kernel copy
pub fn try_fcopyfile(src: &Path, dst: &Path) -> io::Result<()> {
    dispatch::fcopyfile_impl(src, dst)
}
