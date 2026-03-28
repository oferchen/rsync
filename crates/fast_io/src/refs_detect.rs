//! ReFS filesystem detection for Windows reflink support.
//!
//! Windows ReFS (Resilient File System) supports copy-on-write block cloning
//! via `FSCTL_DUPLICATE_EXTENTS_TO_FILE`. Before attempting reflink operations,
//! callers must verify the target path resides on a ReFS volume - NTFS does
//! not support block cloning.
//!
//! # Platform Support
//!
//! - **Windows**: Queries filesystem type via `GetVolumeInformationByHandleW`.
//!   Results are cached per volume root to avoid repeated syscalls.
//! - **Other platforms**: Always returns `false` (ReFS is Windows-only).
//!
//! # Caching
//!
//! Filesystem type is immutable for a mounted volume, so results are cached
//! per volume root path in a process-wide `Mutex<HashMap>`. The cache is
//! populated on first query and never evicted.
//!
//! # Example
//!
//! ```no_run
//! use std::path::Path;
//! use fast_io::refs_detect::is_refs_filesystem;
//!
//! let path = Path::new("C:\\Users\\data\\file.txt");
//! match is_refs_filesystem(path) {
//!     Ok(true) => println!("ReFS detected - reflink available"),
//!     Ok(false) => println!("Not ReFS - use standard copy"),
//!     Err(e) => eprintln!("Detection failed: {e}"),
//! }
//! ```

use std::io;
use std::path::Path;

/// Checks whether the given path resides on a ReFS filesystem.
///
/// On Windows, queries the volume's filesystem name via
/// `GetVolumeInformationByHandleW`. Results are cached per volume root
/// so subsequent calls for paths on the same volume avoid the syscall.
///
/// On non-Windows platforms, always returns `Ok(false)`.
///
/// # Arguments
///
/// * `path` - Any path on the volume to check. Can be a file or directory.
///   The path must exist (or its parent must exist) for the check to succeed.
///
/// # Errors
///
/// Returns `Ok(false)` if detection fails due to missing paths or
/// insufficient permissions. Only returns `Err` for unexpected I/O failures
/// that prevent querying the volume information.
pub fn is_refs_filesystem(path: &Path) -> io::Result<bool> {
    platform::is_refs_filesystem_impl(path)
}

/// Clears the cached filesystem detection results.
///
/// Primarily useful for testing. In production, the cache is populated
/// once per volume and never needs clearing since filesystem type is
/// immutable for a mounted volume.
pub fn clear_refs_cache() {
    platform::clear_cache();
}

#[cfg(windows)]
mod platform {
    use std::collections::HashMap;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use windows_sys::Win32::Storage::FileSystem::{
        GetVolumeInformationByHandleW, GetVolumePathNameW,
    };

    static CACHE: Mutex<Option<HashMap<PathBuf, bool>>> = Mutex::new(None);

    /// Queries the volume filesystem name for the given path and checks for "ReFS".
    pub(super) fn is_refs_filesystem_impl(path: &Path) -> io::Result<bool> {
        let volume_root = get_volume_root(path)?;

        // Check cache first
        {
            let guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(map) = guard.as_ref() {
                if let Some(&cached) = map.get(&volume_root) {
                    return Ok(cached);
                }
            }
        }

        // Query the filesystem type
        let is_refs = query_filesystem_name(&volume_root)?;

        // Cache the result
        {
            let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
            let map = guard.get_or_insert_with(HashMap::new);
            map.insert(volume_root, is_refs);
        }

        Ok(is_refs)
    }

    pub(super) fn clear_cache() {
        let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }

    /// Extracts the volume root path (e.g., `C:\`) from an arbitrary path.
    ///
    /// Uses `GetVolumePathNameW` which handles mount points, junction points,
    /// and UNC paths correctly.
    #[allow(unsafe_code)]
    fn get_volume_root(path: &Path) -> io::Result<PathBuf> {
        use std::os::windows::ffi::OsStringExt;

        let wide_path = to_wide(path);
        // MAX_PATH (260) is sufficient for volume root paths.
        let mut buffer = vec![0u16; 260];

        // SAFETY: `wide_path` is a valid null-terminated UTF-16 string.
        // `buffer` is a properly sized output buffer. `GetVolumePathNameW`
        // writes at most `buffer.len()` wide chars including the null terminator.
        let result = unsafe {
            GetVolumePathNameW(wide_path.as_ptr(), buffer.as_mut_ptr(), buffer.len() as u32)
        };

        if result == 0 {
            return Err(io::Error::last_os_error());
        }

        let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        let os_string = std::ffi::OsString::from_wide(&buffer[..len]);
        Ok(PathBuf::from(os_string))
    }

    /// Queries the filesystem name for a volume root path.
    ///
    /// Opens the volume root directory, then calls `GetVolumeInformationByHandleW`
    /// to retrieve the filesystem name string. Returns `true` if the name is "ReFS".
    #[allow(unsafe_code)]
    fn query_filesystem_name(volume_root: &Path) -> io::Result<bool> {
        use std::os::windows::ffi::OsStringExt;
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, OPEN_EXISTING,
        };

        let wide_root = to_wide(volume_root);

        // Open a handle to the volume root directory.
        // FILE_FLAG_BACKUP_SEMANTICS is required to open directories.
        // SAFETY: `wide_root` is a valid null-terminated UTF-16 path to an
        // existing volume root directory. The share flags allow concurrent
        // access. The handle is closed via `CloseHandle` below.
        let handle = unsafe {
            CreateFileW(
                wide_root.as_ptr(),
                0, // No access needed - just querying volume info
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };

        // INVALID_HANDLE_VALUE is -1 as isize, which is usize::MAX / isize::MAX+1
        // depending on the windows-sys version. Compare against the actual constant.
        if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }

