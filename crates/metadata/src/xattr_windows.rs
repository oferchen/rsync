//! Windows extended-attribute backend backed by NTFS Alternate Data Streams.
//!
//! POSIX-style "extended attributes" map naturally onto NTFS ADS: each named
//! attribute is exposed as an alternate data stream attached to the file.
//! The stream syntax `path:streamname:$DATA` lets standard Win32 file APIs
//! read, write, and delete individual streams. Listing all streams attached
//! to a file requires the dedicated `FindFirstStreamW`/`FindNextStreamW`
//! pair, which iterates every named data stream regardless of size.
//!
//! # Mapping
//!
//! - `list_attributes(path)` enumerates streams via `FindFirstStreamW` and
//!   skips the unnamed primary stream `::$DATA`. Each remaining entry is
//!   stripped of its leading `:` and trailing `:$DATA` so the wire-format
//!   name matches the POSIX semantics expected by upstream rsync.
//! - `read_attribute(path, name)` opens `path:name:$DATA` for read access
//!   and slurps the full stream payload. `ERROR_FILE_NOT_FOUND` and
//!   `ERROR_PATH_NOT_FOUND` map to `Ok(None)`, mirroring `xattr::get` on
//!   POSIX systems.
//! - `write_attribute(path, name, value)` opens the stream with
//!   `CREATE_ALWAYS | GENERIC_WRITE`, writes the value, and closes the
//!   handle.
//! - `remove_attribute(path, name)` calls `DeleteFileW` on the stream
//!   path. Missing streams return `Ok(())` so callers can blindly clear
//!   stale entries during sync.
//!
//! # Cross-platform parity
//!
//! Stream names are returned as `OsString`s decoded from the UTF-16 buffer
//! supplied by the kernel. The wider `xattr` module converts these to the
//! protocol's UTF-8 byte representation. Non-ASCII names are preserved
//! losslessly via the standard `OsStringExt::from_wide` round-trip.
//!
//! # Upstream Reference
//!
//! Upstream rsync 3.4.1 has no native Windows xattr backend; the closest
//! Unix counterpart is `xattrs.c:rsync_xal_get()`/`set_xattr()`, which we
//! mirror at the higher [`crate::xattr`] layer. This module supplies the
//! per-attribute primitives that the cross-platform layer requires.

#![allow(unsafe_code)]

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_HANDLE_EOF, ERROR_NO_MORE_FILES, ERROR_PATH_NOT_FOUND,
    GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows::Win32::Storage::FileSystem::{
    CREATE_ALWAYS, CreateFileW, DeleteFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FindClose,
    FindFirstStreamW, FindNextStreamW, FindStreamInfoStandard, OPEN_EXISTING, ReadFile,
    WIN32_FIND_STREAM_DATA, WriteFile,
};
use windows::core::PCWSTR;

/// Suffix appended by NTFS to every named data stream entry returned by
/// `FindFirstStreamW`/`FindNextStreamW`.
const STREAM_SUFFIX: &str = ":$DATA";

/// Converts an [`OsStr`] (a UTF-16 wide string on Windows) into the UTF-8
/// byte sequence used by the cross-platform xattr layer.
///
/// Names that are not valid UTF-8 cannot round-trip through the wire
/// protocol; they are coerced via the standard lossy path so the listing
/// still surfaces them rather than being silently dropped.
pub fn os_name_to_bytes(name: &std::ffi::OsStr) -> Vec<u8> {
    name.to_string_lossy().into_owned().into_bytes()
}

/// Encodes a path as a NUL-terminated UTF-16 buffer for `*W` Win32 calls.
fn path_to_wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Builds the NUL-terminated wide string `path:name:$DATA` used to open an
/// alternate data stream as if it were a normal file.
fn stream_path_wide(path: &Path, name: &[u8]) -> io::Result<Vec<u16>> {
    let name_str = std::str::from_utf8(name).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "xattr stream name must be valid UTF-8 on Windows",
        )
    })?;
    if name_str.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "xattr stream name must not be empty",
        ));
    }
    if name_str.contains(':') || name_str.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "xattr stream name must not contain ':' or NUL",
        ));
    }
    let mut buf: Vec<u16> = path.as_os_str().encode_wide().collect();
    buf.push(b':' as u16);
    buf.extend(name_str.encode_utf16());
    buf.extend(STREAM_SUFFIX.encode_utf16());
    buf.push(0);
    Ok(buf)
}

