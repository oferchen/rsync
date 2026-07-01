//! NTFS sparse-file marking via `DeviceIoControl(FSCTL_SET_SPARSE)`.
//!
//! On Linux and macOS the sparse writer produces holes implicitly: seeking past
//! a zero run and truncating with `set_len` leaves the skipped range
//! unallocated. NTFS does not honour that pattern - a seek-past-EOF followed by
//! `SetEndOfFile` zero-fills the gap and allocates blocks. To get the same
//! sparse allocation on Windows the file handle must first be flagged sparse
//! with `FSCTL_SET_SPARSE`; afterwards the identical seek + `set_len` logic the
//! Linux path uses materialises real holes.
//!
//! [`mark_file_sparse`] performs that flagging. It is best-effort: on a volume
//! that does not support sparse files (FAT/exFAT), or when the control code is
//! rejected, it returns `Ok(false)` so the caller falls back to a dense write
//! rather than failing the transfer. On non-Windows targets it is a zero-cost
//! no-op returning `Ok(true)`, keeping call sites free of `#[cfg]` plumbing.

use std::fs::File;
use std::io;

/// Marks an open file handle as sparse so subsequent seek-past-zero writes
/// produce filesystem holes on NTFS.
///
/// Mirrors the implicit hole creation the Linux/macOS sparse path relies on.
/// The Windows implementation issues `DeviceIoControl(FSCTL_SET_SPARSE)`.
///
/// # Best-effort semantics
///
/// A `false` return means the volume or filesystem does not support sparse
/// files (for example FAT/exFAT) or rejected the control code. The caller
/// should continue with a normal dense write; this is not an error. A `true`
/// return means the handle is now sparse and later zero runs will be
/// deallocated. On non-Windows platforms this always returns `Ok(true)` after
/// doing nothing, because those filesystems create holes implicitly.
///
/// # Errors
///
/// The Windows implementation never surfaces the "unsupported / not applicable"
/// class as an error (it maps those to `Ok(false)`); it is declared fallible so
/// the signature stays stable if a future revision wants to distinguish other
/// I/O failures. Callers treat any error as a signal to fall back to a dense
/// write and never abort the transfer on it.
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
pub fn mark_file_sparse(file: &File) -> io::Result<bool> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;

    let handle = file.as_raw_handle() as HANDLE;
    let mut bytes_returned: u32 = 0;

    // SAFETY: `handle` is a live handle owned by `file` for the duration of the
    // call. `FSCTL_SET_SPARSE` takes no input buffer, so passing null pointers
    // and zero sizes for both the in and out buffers is the documented calling
    // convention. `lpBytesReturned` points at a valid `u32` and `lpOverlapped`
    // is null (synchronous request), matching the handle's synchronous mode.
    let ok = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut bytes_returned,
            std::ptr::null_mut(),
        )
    };

    if ok == 0 {
        // Sparse files are unsupported on this volume (FAT/exFAT) or the
        // control code was refused. Treat it as "no sparse available" rather
        // than a hard failure so the caller writes densely instead.
        return Ok(false);
    }

    Ok(true)
}

/// Non-Windows no-op: Linux and macOS filesystems create holes implicitly when
/// the sparse writer seeks past zero runs, so no handle flag is required.
///
/// # Errors
///
/// Never returns an error on these platforms.
#[cfg(not(target_os = "windows"))]
#[inline]
pub fn mark_file_sparse(_file: &File) -> io::Result<bool> {
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::mark_file_sparse;

    #[test]
    fn mark_file_sparse_reports_availability() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("sparse-probe.bin");
        let file = std::fs::File::create(&path).expect("create");
        // On non-Windows this is a no-op that returns Ok(true); on Windows it
        // returns Ok(true) when the scratch volume is NTFS or Ok(false) on a
        // FAT/exFAT temp mount. Either way it must not error.
        let result = mark_file_sparse(&file);
        assert!(
            result.is_ok(),
            "mark_file_sparse must never hard-fail: {result:?}"
        );
    }
}
