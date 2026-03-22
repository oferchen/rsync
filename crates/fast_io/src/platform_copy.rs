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
/// - [`DefaultPlatformCopy`] - auto-selects the best mechanism per platform
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
        platform_copy_impl(src, dst, size_hint)
    }

    fn supports_reflink(&self) -> bool {
        platform_supports_reflink()
    }

    fn preferred_method(&self, size: u64) -> CopyMethod {
        platform_preferred_method(size)
    }
}

// ===========================================================================
// Standalone public functions
// ===========================================================================

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
    clonefile_impl(src, dst)
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
    fcopyfile_impl(src, dst)
}

// ===========================================================================
// Linux implementation
// ===========================================================================

/// Linux: try `copy_file_range` for large files, fall back to `std::fs::copy`.
#[cfg(target_os = "linux")]
fn platform_copy_impl(src: &Path, dst: &Path, size_hint: u64) -> io::Result<CopyResult> {
    use std::fs::File;

    // Threshold below which copy_file_range overhead exceeds benefit
    // (matches copy_file_range module constant)
    const CFR_THRESHOLD: u64 = 64 * 1024;

    if size_hint >= CFR_THRESHOLD {
        // Attempt zero-copy via copy_file_range
        let source = File::open(src)?;
        let destination = File::create(dst)?;
        match crate::copy_file_range::copy_file_contents(&source, &destination, size_hint) {
            Ok(bytes) => {
                return Ok(CopyResult::new(bytes, CopyMethod::CopyFileRange));
            }
            Err(_) => {
                // copy_file_range failed (cross-device on old kernel, NFS, FUSE, etc.)
                // Clean up partial destination and fall through to std::fs::copy
                let _ = std::fs::remove_file(dst);
            }
        }
    }

    // Fallback to standard copy (which itself may use copy_file_range internally)
    let bytes = std::fs::copy(src, dst)?;
    Ok(CopyResult::new(bytes, CopyMethod::StandardCopy))
}

// ===========================================================================
// macOS implementation
// ===========================================================================

/// macOS: try `clonefile` (CoW) first, then fall back to `std::fs::copy`.
///
/// On APFS, `clonefile` creates an instant copy-on-write clone sharing storage
/// blocks until modified. Falls back to `std::fs::copy` which uses `copyfile()`
/// under the hood on Darwin, handling metadata and resource forks natively.
#[cfg(target_os = "macos")]
fn platform_copy_impl(src: &Path, dst: &Path, _size_hint: u64) -> io::Result<CopyResult> {
    // Try CoW clone first (instant, zero data copied)
    match clonefile_impl(src, dst) {
        Ok(()) => {
            return Ok(CopyResult::new(0, CopyMethod::Clonefile));
        }
        Err(_) => {
            // clonefile failed (cross-device, HFS+, destination exists, etc.)
            // Clean up any partial destination
            let _ = std::fs::remove_file(dst);
        }
    }

    // Fall back to std::fs::copy (uses Darwin copyfile() internally)
    let bytes = std::fs::copy(src, dst)?;
    Ok(CopyResult::new(bytes, CopyMethod::Copyfile))
}

/// macOS `clonefile` FFI wrapper.
///
/// Used by both `platform_copy_impl` and the standalone `try_clonefile` function.
/// Isolated from the engine's `clonefile` module to avoid circular dependencies.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn clonefile_impl(src: &Path, dst: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let src_c = CString::new(src.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "source path contains null byte",
        )
    })?;
    let dst_c = CString::new(dst.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "destination path contains null byte",
        )
    })?;

    // SAFETY: Both pointers are valid, null-terminated C strings derived from
    // OsStr. The clonefile syscall is safe to call with valid path arguments.
    // Errors are returned via errno and converted to io::Error.
    let ret = unsafe { libc::clonefile(src_c.as_ptr(), dst_c.as_ptr(), 0) };

    if ret == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// macOS `fcopyfile` FFI wrapper.
