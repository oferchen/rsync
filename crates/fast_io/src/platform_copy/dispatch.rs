//! Platform-specific dispatch functions for file copy operations.
//!
//! Each function is gated with `#[cfg]` attributes to compile only on
//! the appropriate platform, with stubs on unsupported platforms.

use std::io;
use std::path::Path;

use super::types::{CopyMethod, CopyResult};

/// Linux: try `FICLONE` (instant CoW), then `copy_file_range`, then `std::fs::copy`.
///
/// The dispatch chain prioritizes zero-data-copy methods:
/// 1. `FICLONE` ioctl - instant reflink on Btrfs/XFS/bcachefs (same device only)
/// 2. `copy_file_range` - zero-copy in kernel space (files >= 64KB)
/// 3. `std::fs::copy` - portable buffered fallback
#[cfg(target_os = "linux")]
pub(super) fn platform_copy_impl(src: &Path, dst: &Path, size_hint: u64) -> io::Result<CopyResult> {
    use std::fs::File;

    // Try FICLONE first - instant CoW clone, O(1) regardless of file size.
    // Opens both files because FICLONE operates on file descriptors.
    // upstream: does not use FICLONE, this is an oc-rsync optimization.
    match try_ficlone_impl(src, dst) {
        Ok(()) => return Ok(CopyResult::new(0, CopyMethod::Ficlone)),
        Err(_) => {
            // FICLONE failed (unsupported fs, cross-device, etc.) - fall through
            let _ = std::fs::remove_file(dst);
        }
    }

    // Threshold below which copy_file_range overhead exceeds benefit
    // (matches copy_file_range module constant)
    const CFR_THRESHOLD: u64 = 64 * 1024;

    if size_hint >= CFR_THRESHOLD {
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

/// macOS: try `clonefile` (CoW), then `fcopyfile` (kernel-accelerated), then `std::fs::copy`.
///
/// The dispatch chain prioritizes zero-data-copy methods:
/// 1. `clonefile` - instant CoW on APFS (fails on cross-device, HFS+, etc.)
/// 2. `fcopyfile` - kernel-accelerated data copy via file descriptors
/// 3. `std::fs::copy` - portable buffered fallback
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

    // Try fcopyfile - kernel-accelerated data-only copy via file descriptors.
    // Faster than userspace buffered copy on all macOS filesystems.
    match fcopyfile_impl(src, dst) {
        Ok(()) => {
            let metadata = std::fs::metadata(dst)?;
            return Ok(CopyResult::new(metadata.len(), CopyMethod::Copyfile));
        }
        Err(_) => {
            // fcopyfile failed - clean up and fall through to standard copy
            let _ = std::fs::remove_file(dst);
        }
    }

    let bytes = std::fs::copy(src, dst)?;
    Ok(CopyResult::new(bytes, CopyMethod::StandardCopy))
}

/// Windows: check for ReFS reflink support, then try `CopyFileExW`, fall back to `std::fs::copy`.
///
/// On ReFS volumes, attempts `FSCTL_DUPLICATE_EXTENTS_TO_FILE` for instant
/// copy-on-write block cloning before falling back to `CopyFileExW`. The
/// reflink path is O(1) regardless of file size when both files reside on
/// the same ReFS volume.
#[cfg(target_os = "windows")]
pub(super) fn platform_copy_impl(src: &Path, dst: &Path, size_hint: u64) -> io::Result<CopyResult> {
    /// Threshold above which `COPY_FILE_NO_BUFFERING` is used (4MB).
    const NO_BUFFERING_THRESHOLD: u64 = 4 * 1024 * 1024;

    let is_refs =
        crate::refs_detect::is_refs_filesystem(dst.parent().unwrap_or(dst)).unwrap_or(false);

    if is_refs {
        match try_refs_reflink_impl(src, dst) {
            Ok(()) => return Ok(CopyResult::new(0, CopyMethod::ReFsReflink)),
            Err(_) => {
                // Reflink failed (cross-volume, alignment issue, etc.) - fall through
                let _ = std::fs::remove_file(dst);
            }
        }
    }

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
    let ret = unsafe { libc::fcopyfile(src_fd, dst_fd, std::ptr::null_mut(), libc::COPYFILE_DATA) };

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
pub(super) fn try_copy_file_ex(src: &Path, dst: &Path, use_no_buffering: bool) -> io::Result<u64> {
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

    /// `COPY_FILE_NO_BUFFERING` flag for `CopyFileExW` - bypasses system cache
    /// for large file copies, reducing memory pressure.
    const COPY_FILE_NO_BUFFERING: u32 = 0x0000_0008;

    let flags: u32 = if use_no_buffering {
        COPY_FILE_NO_BUFFERING
    } else {
        0
    };

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

/// Windows ReFS `FSCTL_DUPLICATE_EXTENTS_TO_FILE` reflink wrapper.
///
/// Creates a copy-on-write block clone on ReFS volumes. Both source and
/// destination must reside on the same ReFS volume. The ioctl requires
/// cluster-aligned offsets and byte count, so this function queries the
/// cluster size via `GetDiskFreeSpaceW` and rounds the file size up to
/// the nearest cluster boundary.
///
/// # Constraints
///
/// - Both files must be on the same ReFS volume.
/// - ReFS cluster size is queried at runtime (typically 4KB or 64KB).
/// - The destination file is created and pre-sized to match the source.
/// - On failure, the caller is responsible for cleaning up the destination.
///
/// # References
///
/// - upstream: ReFS block cloning is an oc-rsync optimization (no upstream equivalent).
/// - Microsoft docs: `FSCTL_DUPLICATE_EXTENTS_TO_FILE` control code.
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
pub(super) fn try_refs_reflink_impl(src: &Path, dst: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_ALWAYS, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, GetDiskFreeSpaceW,
        OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    /// Generic read access right. Defined locally because `windows-sys` 0.59
    /// removed these from `Win32::Storage::FileSystem`.
    const GENERIC_READ: u32 = 0x8000_0000;
    /// Generic write access right.
    const GENERIC_WRITE: u32 = 0x4000_0000;

    /// `FSCTL_DUPLICATE_EXTENTS_TO_FILE` control code.
    /// CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 0xD1, METHOD_BUFFERED, FILE_WRITE_ACCESS)
    const FSCTL_DUPLICATE_EXTENTS_TO_FILE: u32 = 0x0009_8344;

    /// Input structure for `FSCTL_DUPLICATE_EXTENTS_TO_FILE`.
    /// All offset and byte_count fields must be cluster-aligned.
    #[repr(C)]
    struct DuplicateExtentsData {
        file_handle: isize,
        source_file_offset: i64,
        target_file_offset: i64,
        byte_count: i64,
    }

    // Query cluster size for alignment via GetDiskFreeSpaceW on the volume root.
    let volume_root = dst.ancestors().last().unwrap_or(dst);
    let root_wide: Vec<u16> = volume_root
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut sectors_per_cluster: u32 = 0;
    let mut bytes_per_sector: u32 = 0;
    let mut free_clusters: u32 = 0;
    let mut total_clusters: u32 = 0;

    // SAFETY: root_wide is a valid null-terminated UTF-16 string.
    // Output pointers are valid stack-allocated u32 variables.
    let disk_result = unsafe {
        GetDiskFreeSpaceW(
            root_wide.as_ptr(),
            &mut sectors_per_cluster,
            &mut bytes_per_sector,
            &mut free_clusters,
            &mut total_clusters,
        )
    };

    if disk_result == 0 {
        return Err(io::Error::last_os_error());
    }

    let cluster_size = u64::from(sectors_per_cluster) * u64::from(bytes_per_sector);
    if cluster_size == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "cluster size is zero",
        ));
    }

    let file_size = std::fs::metadata(src)?.len();

    if file_size == 0 {
        // Empty file - just create the destination
        std::fs::File::create(dst)?;
        return Ok(());
    }

    // Round up to cluster boundary for the ioctl
    let aligned_size = ((file_size + cluster_size - 1) / cluster_size) * cluster_size;

    let src_wide: Vec<u16> = src
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // Open source for reading.
    // SAFETY: src_wide is a valid null-terminated UTF-16 path.
    // GENERIC_READ and FILE_SHARE_READ allow concurrent readers.
    // OPEN_EXISTING fails if the file does not exist.
    let src_handle = unsafe {
        CreateFileW(
            src_wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };

    if src_handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }

    let dst_wide: Vec<u16> = dst
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // Open/create destination for writing.
    // CREATE_ALWAYS (2) creates new or overwrites existing.
    // SAFETY: dst_wide is a valid null-terminated UTF-16 path.
    // GENERIC_READ | GENERIC_WRITE grants the access required by the ioctl.
    let dst_raw_handle = unsafe {
        CreateFileW(
            dst_wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0, // Exclusive access while setting up the clone
            std::ptr::null(),
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };

    if dst_raw_handle == INVALID_HANDLE_VALUE {
        let err = io::Error::last_os_error();
        // SAFETY: src_handle is a valid open handle.
        unsafe { CloseHandle(src_handle) };
        return Err(err);
    }

    // Wrap the destination handle in a File for RAII cleanup and set_len().
    // SAFETY: dst_raw_handle is a valid handle just returned by CreateFileW.
    // Ownership transfers to the File; it will call CloseHandle on drop.
    let dst_file = unsafe { std::fs::File::from_raw_handle(dst_raw_handle) };

    // Pre-size destination to the cluster-aligned size required by the ioctl.
    if let Err(e) = dst_file.set_len(aligned_size) {
        // SAFETY: src_handle is a valid open handle.
        unsafe { CloseHandle(src_handle) };
        return Err(e);
    }

    let dst_handle = dst_file.as_raw_handle();

    let dup_data = DuplicateExtentsData {
        file_handle: src_handle as isize,
        source_file_offset: 0,
        target_file_offset: 0,
        byte_count: aligned_size as i64,
    };

    let mut bytes_returned: u32 = 0;

    // SAFETY: dst_handle is a valid open file handle with read/write access.
    // src_handle (inside dup_data.file_handle) is a valid open handle with read access.
    // dup_data is a properly initialized DuplicateExtentsData struct with
    // cluster-aligned offsets. bytes_returned is a valid output pointer.
    let ioctl_result = unsafe {
        DeviceIoControl(
            dst_handle as HANDLE,
            FSCTL_DUPLICATE_EXTENTS_TO_FILE,
            &dup_data as *const DuplicateExtentsData as *const std::ffi::c_void,
            std::mem::size_of::<DuplicateExtentsData>() as u32,
            std::ptr::null_mut(),
            0,
            &mut bytes_returned,
            std::ptr::null_mut(),
        )
    };

    // Close source handle. dst_file will close dst_handle on drop.
    // SAFETY: src_handle is a valid open handle.
    unsafe { CloseHandle(src_handle) };

    if ioctl_result == 0 {
        let err = io::Error::last_os_error();
        drop(dst_file);
        return Err(err);
    }

    // Truncate destination to the actual file size (ioctl used cluster-aligned size).
    dst_file.set_len(file_size)?;

    Ok(())
}

/// Non-Windows stub for ReFS reflink.
#[cfg(not(target_os = "windows"))]
pub(super) fn try_refs_reflink_impl(_src: &Path, _dst: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "ReFS reflink is only available on Windows",
    ))
}

