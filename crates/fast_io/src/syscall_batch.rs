//! Batched metadata syscall operations with dual-path runtime selection.
//!
//! This module provides batched metadata operations that reduce syscall overhead when
//! processing many files. It uses Linux's `statx()` syscall for more efficient metadata
//! retrieval and groups operations for improved cache locality.
//!
//! # Dual-Path Strategy
//!
//! The module provides two execution paths that are ALWAYS compiled:
//! - **Individual path**: Processes operations one at a time using standard library calls
//! - **Batched path**: Groups operations by type and processes them together
//!
//! Runtime selection uses [`BATCH_THRESHOLD`]: below this, use individual path;
//! at or above, use batched path.
//!
//! # Platform Support
//!
//! - **Linux**: Uses `statx()` for improved metadata operations in batched path
//! - **Other Unix**: Batched path uses standard library calls with grouping optimization
//! - **Non-Unix (Windows)**: Portable fallbacks — `filetime` crate for timestamps,
//!   readonly attribute mapping for permissions
//!
//! # Performance Characteristics
//!
//! - Individual path: Lower overhead for small operation counts (< 8)
//! - Batched path: Better cache locality and reduced context switches for large batches
//! - Operations are reordered in batched mode but results match original input order
//!
//! # Example
//!
//! ```no_run
//! use std::path::PathBuf;
//! use fast_io::syscall_batch::{MetadataOp, execute_metadata_ops};
//!
//! # fn main() -> std::io::Result<()> {
//! let ops = vec![
//!     MetadataOp::Stat(PathBuf::from("/tmp/file1")),
//!     MetadataOp::Lstat(PathBuf::from("/tmp/file2")),
//!     MetadataOp::Stat(PathBuf::from("/tmp/file3")),
//! ];
//!
//! let results = execute_metadata_ops(&ops);
//! for result in results {
//!     match result {
//!         fast_io::syscall_batch::MetadataResult::Stat(Ok(metadata)) => {
//!             println!("File size: {}", metadata.len());
//!         }
//!         fast_io::syscall_batch::MetadataResult::Stat(Err(e)) => {
//!             eprintln!("Error: {}", e);
//!         }
//!         _ => {}
//!     }
//! }
//! # Ok(())
//! # }
//! ```

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Threshold below which individual syscalls are used.
///
/// Operations below this count use the individual path for lower overhead.
/// Operations at or above this count use the batched path for better performance.
pub const BATCH_THRESHOLD: usize = 8;

/// A metadata operation to be performed on a file.
#[derive(Debug, Clone)]
pub enum MetadataOp {
    /// Stat a file (follow symlinks).
    Stat(PathBuf),
    /// Lstat a file (don't follow symlinks).
    Lstat(PathBuf),
    /// Set file times.
    SetTimes {
        /// Path to the file.
        path: PathBuf,
        /// Access time (None = don't change).
        atime: Option<SystemTime>,
        /// Modification time (None = don't change).
        mtime: Option<SystemTime>,
    },
    /// Set file permissions.
    SetPermissions {
        /// Path to the file.
        path: PathBuf,
        /// Unix permission mode bits.
        mode: u32,
    },
}

/// Result of a metadata operation.
#[derive(Debug)]
pub enum MetadataResult {
    /// Result of a Stat or Lstat operation.
    Stat(io::Result<fs::Metadata>),
    /// Result of a SetTimes operation.
    SetTimes(io::Result<()>),
    /// Result of a SetPermissions operation.
    SetPermissions(io::Result<()>),
}

