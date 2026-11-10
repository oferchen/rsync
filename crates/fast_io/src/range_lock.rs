//! Non-blocking exclusive byte-range locks on an open file.
//!
//! The Windows daemon claims `max connections` slots with these locks, the
//! counterpart of upstream's `fcntl(F_SETLK)` slots (util1.c:712-723
//! `lock_range()`). A `LockFileEx` lock belongs to the file handle: it is
//! released when the handle closes, including when the owning process dies.
//! Two handles in one process therefore contend like two processes, which is
//! what per-connection slots need.
//!
//! On other platforms
//! [`try_lock_exclusive_range`](crate::range_lock::try_lock_exclusive_range)
//! returns `ErrorKind::Unsupported`; Unix callers use `fcntl` record locks directly.

use std::fs::File;
use std::io;

/// Tries to take an exclusive lock on `len` bytes of `file` starting at `offset`.
///
/// Returns `Ok(true)` when the range is now locked through this handle and
/// `Ok(false)` when another handle already holds an overlapping lock. The lock
/// lasts until `file` is closed. The range may lie beyond the end of the file.
///
/// `file` must be opened for synchronous I/O, as `std::fs::File` is, so the
/// call never pends.
#[cfg(windows)]
pub fn try_lock_exclusive_range(file: &File, offset: u64, len: u64) -> io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };
    use windows_sys::Win32::System::IO::{OVERLAPPED, OVERLAPPED_0, OVERLAPPED_0_0};

    let mut overlapped = OVERLAPPED {
        Anonymous: OVERLAPPED_0 {
            Anonymous: OVERLAPPED_0_0 {
                Offset: offset as u32,
                OffsetHigh: (offset >> 32) as u32,
            },
        },
        ..Default::default()
    };
    // SAFETY: the handle is owned by `file`, which outlives the call.
    // `overlapped` is a live, initialised OVERLAPPED that only carries the
    // range offset; the handle is synchronous, so LockFileEx completes before
    // returning and keeps no pointer to it. No safe wrapper exists: std and
    // fs2 lock whole files only.
    #[allow(unsafe_code)]
    let locked = unsafe {
        LockFileEx(
            file.as_raw_handle(),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            len as u32,
            (len >> 32) as u32,
            &mut overlapped,
        )
    };
    if locked != 0 {
        return Ok(true);
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
        Ok(false)
    } else {
        Err(err)
    }
}

/// Unsupported outside Windows; see the module documentation.
#[cfg(not(windows))]
pub fn try_lock_exclusive_range(file: &File, offset: u64, len: u64) -> io::Result<bool> {
    let _ = (file, offset, len);
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "byte-range locks through this API are Windows-only",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;

    fn open(path: &std::path::Path) -> File {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn a_held_range_refuses_a_second_handle_until_the_first_closes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let first = open(&path);
        let second = open(&path);

        assert!(try_lock_exclusive_range(&first, 4, 4).unwrap());
        assert!(!try_lock_exclusive_range(&second, 4, 4).unwrap());
        assert!(
            try_lock_exclusive_range(&second, 8, 4).unwrap(),
            "an adjacent slot is independent"
        );

        drop(first);
        assert!(try_lock_exclusive_range(&second, 4, 4).unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn a_range_past_the_end_of_an_empty_file_locks() {
        let dir = tempfile::tempdir().unwrap();
        let file = open(&dir.path().join("lock"));
        assert!(try_lock_exclusive_range(&file, 1 << 33, 4).unwrap());
    }

    #[cfg(not(windows))]
    #[test]
    fn other_platforms_report_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let file = open(&dir.path().join("lock"));
        let err = try_lock_exclusive_range(&file, 0, 4).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }
}