///
/// Uses `fcopyfile(3)` with `COPYFILE_DATA` to perform a kernel-accelerated
/// data-only copy between open file descriptors.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn fcopyfile_impl(src: &Path, dst: &Path) -> io::Result<()> {
    use std::fs::File;
    use std::os::fd::AsRawFd;

    let src_file = File::open(src)?;
    let dst_file = File::create(dst)?;

    let src_fd = src_file.as_raw_fd();
    let dst_fd = dst_file.as_raw_fd();

    // fcopyfile(from_fd, to_fd, state, flags)
    // state=NULL means no progress callbacks or custom state.
    // COPYFILE_DATA copies only the data fork, not metadata.
    // SAFETY: Both file descriptors are valid and open, owned by the File
    // values above which outlive the call. NULL state is explicitly allowed
    // by the fcopyfile API. Errors are returned via the function return value
    // and errno.
    let ret = unsafe { libc::fcopyfile(src_fd, dst_fd, std::ptr::null_mut(), libc::COPYFILE_DATA) };

    if ret == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

// ===========================================================================
// Windows implementation
// ===========================================================================

/// Windows: try `CopyFileExW` with optional no-buffering, fall back to `std::fs::copy`.
#[cfg(target_os = "windows")]
fn platform_copy_impl(src: &Path, dst: &Path, size_hint: u64) -> io::Result<CopyResult> {
    /// Threshold above which `COPY_FILE_NO_BUFFERING` is used (4MB).
    const NO_BUFFERING_THRESHOLD: u64 = 4 * 1024 * 1024;

    let use_no_buffering = size_hint > NO_BUFFERING_THRESHOLD;

    match try_copy_file_ex(src, dst, use_no_buffering) {
        Ok(bytes) => Ok(CopyResult::new(bytes, CopyMethod::CopyFileEx)),
        Err(_) => {
            // CopyFileExW failed, clean up and fall back
            let _ = std::fs::remove_file(dst);
            let bytes = std::fs::copy(src, dst)?;
            Ok(CopyResult::new(bytes, CopyMethod::StandardCopy))
        }
    }
}

/// Windows `CopyFileExW` FFI wrapper.
#[cfg(target_os = "windows")]
fn try_copy_file_ex(src: &Path, dst: &Path, use_no_buffering: bool) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;

    let src_wide: Vec<u16> = src
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let dst_wide: Vec<u16> = dst
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let flags: u32 = if use_no_buffering { 0x0000_0008 } else { 0 };

    // SAFETY: src_wide and dst_wide are null-terminated UTF-16 slices.
    // Progress callback, data, and cancel pointers are null (unused).
    let result = unsafe {
        windows_sys::Win32::Storage::FileSystem::CopyFileExW(
            src_wide.as_ptr(),
            dst_wide.as_ptr(),
            None,
            std::ptr::null(),
            std::ptr::null_mut(),
            flags,
        )
    };

    if result != 0 {
        let metadata = std::fs::metadata(dst)?;
        Ok(metadata.len())
    } else {
        Err(io::Error::last_os_error())
    }
}

// ===========================================================================
// Generic fallback
// ===========================================================================

/// Fallback for platforms without specialized copy optimizations.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn platform_copy_impl(src: &Path, dst: &Path, _size_hint: u64) -> io::Result<CopyResult> {
    let bytes = std::fs::copy(src, dst)?;
    Ok(CopyResult::new(bytes, CopyMethod::StandardCopy))
}

// ===========================================================================
// Non-macOS stubs for standalone functions
// ===========================================================================

#[cfg(not(target_os = "macos"))]
fn clonefile_impl(_src: &Path, _dst: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "clonefile is only available on macOS",
    ))
}

#[cfg(not(target_os = "macos"))]
fn fcopyfile_impl(_src: &Path, _dst: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "fcopyfile is only available on macOS",
    ))
}