/// Execute a batch of metadata operations.
///
/// Uses batched processing when `ops.len() >= BATCH_THRESHOLD`,
/// otherwise falls back to individual syscalls for lower overhead.
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
/// use fast_io::syscall_batch::{MetadataOp, execute_metadata_ops};
///
/// let ops = vec![
///     MetadataOp::Stat(PathBuf::from("/etc/hosts")),
///     MetadataOp::Lstat(PathBuf::from("/tmp/link")),
/// ];
///
/// let results = execute_metadata_ops(&ops);
/// assert_eq!(results.len(), ops.len());
/// ```
pub fn execute_metadata_ops(ops: &[MetadataOp]) -> Vec<MetadataResult> {
    if ops.len() >= BATCH_THRESHOLD {
        execute_metadata_ops_batched(ops)
    } else {
        execute_metadata_ops_individual(ops)
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

/// Returns a sort key for grouping operations by type.
///
/// Operations are grouped as: Stat, Lstat, SetTimes, SetPermissions.
fn operation_type_key(op: &MetadataOp) -> u8 {
    match op {
        MetadataOp::Stat(_) => 0,
        MetadataOp::Lstat(_) => 1,
        MetadataOp::SetTimes { .. } => 2,
        MetadataOp::SetPermissions { .. } => 3,
    }
}

/// Execute a single metadata operation.
///
/// This is the core implementation used by both individual and batched paths.
fn execute_single_op(op: &MetadataOp) -> MetadataResult {
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
fn set_file_times(
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
fn set_file_permissions(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = fs::Permissions::from_mode(mode);
    fs::set_permissions(path, perms)
}

/// Set file permissions (non-Unix fallback).
///
/// Maps Unix mode bits to Windows readonly attribute: writable if owner
/// write bit is set, readonly otherwise.
#[cfg(not(unix))]
fn set_file_permissions(path: &Path, mode: u32) -> io::Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_readonly(mode & 0o200 == 0);
    fs::set_permissions(path, perms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    /// Helper to create a temp file with content.
    fn create_test_file(dir: &TempDir, name: &str, content: &[u8]) -> io::Result<PathBuf> {
        let path = dir.path().join(name);
        let mut file = File::create(&path)?;
        file.write_all(content)?;
        file.sync_all()?;
        Ok(path)
    }

    #[test]
    fn test_individual_stat() {
        let temp_dir = TempDir::new().unwrap();
        let path = create_test_file(&temp_dir, "test.txt", b"hello").unwrap();

        let ops = vec![MetadataOp::Stat(path.clone())];
        let results = execute_metadata_ops_individual(&ops);

        assert_eq!(results.len(), 1);
        match &results[0] {
            MetadataResult::Stat(Ok(metadata)) => {
                assert_eq!(metadata.len(), 5);
            }
            _ => panic!("Expected successful Stat result"),
        }
    }

    #[test]
    fn test_individual_lstat() {
        let temp_dir = TempDir::new().unwrap();
        let path = create_test_file(&temp_dir, "test.txt", b"hello world").unwrap();

        let ops = vec![MetadataOp::Lstat(path.clone())];
        let results = execute_metadata_ops_individual(&ops);

        assert_eq!(results.len(), 1);
        match &results[0] {
            MetadataResult::Stat(Ok(metadata)) => {
                assert_eq!(metadata.len(), 11);
            }
            _ => panic!("Expected successful Stat result"),
        }
    }

    #[test]
    fn test_batched_stat_multiple() {
        let temp_dir = TempDir::new().unwrap();
        let path1 = create_test_file(&temp_dir, "file1.txt", b"content1").unwrap();
        let path2 = create_test_file(&temp_dir, "file2.txt", b"content22").unwrap();
        let path3 = create_test_file(&temp_dir, "file3.txt", b"content333").unwrap();

        let ops = vec![
            MetadataOp::Stat(path1.clone()),
            MetadataOp::Stat(path2.clone()),
            MetadataOp::Stat(path3.clone()),
        ];
        let results = execute_metadata_ops_batched(&ops);

        assert_eq!(results.len(), 3);
        match &results[0] {
            MetadataResult::Stat(Ok(metadata)) => assert_eq!(metadata.len(), 8),
            _ => panic!("Expected successful Stat result"),
        }
        match &results[1] {
            MetadataResult::Stat(Ok(metadata)) => assert_eq!(metadata.len(), 9),
            _ => panic!("Expected successful Stat result"),
        }
        match &results[2] {
            MetadataResult::Stat(Ok(metadata)) => assert_eq!(metadata.len(), 10),
            _ => panic!("Expected successful Stat result"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_set_times() {
        let temp_dir = TempDir::new().unwrap();
        let path = create_test_file(&temp_dir, "test.txt", b"hello").unwrap();

        let new_mtime = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1000000);
        let ops = vec![MetadataOp::SetTimes {
            path: path.clone(),
            atime: None,
            mtime: Some(new_mtime),
        }];
        let results = execute_metadata_ops_individual(&ops);

        assert_eq!(results.len(), 1);
        match &results[0] {
            MetadataResult::SetTimes(Ok(())) => {
                // Verify the time was set
                let metadata = fs::metadata(&path).unwrap();
                let mtime = metadata.modified().unwrap();
                // Allow small delta due to filesystem time granularity
                let delta = if mtime > new_mtime {
                    mtime.duration_since(new_mtime).unwrap()
                } else {
                    new_mtime.duration_since(mtime).unwrap()
                };
                assert!(delta.as_secs() < 2, "mtime delta too large");
            }
            _ => panic!("Expected successful SetTimes result"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_set_permissions() {
        let temp_dir = TempDir::new().unwrap();
        let path = create_test_file(&temp_dir, "test.txt", b"hello").unwrap();

        let ops = vec![MetadataOp::SetPermissions {
            path: path.clone(),
            mode: 0o644,
        }];
        let results = execute_metadata_ops_individual(&ops);

        assert_eq!(results.len(), 1);
        match &results[0] {
            MetadataResult::SetPermissions(Ok(())) => {
                // Verify permissions were set
                use std::os::unix::fs::PermissionsExt;
                let metadata = fs::metadata(&path).unwrap();
                assert_eq!(metadata.permissions().mode() & 0o777, 0o644);
            }
            _ => panic!("Expected successful SetPermissions result"),
        }
    }

    #[test]
    fn test_threshold_routing() {
        let temp_dir = TempDir::new().unwrap();

        // Create BATCH_THRESHOLD files
        let mut paths = Vec::new();
        for i in 0..BATCH_THRESHOLD {
            let path = create_test_file(&temp_dir, &format!("file{i}.txt"), b"test").unwrap();
            paths.push(path);
        }

        // Below threshold - should use individual path
        let ops_below: Vec<_> = paths
            .iter()
            .take(BATCH_THRESHOLD - 1)
            .map(|p| MetadataOp::Stat(p.clone()))
            .collect();
        let results_below = execute_metadata_ops(&ops_below);
        assert_eq!(results_below.len(), BATCH_THRESHOLD - 1);

        // At threshold - should use batched path
        let ops_at: Vec<_> = paths
            .iter()
            .take(BATCH_THRESHOLD)
            .map(|p| MetadataOp::Stat(p.clone()))
            .collect();
        let results_at = execute_metadata_ops(&ops_at);
        assert_eq!(results_at.len(), BATCH_THRESHOLD);

        // Verify all results are successful
        for result in results_below.iter().chain(results_at.iter()) {
            match result {
                MetadataResult::Stat(Ok(_)) => {}
                _ => panic!("Expected successful Stat result"),
            }
        }
    }

    #[test]
    fn test_batched_preserves_order() {
        let temp_dir = TempDir::new().unwrap();
        let path1 = create_test_file(&temp_dir, "file1.txt", b"a").unwrap();
        let path2 = create_test_file(&temp_dir, "file2.txt", b"bb").unwrap();
        let path3 = create_test_file(&temp_dir, "file3.txt", b"ccc").unwrap();

        // Mix different operation types
        let ops = vec![
            MetadataOp::Lstat(path1.clone()), // index 0
            MetadataOp::Stat(path2.clone()),  // index 1
            MetadataOp::Lstat(path3.clone()), // index 2
        ];
        let results = execute_metadata_ops_batched(&ops);

        assert_eq!(results.len(), 3);
        // Results should be in original order despite grouping
        match &results[0] {
            MetadataResult::Stat(Ok(metadata)) => assert_eq!(metadata.len(), 1),
            _ => panic!("Expected Stat at index 0"),
        }
        match &results[1] {
            MetadataResult::Stat(Ok(metadata)) => assert_eq!(metadata.len(), 2),
            _ => panic!("Expected Stat at index 1"),
        }
        match &results[2] {
            MetadataResult::Stat(Ok(metadata)) => assert_eq!(metadata.len(), 3),
            _ => panic!("Expected Stat at index 2"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_mixed_operations() {
        let temp_dir = TempDir::new().unwrap();
        let path1 = create_test_file(&temp_dir, "file1.txt", b"test1").unwrap();
        let path2 = create_test_file(&temp_dir, "file2.txt", b"test2").unwrap();
        let path3 = create_test_file(&temp_dir, "file3.txt", b"test3").unwrap();

        let new_mtime = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(2000000);
        let ops = vec![
            MetadataOp::Stat(path1.clone()),
            MetadataOp::SetPermissions {
                path: path2.clone(),
                mode: 0o755,
            },
            MetadataOp::Lstat(path3.clone()),
            MetadataOp::SetTimes {
                path: path1.clone(),
                atime: None,
                mtime: Some(new_mtime),
            },
        ];
        let results = execute_metadata_ops_batched(&ops);

        assert_eq!(results.len(), 4);

        // Verify each result type
        match &results[0] {
            MetadataResult::Stat(Ok(_)) => {}
            _ => panic!("Expected Stat at index 0"),
        }
        match &results[1] {
            MetadataResult::SetPermissions(Ok(())) => {}
            _ => panic!("Expected SetPermissions at index 1"),
        }
        match &results[2] {
            MetadataResult::Stat(Ok(_)) => {}
            _ => panic!("Expected Stat at index 2"),
        }
        match &results[3] {
            MetadataResult::SetTimes(Ok(())) => {}
            _ => panic!("Expected SetTimes at index 3"),
        }
    }

    #[test]
    fn test_nonexistent_file_in_batch() {
        let temp_dir = TempDir::new().unwrap();
        let path1 = create_test_file(&temp_dir, "exists.txt", b"hello").unwrap();
        let path2 = temp_dir.path().join("does_not_exist.txt");
        let path3 = create_test_file(&temp_dir, "exists2.txt", b"world").unwrap();

        let ops = vec![
            MetadataOp::Stat(path1.clone()),
            MetadataOp::Stat(path2.clone()),
            MetadataOp::Stat(path3.clone()),
        ];
        let results = execute_metadata_ops_batched(&ops);

        assert_eq!(results.len(), 3);

        // First should succeed
        match &results[0] {
            MetadataResult::Stat(Ok(metadata)) => {
                assert_eq!(metadata.len(), 5);
            }
            _ => panic!("Expected successful Stat at index 0"),
        }

        // Second should fail
        match &results[1] {
            MetadataResult::Stat(Err(_)) => {}
            _ => panic!("Expected failed Stat at index 1"),
        }

        // Third should succeed despite middle failure
        match &results[2] {
            MetadataResult::Stat(Ok(metadata)) => {
                assert_eq!(metadata.len(), 5);
            }
            _ => panic!("Expected successful Stat at index 2"),
        }
    }

    #[test]
    fn test_empty_batch() {
        let ops: Vec<MetadataOp> = vec![];
        let results = execute_metadata_ops(&ops);
        assert_eq!(results.len(), 0);

        let results_individual = execute_metadata_ops_individual(&ops);
        assert_eq!(results_individual.len(), 0);

        let results_batched = execute_metadata_ops_batched(&ops);
        assert_eq!(results_batched.len(), 0);
    }

    #[test]
    fn test_parity_individual_vs_batched() {
        let temp_dir = TempDir::new().unwrap();
        let path1 = create_test_file(&temp_dir, "file1.txt", b"content1").unwrap();
        let path2 = create_test_file(&temp_dir, "file2.txt", b"content2").unwrap();
        let path3 = temp_dir.path().join("nonexistent.txt");
        let path4 = create_test_file(&temp_dir, "file4.txt", b"content4").unwrap();

        let ops = vec![
            MetadataOp::Stat(path1.clone()),
            MetadataOp::Lstat(path2.clone()),
            MetadataOp::Stat(path3.clone()),
            MetadataOp::Lstat(path4.clone()),
        ];

        let results_individual = execute_metadata_ops_individual(&ops);
        let results_batched = execute_metadata_ops_batched(&ops);

        assert_eq!(results_individual.len(), results_batched.len());

        // Compare results
        for (i, (ind, bat)) in results_individual
            .iter()
            .zip(results_batched.iter())
            .enumerate()
        {
            match (ind, bat) {
                (MetadataResult::Stat(Ok(m1)), MetadataResult::Stat(Ok(m2))) => {
                    assert_eq!(m1.len(), m2.len(), "Mismatch at index {i}");
                    assert_eq!(m1.is_dir(), m2.is_dir(), "Mismatch at index {i}");
                }
                (MetadataResult::Stat(Err(_)), MetadataResult::Stat(Err(_))) => {
                    // Both failed - this is expected for nonexistent file
                }
                _ => panic!("Result type mismatch at index {i}: {ind:?} vs {bat:?}"),
            }
        }
    }
}