/// Linux `FICLONE` ioctl wrapper using `rustix::fs::ioctl_ficlone`.
///
/// Creates the destination file, then attempts a reflink clone from the source.
/// On success, source and destination share storage blocks (copy-on-write).
/// On failure, the caller is responsible for cleaning up the destination.
#[cfg(target_os = "linux")]
pub(super) fn try_ficlone_impl(src: &Path, dst: &Path) -> io::Result<()> {
    use std::fs::File;
    use std::os::fd::AsFd;

    let source = File::open(src)?;
    let destination = File::create(dst)?;

    // rustix::fs::ioctl_ficlone is fully safe - it uses AsFd/BorrowedFd
    // for compile-time fd validity, and wraps the FICLONE ioctl internally.
    rustix::fs::ioctl_ficlone(destination.as_fd(), source.as_fd())
        .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
}

/// Stub for non-Linux platforms where FICLONE is unavailable.
#[cfg(not(target_os = "linux"))]
pub(super) fn try_ficlone_impl(_src: &Path, _dst: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "FICLONE is only available on Linux",
    ))
}

/// macOS supports reflink via `clonefile` on APFS.
#[cfg(target_os = "macos")]
pub(super) fn platform_supports_reflink() -> bool {
    true
}

/// Linux supports reflink via `FICLONE` ioctl on Btrfs, XFS (reflink enabled),
/// and bcachefs. Returns `true` as a capability indicator - individual clone
/// operations may still fail on unsupported filesystems (ext4, tmpfs, NFS).
#[cfg(target_os = "linux")]
pub(super) fn platform_supports_reflink() -> bool {
    true
}

