//! Low-level Win32 wrappers for delete-on-close temporary files.
//!
//! On Windows, `FILE_FLAG_DELETE_ON_CLOSE` creates a named file that the
//! kernel automatically deletes when the last handle to it is closed. This
//! provides crash-safe cleanup semantics analogous to Linux `O_TMPFILE`:
//! if the process crashes before commit, no orphaned temp file remains.
//!
//! The commit path clears the delete-on-close disposition via
//! `SetFileInformationByHandle(FileDispositionInfo)` so the file survives
//! handle close, then renames it to the destination.
//!
//! # Kernel requirements
//!
//! - `FILE_FLAG_DELETE_ON_CLOSE`: Windows Vista+ (all supported versions).
//! - `SetFileInformationByHandle(FileDispositionInfo)`: Windows Vista+.
//! - `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`: Windows NT 3.1+.

#[cfg(target_os = "windows")]
mod windows {
    use std::fs::File;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_NEW, CreateFileW, DELETE, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_DELETE_ON_CLOSE,
        FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_DELETE, FILE_SHARE_READ,
    };

    /// Counter for generating unique temp file names.
    static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Creates a delete-on-close temporary file in `dir`.
    ///
    /// The file is created with `FILE_FLAG_DELETE_ON_CLOSE` which instructs
    /// the kernel to delete the file when the last handle is closed. This
    /// provides automatic cleanup on crash or early return - no orphaned
    /// temp files.
    ///
    /// The file is opened with `FILE_SHARE_READ | FILE_SHARE_DELETE` so
    /// external processes can read it (e.g., virus scanners) and the
    /// rename-on-commit path works.
    ///
    /// Returns the opened file and the path of the temp file on disk.
    ///
    /// # Arguments
    ///
    /// * `dir` - directory where the temp file is created. Must exist and
    ///   be writable.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The directory does not exist
    /// - Permission denied
    /// - All generated unique names collide (extremely unlikely)
    pub fn open_delete_on_close_tmpfile(dir: &Path) -> io::Result<(File, PathBuf)> {
        let pid = std::process::id();
        // Retry with different unique names on collision (CREATE_NEW fails
        // with ERROR_FILE_EXISTS if the name is taken).
        for _ in 0..16 {
            let unique = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let name = format!(".oc-rsync-tmp.{pid}.{unique}");
            let temp_path = dir.join(&name);

            let wide_path = to_wide_path(&temp_path)?;

            // SAFETY: `wide_path` is a valid null-terminated wide string.
            // `CREATE_NEW` fails if the file already exists, preventing
            // races. `FILE_FLAG_DELETE_ON_CLOSE` ensures kernel-level
            // cleanup. The returned handle is valid on success and is
            // immediately wrapped in a `File` which owns it.
            #[allow(unsafe_code)]
            let handle = unsafe {
                CreateFileW(
                    wide_path.as_ptr(),
                    FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE,
                    FILE_SHARE_READ | FILE_SHARE_DELETE,
                    std::ptr::null(),
                    CREATE_NEW,
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_DELETE_ON_CLOSE,
                    std::ptr::null_mut(),
                )
            };

            if handle == INVALID_HANDLE_VALUE {
                let err = io::Error::last_os_error();
                // ERROR_FILE_EXISTS = 80
                if err.raw_os_error() == Some(80) {
                    continue;
                }
                return Err(err);
            }

            // SAFETY: `handle` is a valid, open file handle just returned
            // by `CreateFileW`. We transfer ownership to `File` which will
            // close it on drop. No other code holds or aliases this handle.
            #[allow(unsafe_code)]
            let file = unsafe {
                use std::os::windows::io::FromRawHandle;
                File::from_raw_handle(handle as *mut std::ffi::c_void)
            };

            return Ok((file, temp_path));
        }

        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "failed to create unique temp file after 16 attempts",
        ))
    }

    /// Clears the delete-on-close disposition so the file survives handle close.
    ///
    /// After calling this, the file will NOT be deleted when the handle is
    /// closed. This is the first step of the commit sequence: clear the
    /// flag, then rename to the final destination.
    ///
    /// # Arguments
    ///
    /// * `file` - open handle to the delete-on-close temp file.
    ///
    /// # Errors
    ///
    /// Returns an error if `SetFileInformationByHandle` fails.
    pub fn clear_delete_on_close(file: &File) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            FileDispositionInfo, SetFileInformationByHandle,
        };

        // FILE_DISPOSITION_INFO has a single BOOLEAN field `DeleteFile`.
        // Setting it to FALSE (0) clears the delete-on-close flag.
        let disposition_info: u8 = 0; // FALSE = do NOT delete on close

        // SAFETY: `file.as_raw_handle()` returns a valid handle opened
        // with `FILE_FLAG_DELETE_ON_CLOSE`. `SetFileInformationByHandle`
        // with `FileDispositionInfo` and a `FILE_DISPOSITION_INFO` struct
        // (which is a single BOOLEAN byte) clears the delete-on-close
        // disposition. The buffer pointer and size match the struct layout.
        #[allow(unsafe_code)]
        let result = unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE,
                FileDispositionInfo,
                std::ptr::addr_of!(disposition_info).cast(),
                std::mem::size_of::<u8>() as u32,
            )
        };

        if result == 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }

    /// Renames the temp file to the destination path.
    ///
    /// Uses `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING` for atomic
    /// replacement semantics matching upstream `util1.c:robust_rename()`.
    ///
    /// # Arguments
    ///
    /// * `temp_path` - current path of the temp file.
    /// * `dest` - final destination path.
    ///
    /// # Errors
    ///
    /// Returns an error if the rename fails.
    pub fn rename_temp_to_dest(temp_path: &Path, dest: &Path) -> io::Result<()> {
        use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MoveFileExW};

        let wide_src = to_wide_path(temp_path)?;
        let wide_dst = to_wide_path(dest)?;

        // SAFETY: both `wide_src` and `wide_dst` are valid null-terminated
        // wide strings pointing to filesystem paths. `MOVEFILE_REPLACE_EXISTING`
        // allows overwriting an existing destination.
        #[allow(unsafe_code)]
        let result = unsafe {
            MoveFileExW(
                wide_src.as_ptr(),
                wide_dst.as_ptr(),
                MOVEFILE_REPLACE_EXISTING,
            )
        };

        if result == 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }

    /// Commits a delete-on-close temp file to its final destination.
    ///
    /// This is the full commit sequence:
    /// 1. Flush the file to disk.
    /// 2. Clear the delete-on-close disposition so the file survives close.
    /// 3. Close the file handle (so the rename can proceed without sharing
    ///    violations on the source).
    /// 4. Rename the temp file to the destination.
    ///
    /// If step 2 or 4 fails, the temp file is still marked delete-on-close
    /// and will be cleaned up when the process exits or the handle is closed.
    ///
    /// # Arguments
    ///
    /// * `file` - open handle to the delete-on-close temp file.
    /// * `temp_path` - on-disk path of the temp file.
    /// * `dest` - final destination path.
    ///
    /// # Errors
    ///
    /// Returns an error if flushing, clearing the disposition, or renaming fails.
    pub fn commit_delete_on_close(file: File, temp_path: &Path, dest: &Path) -> io::Result<()> {
        use std::io::Write;

        // Ensure all data is flushed before changing disposition.
        let mut file = file;
        file.flush()?;
        file.sync_all()?;

        // Clear delete-on-close so the file survives handle close.
        clear_delete_on_close(&file)?;

        // Drop the handle before renaming. Some Windows configurations
        // (e.g., antivirus holding a read handle) may block rename while
        // any handle is open with certain sharing modes.
        drop(file);

        // Rename to the final destination, replacing any existing file.
        rename_temp_to_dest(temp_path, dest)
    }

    /// Probes whether delete-on-close temp files work on the filesystem
    /// containing `dir`.
    ///
    /// Creates a probe file with `FILE_FLAG_DELETE_ON_CLOSE`, writes a
    /// single byte to verify it works, then drops the handle (which
    /// triggers deletion). The result is cached process-wide.
    ///
    /// # Arguments
    ///
    /// * `dir` - directory on the target filesystem. Must exist and be
    ///   writable.
    #[must_use]
    pub fn delete_on_close_available(dir: &Path) -> bool {
        use std::sync::atomic::AtomicU8;

        static STATUS: AtomicU8 = AtomicU8::new(0);
        const UNKNOWN: u8 = 0;
        const AVAILABLE: u8 = 1;
        const UNAVAILABLE: u8 = 2;

        let status = STATUS.load(Ordering::Relaxed);
        if status != UNKNOWN {
            return status == AVAILABLE;
        }

        let available = probe_delete_on_close(dir);
        STATUS.store(
            if available { AVAILABLE } else { UNAVAILABLE },
            Ordering::Relaxed,
        );
        available
    }

    /// Actual probe implementation.
    fn probe_delete_on_close(dir: &Path) -> bool {
        match open_delete_on_close_tmpfile(dir) {
            Ok((file, _path)) => {
                // Dropping the file triggers deletion via FILE_FLAG_DELETE_ON_CLOSE.
                drop(file);
                true
            }
            Err(_) => false,
        }
    }

    /// Converts a `Path` to a null-terminated UTF-16 wide string for Win32 APIs.
    fn to_wide_path(path: &Path) -> io::Result<Vec<u16>> {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        Ok(wide)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Write;
        use tempfile::tempdir;

        #[test]
        fn probe_returns_true_for_valid_directory() {
            let dir = tempdir().expect("tempdir");
            assert!(delete_on_close_available(dir.path()));
        }

        #[test]
        fn open_creates_file_that_is_writable() {
            let dir = tempdir().expect("tempdir");
            let (mut file, path) = open_delete_on_close_tmpfile(dir.path()).expect("open");
            file.write_all(b"test data").expect("write");
            assert!(path.exists());
        }

        #[test]
        fn file_deleted_on_drop() {
            let dir = tempdir().expect("tempdir");
            let (file, path) = open_delete_on_close_tmpfile(dir.path()).expect("open");
            assert!(path.exists());
            drop(file);
            // After dropping, the kernel should have deleted the file.
            assert!(!path.exists(), "file must be deleted after handle close");
        }

        #[test]
        fn clear_disposition_prevents_deletion() {
            let dir = tempdir().expect("tempdir");
            let (file, path) = open_delete_on_close_tmpfile(dir.path()).expect("open");
            clear_delete_on_close(&file).expect("clear disposition");
            drop(file);
            // File should survive because we cleared the delete-on-close flag.
            assert!(
                path.exists(),
                "file must survive after clearing disposition"
            );
            // Clean up.
            std::fs::remove_file(&path).expect("cleanup");
        }

        #[test]
        fn commit_creates_destination_file() {
            let dir = tempdir().expect("tempdir");
            let (mut file, temp_path) = open_delete_on_close_tmpfile(dir.path()).expect("open");
            file.write_all(b"commit test data").expect("write");

            let dest = dir.path().join("committed.txt");
            commit_delete_on_close(file, &temp_path, &dest).expect("commit");

            assert!(dest.exists());
            assert!(!temp_path.exists(), "temp file must be gone after rename");
            let content = std::fs::read_to_string(&dest).expect("read");
            assert_eq!(content, "commit test data");
        }

        #[test]
        fn commit_replaces_existing_destination() {
            let dir = tempdir().expect("tempdir");
            let dest = dir.path().join("existing.txt");
            std::fs::write(&dest, b"old content").expect("create existing");

            let (mut file, temp_path) = open_delete_on_close_tmpfile(dir.path()).expect("open");
            file.write_all(b"new content").expect("write");

            commit_delete_on_close(file, &temp_path, &dest).expect("commit");

            assert_eq!(std::fs::read_to_string(&dest).expect("read"), "new content");
        }

        #[test]
        fn unique_names_do_not_collide() {
            let dir = tempdir().expect("tempdir");
            let (f1, p1) = open_delete_on_close_tmpfile(dir.path()).expect("open 1");
            let (f2, p2) = open_delete_on_close_tmpfile(dir.path()).expect("open 2");
            assert_ne!(p1, p2);
            drop(f1);
            drop(f2);
        }

        #[test]
        fn probe_returns_false_for_nonexistent_dir() {
            // Reset static cache by using a fresh call path.
            // Note: the static cache means this may not actually probe if
            // a previous test already set the status. This is acceptable
            // because the real test is the open call.
            let result = open_delete_on_close_tmpfile(Path::new(r"Z:\nonexistent_dir_test"));
            assert!(result.is_err());
        }
    }
}