/// Translates the raw stream name reported by NTFS (`:streamname:$DATA`)
/// into the bare attribute name expected by the upper xattr layer.
fn parse_stream_name(raw: &[u16]) -> Option<OsString> {
    let len = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    let slice = &raw[..len];
    if slice.is_empty() {
        return None;
    }
    if slice[0] != b':' as u16 {
        return None;
    }
    // Strip the trailing `:$DATA` suffix when present (always the case for
    // NTFS data streams; named non-data streams use other suffixes and are
    // skipped to keep parity with POSIX xattrs).
    let suffix: Vec<u16> = STREAM_SUFFIX.encode_utf16().collect();
    if !slice.ends_with(&suffix) {
        return None;
    }
    let body = &slice[1..slice.len() - suffix.len()];
    if body.is_empty() {
        // `::$DATA` is the unnamed primary stream - the file's content.
        return None;
    }
    Some(OsString::from_wide(body))
}

/// Wraps a Win32 `FindFirst*` handle so we always release it.
struct FindStreamHandle(HANDLE);

impl Drop for FindStreamHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: handle came from `FindFirstStreamW` and has not been closed.
            unsafe {
                let _ = FindClose(self.0);
            }
        }
    }
}

/// Wraps a file/stream handle so we close it on drop.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: handle was returned from a successful `CreateFileW`.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// Returns `true` when an [`io::Error`] indicates the file or stream
/// simply does not exist, so callers can map it to `Ok(None)`.
fn io_error_is_missing(error: &io::Error) -> bool {
    let raw = error.raw_os_error().unwrap_or(0);
    raw == ERROR_FILE_NOT_FOUND.0 as i32 || raw == ERROR_PATH_NOT_FOUND.0 as i32
}

/// Maps a [`windows::core::Error`] to an [`io::Error`] preserving the raw
/// Win32 error code so [`io::Error::raw_os_error`] returns the underlying
/// numeric value (rather than the wrapping HRESULT).
fn windows_to_io_error(error: windows::core::Error) -> io::Error {
    // The HRESULT layout `0x8007XXXX` embeds the Win32 error in the lower
    // 16 bits when the facility is FACILITY_WIN32 (0x0007). When a Win32
    // helper has already stashed the code in `GetLastError`, prefer that
    // because it preserves errors > 0xFFFF.
    // SAFETY: `GetLastError` is always safe.
    let last = unsafe { GetLastError() };
    if last.0 != 0 {
        return io::Error::from_raw_os_error(last.0 as i32);
    }
    let hr = error.code().0;
    let win32 = if (hr as u32) >> 16 == 0x8007 {
        (hr as u32) & 0xFFFF
    } else {
        hr as u32
    };
    io::Error::from_raw_os_error(win32 as i32)
}

