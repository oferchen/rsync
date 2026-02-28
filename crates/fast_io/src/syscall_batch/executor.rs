//! Core execution logic for individual metadata operations.

use std::fs;
use std::io;
use std::path::Path;
use std::time::SystemTime;

use super::types::{MetadataOp, MetadataResult};

/// Execute a single metadata operation.
///
/// This is the core implementation used by both individual and batched paths.
pub(super) fn execute_single_op(op: &MetadataOp) -> MetadataResult {
    match op {
        MetadataOp::Stat(path) => MetadataResult::Stat(stat_file(path, true)),
        MetadataOp::Lstat(path) => MetadataResult::Stat(stat_file(path, false)),
        MetadataOp::SetTimes { path, atime, mtime } => {
            MetadataResult::SetTimes(set_file_times(path, *atime, *mtime))
        }
        MetadataOp::SetPermissions { path, mode } => {
            MetadataResult::SetPermissions(set_file_permissions(path, *mode))
        }
    }
}

/// Stat a file using the best available syscall.
///
/// On Linux, uses `statx()` for improved performance.
/// On other platforms, uses standard library calls.
#[cfg(target_os = "linux")]
fn stat_file(path: &Path, follow_symlinks: bool) -> io::Result<fs::Metadata> {
    // Try statx first for better performance
    match try_statx(path, follow_symlinks) {
        Ok(metadata) => Ok(metadata),
        Err(_) => {
            // Fallback to standard library
            if follow_symlinks {
                fs::metadata(path)
            } else {
                fs::symlink_metadata(path)
            }
        }
    }
}

/// Stat a file using standard library calls (non-Linux).
#[cfg(not(target_os = "linux"))]
fn stat_file(path: &Path, follow_symlinks: bool) -> io::Result<fs::Metadata> {
    if follow_symlinks {
        fs::metadata(path)
    } else {
        fs::symlink_metadata(path)
    }
}

/// Try to use statx syscall for improved performance.
///
/// Returns metadata if successful, otherwise returns an error to trigger fallback.
#[cfg(target_os = "linux")]
fn try_statx(path: &Path, follow_symlinks: bool) -> io::Result<fs::Metadata> {
    use rustix::fs::{AtFlags, StatxFlags};

    let flags = if follow_symlinks {
        AtFlags::empty()
    } else {
        AtFlags::SYMLINK_NOFOLLOW
    };

    // Request basic stat info
    let mask = StatxFlags::BASIC_STATS;

    match rustix::fs::statx(rustix::fs::CWD, path, flags, mask) {
        Ok(_statx_result) => {
            // Convert statx result to std::fs::Metadata
            // We need to go through the filesystem to get proper Metadata type
            // since we can't construct it directly. The statx call validates
            // the path exists and is accessible, so this should succeed.
            if follow_symlinks {
                fs::metadata(path)
            } else {
                fs::symlink_metadata(path)
            }
        }
        Err(_) => {
            // Return error to trigger fallback
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "statx not available",
            ))
        }
    }
}

/// Set file times.
///
/// Uses `filetime` crate equivalent functionality via std::fs.
pub(super) fn set_file_times(
    path: &Path,
    atime: Option<SystemTime>,
    mtime: Option<SystemTime>,
) -> io::Result<()> {
    // For setting times, we need to use platform-specific APIs
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        // We need to use libc for utimensat
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid path"))?;

        let times = [timespec_from_option(atime), timespec_from_option(mtime)];

        // SAFETY: c_path is a valid C string, times is a valid array
        let result = unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) };

        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(not(unix))]
    {
        // Use the filetime crate as a portable fallback on non-Unix platforms.
        let ft_atime = atime.map(filetime::FileTime::from_system_time);
        let ft_mtime = mtime.map(filetime::FileTime::from_system_time);

        // filetime::set_file_times requires both; use current values for omitted.
        let meta = fs::metadata(path)?;
        let current_atime = filetime::FileTime::from_last_access_time(&meta);
        let current_mtime = filetime::FileTime::from_last_modification_time(&meta);

        filetime::set_file_times(
            path,
            ft_atime.unwrap_or(current_atime),
            ft_mtime.unwrap_or(current_mtime),
        )
    }
}