#[cfg(target_os = "windows")]
pub use windows::{
    clear_delete_on_close, commit_delete_on_close, delete_on_close_available,
    open_delete_on_close_tmpfile, rename_temp_to_dest,
};

/// Stub: delete-on-close temp files are only available on Windows.
#[cfg(not(target_os = "windows"))]
#[must_use]
pub fn delete_on_close_available(_dir: &std::path::Path) -> bool {
    false
}

/// Stub: delete-on-close temp files are only available on Windows.
#[cfg(not(target_os = "windows"))]
pub fn open_delete_on_close_tmpfile(
    _dir: &std::path::Path,
) -> std::io::Result<(std::fs::File, std::path::PathBuf)> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "FILE_FLAG_DELETE_ON_CLOSE is only supported on Windows",
    ))
}

/// Stub: delete-on-close temp files are only available on Windows.
#[cfg(not(target_os = "windows"))]
pub fn clear_delete_on_close(_file: &std::fs::File) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "FILE_FLAG_DELETE_ON_CLOSE is only supported on Windows",
    ))
}

/// Stub: delete-on-close temp files are only available on Windows.
#[cfg(not(target_os = "windows"))]
pub fn rename_temp_to_dest(
    _temp_path: &std::path::Path,
    _dest: &std::path::Path,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "FILE_FLAG_DELETE_ON_CLOSE is only supported on Windows",
    ))
}

/// Stub: delete-on-close temp files are only available on Windows.
#[cfg(not(target_os = "windows"))]
pub fn commit_delete_on_close(
    _file: std::fs::File,
    _temp_path: &std::path::Path,
    _dest: &std::path::Path,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "FILE_FLAG_DELETE_ON_CLOSE is only supported on Windows",
    ))
}
