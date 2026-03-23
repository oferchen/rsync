//! Platform-specific dispatch functions for file copy operations.
//!
//! Each function is gated with `#[cfg]` attributes to compile only on
//! the appropriate platform, with stubs on unsupported platforms.

use std::io;
use std::path::Path;

use super::types::{CopyMethod, CopyResult};

/// Linux: try `copy_file_range` for large files, fall back to `std::fs::copy`.
#[cfg(target_os = "linux")]
pub(super) fn platform_copy_impl(
    src: &Path,
    dst: &Path,
    size_hint: u64,
) -> io::Result<CopyResult> {
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

/// macOS: try `clonefile` (CoW) first, then fall back to `std::fs::copy`.
///
/// On APFS, `clonefile` creates an instant copy-on-write clone sharing storage
/// blocks until modified. Falls back to `std::fs::copy` which uses `copyfile()`
/// under the hood on Darwin, handling metadata and resource forks natively.
#[cfg(target_os = "macos")]
pub(super) fn platform_copy_impl(
    src: &Path,
    dst: &Path,
    _size_hint: u64,
) -> io::Result<CopyResult> {
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

/// Windows: try `CopyFileExW` with optional no-buffering, fall back to `std::fs::copy`.
#[cfg(target_os = "windows")]
pub(super) fn platform_copy_impl(
    src: &Path,
    dst: &Path,
    size_hint: u64,
) -> io::Result<CopyResult> {
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

/// Fallback for platforms without specialized copy optimizations.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub(super) fn platform_copy_impl(
    src: &Path,
    dst: &Path,
    _size_hint: u64,
) -> io::Result<CopyResult> {
    let bytes = std::fs::copy(src, dst)?;
    Ok(CopyResult::new(bytes, CopyMethod::StandardCopy))
}

/// macOS `clonefile` FFI wrapper.
///
/// Used by both `platform_copy_impl` and the standalone `try_clonefile` function.
/// Isolated from the engine's `clonefile` module to avoid circular dependencies.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub(super) fn clonefile_impl(src: &Path, dst: &Path) -> io::Result<()> {
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
pub(super) fn fcopyfile_impl(src: &Path, dst: &Path) -> io::Result<()> {
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
    let ret = unsafe {
        libc::fcopyfile(src_fd, dst_fd, std::ptr::null_mut(), libc::COPYFILE_DATA)
    };

    if ret == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "macos"))]
pub(super) fn clonefile_impl(_src: &Path, _dst: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "clonefile is only available on macOS",
    ))
}

#[cfg(not(target_os = "macos"))]
pub(super) fn fcopyfile_impl(_src: &Path, _dst: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "fcopyfile is only available on macOS",
    ))
}

/// Windows `CopyFileExW` FFI wrapper.
#[cfg(target_os = "windows")]
pub(super) fn try_copy_file_ex(
    src: &Path,
    dst: &Path,
    use_no_buffering: bool,
) -> io::Result<u64> {
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

/// macOS supports reflink via `clonefile` on APFS.
#[cfg(target_os = "macos")]
pub(super) fn platform_supports_reflink() -> bool {
    true
}

/// Linux does not yet expose reflink through this trait (FICLONE ioctl planned).
#[cfg(not(target_os = "macos"))]
pub(super) fn platform_supports_reflink() -> bool {
    false
}

/// Linux: prefer `copy_file_range` for files >= 64KB.
#[cfg(target_os = "linux")]
pub(super) fn platform_preferred_method(size: u64) -> CopyMethod {
    if size >= 64 * 1024 {
        CopyMethod::CopyFileRange
    } else {
        CopyMethod::StandardCopy
    }
}

/// macOS: prefer `clonefile` regardless of size (instant CoW).
#[cfg(target_os = "macos")]
pub(super) fn platform_preferred_method(_size: u64) -> CopyMethod {
    CopyMethod::Clonefile
}

/// Windows: prefer `CopyFileExW` with no-buffering for large files.
#[cfg(target_os = "windows")]
pub(super) fn platform_preferred_method(size: u64) -> CopyMethod {
    if size > 4 * 1024 * 1024 {
        CopyMethod::CopyFileEx
    } else {
        CopyMethod::StandardCopy
    }
}

/// Other platforms: always standard copy.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub(super) fn platform_preferred_method(_size: u64) -> CopyMethod {
    CopyMethod::StandardCopy
}