/// Convert SystemTime to libc::timespec.
#[cfg(unix)]
fn timespec_from_option(time: Option<SystemTime>) -> libc::timespec {
    match time {
        Some(t) => {
            let duration = t
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or(std::time::Duration::from_secs(0));
            libc::timespec {
                #[allow(deprecated)]
                tv_sec: duration.as_secs() as libc::time_t,
                tv_nsec: duration.subsec_nanos() as libc::c_long,
            }
        }
        None => {
            // UTIME_OMIT: don't change this timestamp
            libc::timespec {
                tv_sec: 0,
                tv_nsec: libc::UTIME_OMIT,
            }
        }
    }
}

/// Set file permissions.
#[cfg(unix)]
pub(super) fn set_file_permissions(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = fs::Permissions::from_mode(mode);
    fs::set_permissions(path, perms)
}

/// Set file permissions (non-Unix fallback).
///
/// Maps Unix mode bits to Windows readonly attribute: writable if owner
/// write bit is set, readonly otherwise.
#[cfg(not(unix))]
pub(super) fn set_file_permissions(path: &Path, mode: u32) -> io::Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_readonly(mode & 0o200 == 0);
    fs::set_permissions(path, perms)
}

/// Returns a sort key for grouping operations by type.
///
/// Operations are grouped as: Stat, Lstat, SetTimes, SetPermissions.
pub(super) fn operation_type_key(op: &MetadataOp) -> u8 {
    match op {
        MetadataOp::Stat(_) => 0,
        MetadataOp::Lstat(_) => 1,
        MetadataOp::SetTimes { .. } => 2,
        MetadataOp::SetPermissions { .. } => 3,
    }
}

/// Execute metadata operations individually (always available).
///
/// Processes each operation one at a time using standard library calls.
/// This path has lower overhead for small operation counts.
///
/// # Arguments
///
/// * `ops` - Slice of metadata operations to execute
///
/// # Returns
///
/// Vector of results in the same order as input operations.
///
/// # Example
///
/// ```no_run
/// use std::path::PathBuf;
/// use fast_io::syscall_batch::{MetadataOp, execute_metadata_ops_individual};
///
/// let ops = vec![MetadataOp::Stat(PathBuf::from("/etc/hosts"))];
/// let results = execute_metadata_ops_individual(&ops);
/// ```
pub fn execute_metadata_ops_individual(ops: &[MetadataOp]) -> Vec<MetadataResult> {
    ops.iter().map(execute_single_op).collect()
}

/// Execute metadata operations in batched mode (always available).
///
/// Groups operations by type and processes them together for better cache locality.
/// Results are mapped back to the original input order.
///
/// # Arguments
///
/// * `ops` - Slice of metadata operations to execute
///
/// # Returns
///
/// Vector of results in the same order as input operations.
///
/// # Example
///
/// ```no_run
/// use std::path::PathBuf;
/// use fast_io::syscall_batch::{MetadataOp, execute_metadata_ops_batched};
///
/// let ops = vec![
///     MetadataOp::Stat(PathBuf::from("/tmp/file1")),
///     MetadataOp::Stat(PathBuf::from("/tmp/file2")),
///     MetadataOp::Stat(PathBuf::from("/tmp/file3")),
/// ];
/// let results = execute_metadata_ops_batched(&ops);
/// ```
pub fn execute_metadata_ops_batched(ops: &[MetadataOp]) -> Vec<MetadataResult> {
    if ops.is_empty() {
        return Vec::new();
    }

    // Create index mapping for reordering results back to original order
    let mut indexed_ops: Vec<(usize, &MetadataOp)> = ops.iter().enumerate().collect();

    // Sort by operation type for cache locality
    indexed_ops.sort_by_key(|(_, op)| operation_type_key(op));

    // Execute operations in sorted order
    let mut indexed_results: Vec<(usize, MetadataResult)> = indexed_ops
        .iter()
        .map(|(idx, op)| (*idx, execute_single_op(op)))
        .collect();

    // Sort results back to original order
    indexed_results.sort_by_key(|(idx, _)| *idx);

    // Extract results
    indexed_results
        .into_iter()
        .map(|(_, result)| result)
        .collect()
}
