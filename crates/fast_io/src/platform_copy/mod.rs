//! Platform-abstracted file copy trait with automatic optimization selection.
//!
//! This module provides the [`PlatformCopy`] trait that abstracts platform-specific
//! file copy optimizations behind a unified interface. Each platform selects the
//! best available mechanism at runtime, falling back through increasingly portable
//! options.
//!
//! # Platform Optimization Chain
//!
//! | Platform | Primary | Fallback |
//! |----------|---------|----------|
//! | Linux | `copy_file_range` (zero-copy, kernel 4.5+) | buffered read/write |
//! | macOS | `clonefile` (CoW, APFS) then `copyfile` | `std::fs::copy` |
//! | Windows | `CopyFileExW` (optional no-buffering) | `std::fs::copy` |
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

mod types;
mod dispatch;
#[cfg(test)]
mod tests;

use std::io;
use std::path::Path;

pub use types::{CopyMethod, CopyResult, PlatformCopy};

/// Default platform copy implementation with automatic mechanism selection.
///
/// Selects the best available copy mechanism for the current platform:
///
/// - **Linux**: `copy_file_range` for files >= 64KB, buffered read/write for smaller files
/// - **macOS**: `clonefile` (CoW) with `std::fs::copy` fallback
/// - **Windows**: `CopyFileExW` with `COPY_FILE_NO_BUFFERING` for files > 4MB
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
