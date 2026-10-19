//! Probing helper for NTFS Alternate Data Streams (ADS).
//!
//! Provides [`has_named_ads`] to determine whether a file possesses any named
//! alternate data streams, short-circuiting on the first named stream found.
//! Non-Windows targets compile an efficient zero-cost stub returning `false`.
//! Also provides [`warn_ads_without_xattrs_once`] to emit a one-shot warning
//! per transfer when an ADS is detected on a source file but `-X` is disabled.

use std::io;
use std::path::Path;

#[cfg(windows)]
mod imp {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows_sys::Win32::Foundation::{
        ERROR_HANDLE_EOF, ERROR_INVALID_FUNCTION, ERROR_NO_MORE_FILES, ERROR_NOT_SUPPORTED,
        GetLastError, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FindClose, FindFirstStreamW, FindNextStreamW, FindStreamInfoStandard,
        WIN32_FIND_STREAM_DATA,
    };

    use crate::to_extended_path;

    const STREAM_SUFFIX: &str = ":$DATA";

    /// One-shot guard for the ADS-without-xattrs warning.
    static ADS_WARNING_EMITTED: AtomicBool = AtomicBool::new(false);

    fn path_to_wide(path: &Path) -> Vec<u16> {
        to_extended_path(path)
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Returns `true` if `raw` describes a named data stream (not the default unnamed `::$DATA`).
    pub(super) fn is_named_data_stream(raw: &[u16]) -> bool {
        let len = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
        let slice = &raw[..len];
        if slice.is_empty() || slice[0] != b':' as u16 {
            return false;
        }
        let suffix: Vec<u16> = STREAM_SUFFIX.encode_utf16().collect();
        if !slice.ends_with(&suffix) {
            return false;
        }
        let body = &slice[1..slice.len() - suffix.len()];
        // `::$DATA` is the unnamed primary stream; non-empty body indicates a named ADS
        !body.is_empty()
    }

    struct StreamSearchHandle(windows_sys::Win32::Foundation::HANDLE);

    impl Drop for StreamSearchHandle {
        fn drop(&mut self) {
            if self.0 != INVALID_HANDLE_VALUE {
                // SAFETY: self.0 is a valid handle returned by FindFirstStreamW.
                unsafe {
                    FindClose(self.0);
                }
            }
        }
    }

    /// Returns `true` when `path` has at least one named NTFS alternate
    /// data stream (excluding the unnamed primary stream `::$DATA`).
    ///
    /// Short-circuits on the first named stream found without full stream enumeration.
    /// Non-NTFS volumes (e.g. FAT32/exFAT) gracefully return `Ok(false)`.
    pub fn has_named_ads(path: &Path) -> io::Result<bool> {
        let wide = path_to_wide(path);
        // SAFETY: WIN32_FIND_STREAM_DATA is POD; zeroing is valid.
        let mut data: WIN32_FIND_STREAM_DATA = unsafe { std::mem::zeroed() };

        // SAFETY: `wide` is NUL-terminated; `data` outlives the call.
        let handle = unsafe {
            FindFirstStreamW(
                wide.as_ptr(),
                FindStreamInfoStandard,
                (&mut data as *mut WIN32_FIND_STREAM_DATA).cast(),
                0,
            )
        };

        if handle == INVALID_HANDLE_VALUE {
            // SAFETY: GetLastError is safe.
            let code = unsafe { GetLastError() };
            if code == ERROR_HANDLE_EOF
                || code == ERROR_NO_MORE_FILES
                || code == ERROR_INVALID_FUNCTION
                || code == ERROR_NOT_SUPPORTED
            {
                return Ok(false);
            }
            return Err(io::Error::from_raw_os_error(code as i32));
        }

        let _guard = StreamSearchHandle(handle);

        if is_named_data_stream(&data.cStreamName) {
            return Ok(true);
        }

        loop {
            // SAFETY: handle is valid and data outlives the call.
            let ok = unsafe {
                FindNextStreamW(handle, (&mut data as *mut WIN32_FIND_STREAM_DATA).cast())
            };
            if ok == 0 {
                // SAFETY: GetLastError is safe.
                let code = unsafe { GetLastError() };
                if code == ERROR_HANDLE_EOF
                    || code == ERROR_NO_MORE_FILES
                    || code == ERROR_INVALID_FUNCTION
                    || code == ERROR_NOT_SUPPORTED
                {
                    return Ok(false);
                }
                return Err(io::Error::from_raw_os_error(code as i32));
            }
            if is_named_data_stream(&data.cStreamName) {
                return Ok(true);
            }
        }
    }

    /// Emits a one-shot warning if `path` carries alternate data streams and
    /// the warning has not yet fired during this transfer session.
    pub fn warn_ads_without_xattrs_once(path: &Path) {
        if !ADS_WARNING_EMITTED.load(Ordering::Relaxed)
            && has_named_ads(path).unwrap_or(false)
            && !ADS_WARNING_EMITTED.swap(true, Ordering::SeqCst)
        {
            tracing::warn!(
                "warning: windows alternate data streams on {} will not be preserved without --xattrs (-X)",
                path.display()
            );
        }
    }

    #[cfg(test)]
    /// Resets the ADS one-shot warning flag for testing purposes.
    pub fn reset_ads_warning_for_test() {
        ADS_WARNING_EMITTED.store(false, Ordering::SeqCst);
    }
}

#[cfg(windows)]
pub use imp::{has_named_ads, warn_ads_without_xattrs_once};

#[cfg(windows)]
#[cfg(test)]
#[doc(inline)]
pub use imp::reset_ads_warning_for_test;

#[cfg(not(windows))]
#[cfg(test)]
/// Resets the ADS one-shot warning flag on non-Windows platforms (no-op).
pub fn reset_ads_warning_for_test() {}

#[cfg(not(windows))]
/// Returns `true` when `path` has at least one named NTFS alternate
/// data stream (excluding the unnamed primary stream `::$DATA`).
///
/// Always returns `Ok(false)` on non-Windows platforms.
pub fn has_named_ads(_path: &Path) -> io::Result<bool> {
    Ok(false)
}

#[cfg(not(windows))]
/// Emits a one-shot warning if `path` carries alternate data streams.
///
/// No-op on non-Windows platforms.
pub fn warn_ads_without_xattrs_once(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_windows_stub_returns_false() {
        #[cfg(not(windows))]
        {
            let path = Path::new("dummy/file.txt");
            assert!(!has_named_ads(path).unwrap());
        }
    }

    #[cfg(windows)]
    #[test]
    fn is_named_data_stream_classification() {
        let default_stream: Vec<u16> = "::$DATA\0".encode_utf16().collect();
        assert!(!imp::is_named_data_stream(&default_stream));

        let named_stream: Vec<u16> = ":Zone.Identifier:$DATA\0".encode_utf16().collect();
        assert!(imp::is_named_data_stream(&named_stream));

        let non_data_stream: Vec<u16> = ":INDEX:$INDEX_ALLOCATION\0".encode_utf16().collect();
        assert!(!imp::is_named_data_stream(&non_data_stream));
    }
}