/// Lists every named alternate data stream attached to `path`.
///
/// The unnamed primary stream `::$DATA` is intentionally omitted because it
/// represents the file's main contents, not a user-visible extended
/// attribute. Returns an empty vector when the volume does not support ADS
/// (e.g. FAT32) or the file simply has no named streams.
///
/// The `_follow_symlinks` parameter is accepted for API parity with the
/// Unix backend; NTFS reparse points are always traversed by the
/// underlying `FindFirstStreamW` call.
pub fn list_attributes(path: &Path, _follow_symlinks: bool) -> io::Result<Vec<OsString>> {
    let wide = path_to_wide(path);
    // SAFETY: `WIN32_FIND_STREAM_DATA` is a POD struct of u16/i64 fields; the
    // all-zero pattern is a valid initial state and the Win32 API overwrites
    // it on success.
    let mut data: WIN32_FIND_STREAM_DATA = unsafe { std::mem::zeroed() };

    // SAFETY: `wide` is NUL-terminated; `data` outlives the call.
    let result = unsafe {
        FindFirstStreamW(
            PCWSTR(wide.as_ptr()),
            FindStreamInfoStandard,
            (&mut data as *mut WIN32_FIND_STREAM_DATA).cast(),
            0,
        )
    };

    let handle = match result {
        Ok(h) if !h.is_invalid() => FindStreamHandle(h),
        Ok(_) | Err(_) => {
            // Documented behaviour: failures plus `ERROR_HANDLE_EOF` mean
            // there are simply no streams to enumerate. FAT/exFAT volumes
            // surface other errors which we propagate so the caller sees
            // the underlying reason.
            // SAFETY: `GetLastError` is always safe.
            let code = unsafe { GetLastError() };
            if code == ERROR_HANDLE_EOF || code == ERROR_NO_MORE_FILES {
                return Ok(Vec::new());
            }
            return Err(io::Error::from_raw_os_error(code.0 as i32));
        }
    };

    let mut names = Vec::new();
    if let Some(name) = parse_stream_name(&data.cStreamName) {
        names.push(name);
    }

    loop {
        // SAFETY: `handle.0` is a live FindFirstStream handle; `data`
        // outlives the call.
        let next =
            unsafe { FindNextStreamW(handle.0, (&mut data as *mut WIN32_FIND_STREAM_DATA).cast()) };
        if next.is_err() {
            // SAFETY: `GetLastError` is always safe.
            let code = unsafe { GetLastError() };
            if code == ERROR_HANDLE_EOF || code == ERROR_NO_MORE_FILES {
                break;
            }
            return Err(io::Error::from_raw_os_error(code.0 as i32));
        }
        if let Some(name) = parse_stream_name(&data.cStreamName) {
            names.push(name);
        }
    }

    Ok(names)
}

/// Reads the contents of `path:name:$DATA` and returns them as a byte
/// vector, or `Ok(None)` when the stream does not exist.
///
/// `_follow_symlinks` is ignored on Windows; reparse-point traversal is
/// the default behaviour of `CreateFileW`.
pub fn read_attribute(
    path: &Path,
    name: &[u8],
    _follow_symlinks: bool,
) -> io::Result<Option<Vec<u8>>> {
    let wide = stream_path_wide(path, name)?;

    // SAFETY: `wide` is NUL-terminated; the security and template handles
    // are null which is the documented "use defaults" pattern.
    let raw = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    };
    let handle = match raw {
        Ok(h) if h == INVALID_HANDLE_VALUE => return Err(io::Error::last_os_error()),
        Ok(h) => OwnedHandle(h),
        Err(error) => {
            let io_err = windows_to_io_error(error);
            if io_error_is_missing(&io_err) {
                return Ok(None);
            }
            return Err(io_err);
        }
    };

    let mut buffer = Vec::with_capacity(256);
    let mut chunk = [0u8; 4096];
    loop {
        let mut read: u32 = 0;
        // SAFETY: handle is valid; `chunk` and `read` outlive the call.
        let res = unsafe { ReadFile(handle.0, Some(&mut chunk), Some(&mut read), None) };
        if res.is_err() {
            return Err(io::Error::last_os_error());
        }
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read as usize]);
    }

    Ok(Some(buffer))
}