// ===========================================================================
// Reflink support detection
// ===========================================================================

/// macOS supports reflink via `clonefile` on APFS.
#[cfg(target_os = "macos")]
fn platform_supports_reflink() -> bool {
    true
}

/// Linux does not yet expose reflink through this trait (FICLONE ioctl planned).
#[cfg(not(target_os = "macos"))]
fn platform_supports_reflink() -> bool {
    false
}

// ===========================================================================
// Preferred method selection
// ===========================================================================

/// Linux: prefer `copy_file_range` for files >= 64KB.
#[cfg(target_os = "linux")]
fn platform_preferred_method(size: u64) -> CopyMethod {
    if size >= 64 * 1024 {
        CopyMethod::CopyFileRange
    } else {
        CopyMethod::StandardCopy
    }
}

/// macOS: prefer `clonefile` regardless of size (instant CoW).
#[cfg(target_os = "macos")]
fn platform_preferred_method(_size: u64) -> CopyMethod {
    CopyMethod::Clonefile
}

/// Windows: prefer `CopyFileExW` with no-buffering for large files.
#[cfg(target_os = "windows")]
fn platform_preferred_method(size: u64) -> CopyMethod {
    if size > 4 * 1024 * 1024 {
        CopyMethod::CopyFileEx
    } else {
        CopyMethod::StandardCopy
    }
}