/// Windows supports reflink via `FSCTL_DUPLICATE_EXTENTS_TO_FILE` on ReFS only.
/// Returns `true` as a capability indicator - individual clone operations will
/// only be attempted when `is_refs_filesystem` returns true for the destination
/// path, and may still fail on NTFS or cross-volume scenarios.
#[cfg(target_os = "windows")]
pub(super) fn platform_supports_reflink() -> bool {
    true
}

/// Other platforms do not support reflink.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub(super) fn platform_supports_reflink() -> bool {
    false
}

/// Linux: prefer `FICLONE` (instant CoW) regardless of size.
///
/// FICLONE is O(1) and free when the filesystem supports it. The actual
/// dispatch falls back to `copy_file_range` then standard copy at runtime.
#[cfg(target_os = "linux")]
pub(super) fn platform_preferred_method(_size: u64) -> CopyMethod {
    CopyMethod::Ficlone
}

/// macOS: prefer `clonefile` regardless of size (instant CoW).
///
/// Runtime fallback chain: `clonefile` -> `fcopyfile` -> `std::fs::copy`.
#[cfg(target_os = "macos")]
pub(super) fn platform_preferred_method(_size: u64) -> CopyMethod {
    CopyMethod::Clonefile
}

/// Windows: prefer `CopyFileExW` with no-buffering for large files.
///
/// On ReFS volumes, the dispatch chain attempts `FSCTL_DUPLICATE_EXTENTS_TO_FILE`
/// first (instant CoW clone). This advisory method cannot determine the filesystem
/// type without a path, so it reports the non-reflink preference. The actual
/// method selection happens at runtime in `platform_copy_impl`.
#[cfg(target_os = "windows")]
pub(super) fn platform_preferred_method(size: u64) -> CopyMethod {
    /// Threshold above which `COPY_FILE_NO_BUFFERING` is used (4MB).
    const NO_BUFFERING_THRESHOLD: u64 = 4 * 1024 * 1024;

    if size > NO_BUFFERING_THRESHOLD {
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
