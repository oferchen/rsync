//! Core types for platform-abstracted file copy operations.

use std::fmt;
use std::io;
use std::path::Path;

/// Method used to perform the file copy.
///
/// Indicates which platform optimization was used for a copy operation.
/// Callers can use this for logging, statistics, or adaptive strategy selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CopyMethod {
    /// Linux `copy_file_range` syscall - zero-copy in kernel space.
    ///
    /// Available on Linux 4.5+ (same-filesystem) and 5.3+ (cross-filesystem).
    CopyFileRange,

    /// macOS `clonefile` syscall - copy-on-write clone.
    ///
    /// Instant, zero data copied. Available on APFS filesystems.
    Clonefile,

    /// macOS `copyfile` or `fcopyfile` - platform-optimized copy.
    ///
    /// Uses the Darwin `copyfile` API which handles metadata, ACLs, and
    /// resource forks natively.
    Copyfile,

    /// Windows `CopyFileExW` API - with optional `COPY_FILE_NO_BUFFERING`.
    ///
    /// Bypasses system cache for large files (> 4MB), reducing memory pressure.
    CopyFileEx,

    /// Standard buffered read/write - portable fallback.
    ///
    /// Uses `std::fs::copy` or manual read/write loop with 256KB buffer.
    /// Available on all platforms.
    StandardCopy,
}

impl fmt::Display for CopyMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CopyMethod::CopyFileRange => write!(f, "copy_file_range"),
            CopyMethod::Clonefile => write!(f, "clonefile"),
            CopyMethod::Copyfile => write!(f, "copyfile"),
            CopyMethod::CopyFileEx => write!(f, "CopyFileExW"),
            CopyMethod::StandardCopy => write!(f, "standard copy"),
        }
    }
}

/// Result of a platform copy operation.
///
/// Contains both the number of bytes transferred and the method used,
/// enabling callers to collect statistics about copy path selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyResult {
    /// Number of bytes copied (0 for clonefile, actual bytes for other methods).
    pub bytes_copied: u64,
    /// The platform mechanism that performed the copy.
    pub method: CopyMethod,
}

impl CopyResult {
    /// Creates a new `CopyResult`.
    #[must_use]
    pub fn new(bytes_copied: u64, method: CopyMethod) -> Self {
        Self {
            bytes_copied,
            method,
        }
    }

    /// Returns true if the copy used a zero-copy or CoW mechanism.
    ///
    /// Zero-copy methods transfer data entirely in kernel space without
    /// copying bytes through userspace buffers.
    #[must_use]
    pub fn is_zero_copy(&self) -> bool {
        matches!(
            self.method,
            CopyMethod::CopyFileRange | CopyMethod::Clonefile
        )
    }
}

/// Platform-abstracted file copy interface.
///
/// Implementations select the best available copy mechanism for the current
/// platform and filesystem, with automatic fallback to portable alternatives.
///
/// # Thread Safety
///
/// Implementations must be `Send + Sync` to support parallel file processing
/// via rayon. The trait methods take `&self` (shared reference) since copy
/// operations are inherently independent per source/destination pair.
///
/// # Implementors
///
/// - [`DefaultPlatformCopy`](super::DefaultPlatformCopy) - auto-selects the best mechanism per platform
/// - Custom implementations can be provided for testing or specialized behavior
pub trait PlatformCopy: Send + Sync {
    /// Copies a file from `src` to `dst`, selecting the best platform mechanism.
    ///
    /// The `size_hint` parameter is advisory - it helps select the optimal copy
    /// strategy (e.g., unbuffered I/O for large files on Windows) but the actual
    /// number of bytes copied may differ if the file size changes between stat
    /// and copy.
    ///
    /// # Arguments
    ///
    /// * `src` - Source file path (must exist and be readable)
    /// * `dst` - Destination file path (created or overwritten)
    /// * `size_hint` - Expected file size in bytes (advisory, for strategy selection)
    ///
    /// # Returns
    ///
    /// A [`CopyResult`] indicating the bytes copied and the method used.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Source file does not exist or is not readable
    /// - Destination cannot be created (permission denied, invalid path)
    /// - I/O error during transfer
    /// - All platform mechanisms and fallbacks fail
    fn copy_file(&self, src: &Path, dst: &Path, size_hint: u64) -> io::Result<CopyResult>;

    /// Returns whether the current platform and filesystem support reflink/CoW cloning.
    ///
    /// This is a best-effort check. Even when this returns `true`, individual
    /// clone operations may fail (e.g., cross-device, unsupported filesystem type).
    ///
    /// # Platform Behavior
    ///
    /// - **macOS**: Returns `true` (APFS supports clonefile; HFS+ does not but
    ///   filesystem detection is deferred to the actual clone call)
    /// - **Linux**: Returns `false` (reflink via `FICLONE` ioctl is not yet implemented)
    /// - **Windows**: Returns `false` (block cloning requires ReFS and is not yet implemented)
    fn supports_reflink(&self) -> bool;

    /// Returns the preferred [`CopyMethod`] for a file of the given size.
    ///
    /// This is advisory - the actual method used may differ based on runtime
    /// conditions (filesystem type, kernel version, syscall availability).
    /// Useful for logging or pre-allocating resources.
    ///
    /// # Arguments
    ///
    /// * `size` - Expected file size in bytes
    fn preferred_method(&self, size: u64) -> CopyMethod;
}