/// Creates or replaces `path:name:$DATA` with the supplied value bytes.
///
/// `_follow_symlinks` is ignored on Windows; reparse-point traversal is
/// the default behaviour of `CreateFileW`.
pub fn write_attribute(
    path: &Path,
    name: &[u8],
    value: &[u8],
    _follow_symlinks: bool,
) -> io::Result<()> {
    let wide = stream_path_wide(path, name)?;

    // SAFETY: `wide` is NUL-terminated; defaults are documented for null
    // security and template handle arguments.
    let raw = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    };
    let handle = match raw {
        Ok(h) if h == INVALID_HANDLE_VALUE => return Err(io::Error::last_os_error()),
        Ok(h) => OwnedHandle(h),
        Err(error) => return Err(windows_to_io_error(error)),
    };

    if value.is_empty() {
        // CREATE_ALWAYS already truncated the stream to zero bytes; no
        // further write is required.
        return Ok(());
    }

    let mut offset = 0usize;
    while offset < value.len() {
        let chunk = &value[offset..];
        let len = chunk.len().min(u32::MAX as usize);
        let mut written: u32 = 0;
        // SAFETY: handle is valid; the slice outlives the call.
        let res = unsafe { WriteFile(handle.0, Some(&chunk[..len]), Some(&mut written), None) };
        if res.is_err() {
            return Err(io::Error::last_os_error());
        }
        if written == 0 {
            return Err(io::Error::other("WriteFile reported zero bytes written"));
        }
        offset += written as usize;
    }

    Ok(())
}