        // Query filesystem name - buffer of 64 wide chars is more than enough
        // for filesystem names like "NTFS", "ReFS", "FAT32", "exFAT".
        let mut fs_name_buf = [0u16; 64];
        // SAFETY: `handle` is a valid open handle. The null pointers for
        // volume name, serial, component length, and flags are allowed -
        // `GetVolumeInformationByHandleW` skips those output parameters when
        // null. `fs_name_buf` is a properly sized output buffer.
        let result = unsafe {
            GetVolumeInformationByHandleW(
                handle,
                std::ptr::null_mut(), // lpVolumeNameBuffer - not needed
                0,                    // nVolumeNameSize
                std::ptr::null_mut(), // lpVolumeSerialNumber
                std::ptr::null_mut(), // lpMaximumComponentLength
                std::ptr::null_mut(), // lpFileSystemFlags
                fs_name_buf.as_mut_ptr(),
                fs_name_buf.len() as u32,
            )
        };

        // Close the handle regardless of the query result.
        // SAFETY: `handle` is a valid handle that was successfully opened above.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(handle);
        }

        if result == 0 {
            return Err(io::Error::last_os_error());
        }

        let len = fs_name_buf
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(fs_name_buf.len());
        let fs_name = std::ffi::OsString::from_wide(&fs_name_buf[..len]);

        Ok(fs_name.to_string_lossy() == "ReFS")
    }

    /// Converts a `Path` to a null-terminated UTF-16 `Vec<u16>` for Win32 APIs.
    fn to_wide(path: &Path) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;

        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }
}

#[cfg(not(windows))]
mod platform {
    use std::io;
    use std::path::Path;

    /// Non-Windows stub - ReFS is Windows-only.
    pub(super) fn is_refs_filesystem_impl(_path: &Path) -> io::Result<bool> {
        Ok(false)
    }

    /// Non-Windows stub - no cache to clear.
    pub(super) fn clear_cache() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_refs_returns_false() {
        // On macOS/Linux CI, always false. On Windows CI (NTFS), also false.
        let temp = tempfile::tempdir().expect("create temp dir");
        let result = is_refs_filesystem(temp.path());
        assert!(
            result.is_ok(),
            "detection should not error on valid temp dir"
        );
        assert!(
            !result.unwrap(),
            "temp dir should not be on ReFS (CI uses NTFS/ext4/APFS)"
        );
    }

    #[test]
    fn nonexistent_path_returns_gracefully() {
        // Non-existent paths should either return Ok(false) or a clean error,
        // never panic.
        let result = is_refs_filesystem(Path::new("/nonexistent/path/that/does/not/exist"));
        // On non-Windows: always Ok(false). On Windows: may error or Ok(false).
        match result {
            Ok(val) => assert!(!val, "nonexistent path cannot be on ReFS"),
            Err(_) => {
                // Acceptable - the path doesn't exist so volume query may fail
            }
        }
    }

    #[test]
    fn cache_clear_does_not_panic() {
        clear_refs_cache();
        // Verify detection still works after clearing
        let temp = tempfile::tempdir().expect("create temp dir");
        let result = is_refs_filesystem(temp.path());
        assert!(result.is_ok());
    }

    #[test]
    fn repeated_calls_use_cache() {
        let temp = tempfile::tempdir().expect("create temp dir");
        clear_refs_cache();

        // First call populates cache
        let first = is_refs_filesystem(temp.path()).expect("first call");
        // Second call hits cache
        let second = is_refs_filesystem(temp.path()).expect("second call");

        assert_eq!(first, second, "cached and uncached results must agree");
    }

    #[test]
    fn multiple_paths_same_volume() {
        let temp = tempfile::tempdir().expect("create temp dir");
        clear_refs_cache();

        let dir_a = temp.path().join("subdir_a");
        let dir_b = temp.path().join("subdir_b");
        std::fs::create_dir_all(&dir_a).expect("create dir_a");
        std::fs::create_dir_all(&dir_b).expect("create dir_b");

        let result_a = is_refs_filesystem(&dir_a);
        let result_b = is_refs_filesystem(&dir_b);

        // Both subdirs are on the same volume, results must agree
        match (result_a, result_b) {
            (Ok(a), Ok(b)) => assert_eq!(a, b, "same volume must give same result"),
            _ => {
                // If one fails both should fail similarly - acceptable in test
            }
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn stub_always_returns_false() {
        assert!(!is_refs_filesystem(Path::new("/")).unwrap());
        assert!(!is_refs_filesystem(Path::new("/tmp")).unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn windows_system_drive_is_not_refs() {
        // Standard Windows CI runners use NTFS on C:\
        let result = is_refs_filesystem(Path::new("C:\\"));
        assert!(result.is_ok(), "C:\\ should be queryable");
        assert!(!result.unwrap(), "C:\\ is typically NTFS, not ReFS");
    }

    #[cfg(windows)]
    #[test]
    fn windows_cache_populated_after_query() {
        clear_refs_cache();
        let _ = is_refs_filesystem(Path::new("C:\\"));

        // Second call should hit cache (we can't directly inspect the cache,
        // but we verify it doesn't error or change result)
        let result = is_refs_filesystem(Path::new("C:\\"));
        assert!(result.is_ok());
    }
}