/// Other platforms: always standard copy.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn platform_preferred_method(_size: u64) -> CopyMethod {
    CopyMethod::StandardCopy
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn setup_source(dir: &Path, name: &str, content: &[u8]) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).expect("create source");
        file.write_all(content).expect("write source");
        path
    }

    fn setup_test_files(
        dir: &Path,
        name: &str,
        content: &[u8],
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let src = dir.join(format!("{name}_src.txt"));
        let dst = dir.join(format!("{name}_dst.txt"));
        std::fs::write(&src, content).expect("write source file");
        (src, dst)
    }

    // --- PlatformCopy trait tests ---

    #[test]
    fn copy_result_new_and_accessors() {
        let result = CopyResult::new(1024, CopyMethod::StandardCopy);
        assert_eq!(result.bytes_copied, 1024);
        assert_eq!(result.method, CopyMethod::StandardCopy);
        assert!(!result.is_zero_copy());
    }

    #[test]
    fn copy_result_is_zero_copy() {
        assert!(CopyResult::new(0, CopyMethod::CopyFileRange).is_zero_copy());
        assert!(CopyResult::new(0, CopyMethod::Clonefile).is_zero_copy());
        assert!(!CopyResult::new(0, CopyMethod::Copyfile).is_zero_copy());
        assert!(!CopyResult::new(0, CopyMethod::CopyFileEx).is_zero_copy());
        assert!(!CopyResult::new(0, CopyMethod::StandardCopy).is_zero_copy());
    }

    #[test]
    fn copy_method_display() {
        assert_eq!(CopyMethod::CopyFileRange.to_string(), "copy_file_range");
        assert_eq!(CopyMethod::Clonefile.to_string(), "clonefile");
        assert_eq!(CopyMethod::Copyfile.to_string(), "copyfile");
        assert_eq!(CopyMethod::CopyFileEx.to_string(), "CopyFileExW");
        assert_eq!(CopyMethod::StandardCopy.to_string(), "standard copy");
    }

    #[test]
    fn copy_method_equality_and_hash() {
        use std::collections::HashSet;

        let mut set = HashSet::new();
        set.insert(CopyMethod::CopyFileRange);
        set.insert(CopyMethod::Clonefile);
        set.insert(CopyMethod::StandardCopy);
        assert_eq!(set.len(), 3);
        assert!(set.contains(&CopyMethod::CopyFileRange));
    }

    #[test]
    fn default_platform_copy_small_file() {
        let temp = TempDir::new().expect("create temp dir");
        let content = b"Hello, platform copy!";
        let src = setup_source(temp.path(), "small_src.txt", content);
        let dst = temp.path().join("small_dst.txt");

        let copier = DefaultPlatformCopy::new();
        let result = copier
            .copy_file(&src, &dst, content.len() as u64)
            .expect("copy succeeds");

        let dst_content = std::fs::read(&dst).expect("read destination");
        assert_eq!(dst_content, content);
        assert!(result.bytes_copied > 0 || result.method == CopyMethod::Clonefile);
    }

    #[test]
    fn default_platform_copy_empty_file() {
        let temp = TempDir::new().expect("create temp dir");
        let src = setup_source(temp.path(), "empty_src.txt", b"");
        let dst = temp.path().join("empty_dst.txt");

        let copier = DefaultPlatformCopy::new();
        let result = copier.copy_file(&src, &dst, 0).expect("copy succeeds");

        let dst_content = std::fs::read(&dst).expect("read destination");
        assert_eq!(dst_content, b"");
        assert!(
            result.bytes_copied == 0,
            "empty file should copy 0 bytes, got {}",
            result.bytes_copied
        );
    }

    #[test]
    fn default_platform_copy_large_file() {
        let temp = TempDir::new().expect("create temp dir");
        let size = 256 * 1024; // 256KB - above copy_file_range threshold
        let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let src = setup_source(temp.path(), "large_src.bin", &content);
        let dst = temp.path().join("large_dst.bin");

        let copier = DefaultPlatformCopy::new();
        let result = copier
            .copy_file(&src, &dst, size as u64)
            .expect("copy succeeds");

        let dst_content = std::fs::read(&dst).expect("read destination");
        assert_eq!(dst_content, content);
        // On macOS with APFS, clonefile copies 0 bytes; otherwise expect full copy
        if result.method != CopyMethod::Clonefile {
            assert_eq!(result.bytes_copied, size as u64);
        }
    }

    #[test]
    fn default_platform_copy_preserves_binary_data() {
        let temp = TempDir::new().expect("create temp dir");
        // Binary content with all byte values
        let content: Vec<u8> = (0..=255).collect();
        let src = setup_source(temp.path(), "binary_src.bin", &content);
        let dst = temp.path().join("binary_dst.bin");

        let copier = DefaultPlatformCopy::new();
        copier
            .copy_file(&src, &dst, content.len() as u64)
            .expect("copy succeeds");

        let dst_content = std::fs::read(&dst).expect("read destination");
        assert_eq!(
            dst_content, content,
            "binary data must be preserved exactly"
        );
    }

    #[test]
    fn default_platform_copy_nonexistent_source() {
        let temp = TempDir::new().expect("create temp dir");
        let src = temp.path().join("nonexistent.txt");
        let dst = temp.path().join("dest.txt");

        let copier = DefaultPlatformCopy::new();
        let result = copier.copy_file(&src, &dst, 0);
        assert!(result.is_err(), "should error on missing source");
    }

    #[test]
    fn default_platform_copy_overwrites_destination() {
        let temp = TempDir::new().expect("create temp dir");
        let src = setup_source(temp.path(), "overwrite_src.txt", b"new content");
        let dst = temp.path().join("overwrite_dst.txt");
        std::fs::write(&dst, b"old content").expect("write old content");

        let copier = DefaultPlatformCopy::new();
        copier.copy_file(&src, &dst, 11).expect("copy succeeds");

        let dst_content = std::fs::read(&dst).expect("read destination");
        assert_eq!(dst_content, b"new content");
    }

    #[test]
    fn supports_reflink_platform_specific() {
        let copier = DefaultPlatformCopy::new();
        let supports = copier.supports_reflink();

        #[cfg(target_os = "macos")]
        assert!(supports, "macOS should report reflink support");

        #[cfg(not(target_os = "macos"))]
        assert!(!supports, "non-macOS should not report reflink support");
    }

    #[test]
    fn preferred_method_small_file() {
        let copier = DefaultPlatformCopy::new();
        let method = copier.preferred_method(100);

        #[cfg(target_os = "macos")]
        assert_eq!(method, CopyMethod::Clonefile);

        #[cfg(target_os = "linux")]
        assert_eq!(method, CopyMethod::StandardCopy);

        #[cfg(target_os = "windows")]
        assert_eq!(method, CopyMethod::StandardCopy);
    }

    #[test]
    fn preferred_method_large_file() {
        let copier = DefaultPlatformCopy::new();
        let method = copier.preferred_method(100 * 1024 * 1024); // 100MB

        #[cfg(target_os = "macos")]
        assert_eq!(method, CopyMethod::Clonefile);

        #[cfg(target_os = "linux")]
        assert_eq!(method, CopyMethod::CopyFileRange);

        #[cfg(target_os = "windows")]
        assert_eq!(method, CopyMethod::CopyFileEx);
    }

    #[test]
    fn trait_object_usage() {
        // Verify PlatformCopy works as a trait object (dyn dispatch)
        let copier: Box<dyn PlatformCopy> = Box::new(DefaultPlatformCopy::new());
        let _supports = copier.supports_reflink();
        let _preferred = copier.preferred_method(1024);
    }

    #[test]
    fn parity_default_vs_std_fs_copy() {
        let temp = TempDir::new().expect("create temp dir");

        let mut content = Vec::new();
        content.extend_from_slice(b"ASCII text\n");
        content.extend_from_slice(&[0x00, 0xFF, 0xAA, 0x55]);
        content.extend_from_slice("Unicode: \u{1F980}\u{1F99E}".as_bytes());

        let src = setup_source(temp.path(), "parity_src.txt", &content);

        // Path 1: PlatformCopy trait
        let dst1 = temp.path().join("parity_dst1.txt");
        let copier = DefaultPlatformCopy::new();
        copier
            .copy_file(&src, &dst1, content.len() as u64)
            .expect("platform copy succeeds");

        // Path 2: std::fs::copy
        let dst2 = temp.path().join("parity_dst2.txt");
        std::fs::copy(&src, &dst2).expect("std::fs::copy succeeds");

        let content1 = std::fs::read(&dst1).expect("read dst1");
        let content2 = std::fs::read(&dst2).expect("read dst2");

        assert_eq!(
            content1, content2,
            "PlatformCopy and std::fs::copy must produce identical output"
        );
        assert_eq!(content1, content, "both must match source");
    }

    // --- Standalone clonefile/fcopyfile tests ---

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn clonefile_returns_unsupported_on_non_macos() {
        let temp = TempDir::new().expect("create temp dir");
        let (src, dst) = setup_test_files(temp.path(), "clone_stub", b"data");

        let err = try_clonefile(&src, &dst).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn fcopyfile_returns_unsupported_on_non_macos() {
        let temp = TempDir::new().expect("create temp dir");
        let (src, dst) = setup_test_files(temp.path(), "fcopy_stub", b"data");

        let err = try_fcopyfile(&src, &dst).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn clonefile_copies_data() {
        let temp = TempDir::new().expect("create temp dir");
        let content = b"hello from clonefile";
        let (src, dst) = setup_test_files(temp.path(), "clone_data", content);

        // clonefile requires destination does not exist
        let _ = std::fs::remove_file(&dst);

        match try_clonefile(&src, &dst) {
            Ok(()) => {
                let result = std::fs::read(&dst).expect("read cloned file");
                assert_eq!(result, content);
            }
            Err(e) => {
                // APFS not available (e.g., HFS+ volume) - acceptable in test
                assert_ne!(
                    e.kind(),
                    io::ErrorKind::Unsupported,
                    "macOS should never return Unsupported"
                );
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn clonefile_fails_when_dst_exists() {
        let temp = TempDir::new().expect("create temp dir");
        let (src, dst) = setup_test_files(temp.path(), "clone_exists", b"data");

        // Create destination so clonefile fails
        std::fs::write(&dst, b"existing").expect("write dst");

        let result = try_clonefile(&src, &dst);
        assert!(result.is_err(), "clonefile should fail when dst exists");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn clonefile_fails_on_missing_source() {
        let temp = TempDir::new().expect("create temp dir");
        let src = temp.path().join("nonexistent.txt");
        let dst = temp.path().join("dst.txt");

        let result = try_clonefile(&src, &dst);
        assert!(result.is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn fcopyfile_copies_data() {
        let temp = TempDir::new().expect("create temp dir");
        let content = b"hello from fcopyfile";
        let (src, dst) = setup_test_files(temp.path(), "fcopy_data", content);

        try_fcopyfile(&src, &dst).expect("fcopyfile should succeed");

        let result = std::fs::read(&dst).expect("read copied file");
        assert_eq!(result, content);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn fcopyfile_overwrites_destination() {
        let temp = TempDir::new().expect("create temp dir");
        let content = b"new content";
        let (src, dst) = setup_test_files(temp.path(), "fcopy_overwrite", content);

        // Pre-populate destination
        std::fs::write(&dst, b"old content").expect("write old dst");

        try_fcopyfile(&src, &dst).expect("fcopyfile should succeed");

        let result = std::fs::read(&dst).expect("read copied file");
        assert_eq!(result, content);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn fcopyfile_fails_on_missing_source() {
        let temp = TempDir::new().expect("create temp dir");
        let src = temp.path().join("nonexistent.txt");
        let dst = temp.path().join("dst.txt");

        let result = try_fcopyfile(&src, &dst);
        assert!(result.is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn fcopyfile_copies_empty_file() {
        let temp = TempDir::new().expect("create temp dir");
        let (src, dst) = setup_test_files(temp.path(), "fcopy_empty", b"");

        try_fcopyfile(&src, &dst).expect("fcopyfile should succeed for empty file");

        let result = std::fs::read(&dst).expect("read copied file");
        assert!(result.is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn fcopyfile_copies_large_file() {
        let temp = TempDir::new().expect("create temp dir");
        let content = vec![0xAB_u8; 1024 * 1024]; // 1MB
        let src = temp.path().join("fcopy_large_src.bin");
        let dst = temp.path().join("fcopy_large_dst.bin");
        std::fs::write(&src, &content).expect("write large source");

        try_fcopyfile(&src, &dst).expect("fcopyfile should succeed for large file");

        let result = std::fs::read(&dst).expect("read large copied file");
        assert_eq!(result.len(), content.len());
        assert_eq!(result, content);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parity_fcopyfile_vs_std_copy() {
        let temp = TempDir::new().expect("create temp dir");

        let mut content = Vec::new();
        content.extend_from_slice(b"ASCII text\n");
        content.extend_from_slice(&[0x00, 0xFF, 0xAA, 0x55]);
        content.extend_from_slice("Unicode: \u{1F980}\u{1F99E}".as_bytes());

        let src = temp.path().join("parity_src.bin");
        std::fs::write(&src, &content).expect("write source");

        let dst_fcopy = temp.path().join("parity_fcopy.bin");
        try_fcopyfile(&src, &dst_fcopy).expect("fcopyfile should succeed");

        let dst_std = temp.path().join("parity_std.bin");
        std::fs::copy(&src, &dst_std).expect("std::fs::copy should succeed");

        let result_fcopy = std::fs::read(&dst_fcopy).expect("read fcopyfile result");
        let result_std = std::fs::read(&dst_std).expect("read std::fs::copy result");

        assert_eq!(
            result_fcopy, result_std,
            "fcopyfile and std::fs::copy must produce identical output"
        );
        assert_eq!(result_fcopy, content);
    }
}