/// Deletes `path:name:$DATA` from the file. Missing streams are not an
/// error - this matches the POSIX `xattr::remove` semantics callers expect.
///
/// `_follow_symlinks` is ignored on Windows; reparse-point traversal is
/// the default behaviour of `DeleteFileW`.
pub fn remove_attribute(path: &Path, name: &[u8], _follow_symlinks: bool) -> io::Result<()> {
    let wide = stream_path_wide(path, name)?;
    // SAFETY: `wide` is NUL-terminated.
    let res = unsafe { DeleteFileW(PCWSTR(wide.as_ptr())) };
    match res {
        Ok(()) => Ok(()),
        Err(error) => {
            let io_err = windows_to_io_error(error);
            if io_error_is_missing(&io_err) {
                Ok(())
            } else {
                Err(io_err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// Returns `true` if the temp directory's volume supports alternate data
    /// streams. Used to gracefully skip tests on FAT32-mounted runners.
    fn ads_supported(file: &Path) -> bool {
        match write_attribute(file, b"oc_rsync_ads_probe", b"1", false) {
            Ok(()) => {
                let _ = remove_attribute(file, b"oc_rsync_ads_probe", false);
                true
            }
            Err(_) => false,
        }
    }

    #[test]
    fn parse_stream_name_strips_prefix_and_suffix() {
        let raw: Vec<u16> = ":foo:$DATA\0extra".encode_utf16().collect();
        let parsed = parse_stream_name(&raw).expect("parse");
        assert_eq!(parsed, OsString::from("foo"));
    }

    #[test]
    fn parse_stream_name_skips_default_stream() {
        let raw: Vec<u16> = "::$DATA\0".encode_utf16().collect();
        assert!(parse_stream_name(&raw).is_none());
    }

    #[test]
    fn parse_stream_name_rejects_non_data_streams() {
        let raw: Vec<u16> = ":foo:$INDEX_ALLOCATION\0".encode_utf16().collect();
        assert!(parse_stream_name(&raw).is_none());
    }

    #[test]
    fn parse_stream_name_handles_unicode() {
        let raw: Vec<u16> = ":caf\u{00e9}:$DATA\0".encode_utf16().collect();
        let parsed = parse_stream_name(&raw).expect("parse");
        assert_eq!(parsed, OsString::from("café"));
    }

    #[test]
    fn stream_path_wide_rejects_colon_in_name() {
        let path = Path::new("C:\\tmp\\f.txt");
        let err = stream_path_wide(path, b"bad:name").expect_err("must reject");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn stream_path_wide_rejects_empty_name() {
        let path = Path::new("C:\\tmp\\f.txt");
        let err = stream_path_wide(path, b"").expect_err("must reject");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn stream_path_wide_rejects_non_utf8() {
        let path = Path::new("C:\\tmp\\f.txt");
        let err = stream_path_wide(path, &[0xff, 0xfe, 0xfd]).expect_err("must reject");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn stream_path_wide_appends_data_suffix() {
        let path = Path::new("C:\\tmp\\f.txt");
        let wide = stream_path_wide(path, b"foo").expect("build");
        let s = String::from_utf16_lossy(&wide[..wide.len() - 1]);
        assert!(s.ends_with(":foo:$DATA"), "got {s}");
    }

    #[test]
    fn write_then_read_roundtrip() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("ads-roundtrip.txt");
        fs::write(&file, b"primary").expect("write file");
        if !ads_supported(&file) {
            eprintln!("ADS not supported on this volume; skipping");
            return;
        }
        write_attribute(&file, b"hello", b"world", false).expect("write attr");
        let value = read_attribute(&file, b"hello", false)
            .expect("read attr")
            .expect("attr present");
        assert_eq!(value, b"world");
    }

    #[test]
    fn list_returns_written_streams() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("ads-list.txt");
        fs::write(&file, b"primary").expect("write file");
        if !ads_supported(&file) {
            return;
        }
        write_attribute(&file, b"alpha", b"a", false).expect("alpha");
        write_attribute(&file, b"beta", b"bb", false).expect("beta");
        let mut names: Vec<String> = list_attributes(&file, false)
            .expect("list")
            .into_iter()
            .map(|n| n.to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert!(names.iter().any(|n| n == "alpha"), "names = {names:?}");
        assert!(names.iter().any(|n| n == "beta"), "names = {names:?}");
    }

    #[test]
    fn remove_makes_stream_disappear() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("ads-remove.txt");
        fs::write(&file, b"primary").expect("write file");
        if !ads_supported(&file) {
            return;
        }
        write_attribute(&file, b"gone", b"value", false).expect("write");
        remove_attribute(&file, b"gone", false).expect("remove");
        let value = read_attribute(&file, b"gone", false).expect("read after remove");
        assert!(value.is_none());
        let names: Vec<String> = list_attributes(&file, false)
            .expect("list")
            .into_iter()
            .map(|n| n.to_string_lossy().into_owned())
            .collect();
        assert!(!names.iter().any(|n| n == "gone"));
    }

    #[test]
    fn remove_missing_stream_is_ok() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("ads-remove-missing.txt");
        fs::write(&file, b"primary").expect("write file");
        if !ads_supported(&file) {
            return;
        }
        // Removing an attribute that was never written must succeed,
        // matching POSIX xattr::remove behaviour for receiver-side
        // resync passes.
        remove_attribute(&file, b"never-set", false).expect("remove missing");
    }

    #[test]
    fn empty_value_round_trips() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("ads-empty.txt");
        fs::write(&file, b"primary").expect("write file");
        if !ads_supported(&file) {
            return;
        }
        write_attribute(&file, b"empty", b"", false).expect("write empty");
        let value = read_attribute(&file, b"empty", false)
            .expect("read")
            .expect("present");
        assert!(value.is_empty());
    }

    #[test]
    fn non_ascii_stream_name_round_trips() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("ads-utf8.txt");
        fs::write(&file, b"primary").expect("write file");
        if !ads_supported(&file) {
            return;
        }
        let name = "café".as_bytes();
        write_attribute(&file, name, b"crema", false).expect("write");
        let value = read_attribute(&file, name, false)
            .expect("read")
            .expect("present");
        assert_eq!(value, b"crema");
        let names: Vec<String> = list_attributes(&file, false)
            .expect("list")
            .into_iter()
            .map(|n| n.to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().any(|n| n == "café"), "names = {names:?}");
    }

    #[test]
    fn read_missing_stream_returns_none() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("ads-missing.txt");
        fs::write(&file, b"primary").expect("write file");
        if !ads_supported(&file) {
            return;
        }
        let value = read_attribute(&file, b"does-not-exist", false).expect("read");
        assert!(value.is_none());
    }

    #[test]
    fn os_name_to_bytes_round_trip() {
        let s = OsString::from("user.test");
        let bytes = os_name_to_bytes(&s);
        assert_eq!(bytes, b"user.test");
    }
}
