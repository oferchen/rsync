#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(clippy::undocumented_unsafe_blocks)]
#![doc = include_str!("../README.md")]

use std::io;
use std::path::Path;

#[cfg(not(unix))]
type ModeType = libc::c_uint;
#[cfg(not(unix))]
type DeviceType = libc::c_uint;

#[cfg(unix)]
mod unix {
    use super::{Path, io};
    use nix::sys::stat::{Mode, SFlag, mknod as nix_mknod};
    use nix::unistd::mkfifo as nix_mkfifo;

    pub fn mkfifo(path: &Path, mode: libc::mode_t) -> io::Result<()> {
        let mode = Mode::from_bits_truncate(mode);
        nix_mkfifo(path, mode).map_err(io::Error::from)
    }

    pub fn mknod(path: &Path, mode: libc::mode_t, device: libc::dev_t) -> io::Result<()> {
        let kind = SFlag::from_bits_truncate(mode);
        let perm = Mode::from_bits_truncate(mode);
        nix_mknod(path, kind, perm, device).map_err(io::Error::from)
    }
}

#[cfg(unix)]
#[cfg_attr(docsrs, doc(cfg(unix)))]
/// Creates a FIFO special file at `path` using the requested `mode`.
///
/// The helper mirrors the behaviour of `mkfifo(3)` and is only available on
/// Unix platforms. The function returns an error when the path cannot be
/// represented as a C string or when the underlying syscall fails.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] if the provided path contains an
/// interior NUL byte. Other error kinds bubble up from the `mkfifo(3)` call,
/// such as [`io::ErrorKind::AlreadyExists`] or [`io::ErrorKind::PermissionDenied`].
///
/// # Examples
///
/// ```rust
/// # #[cfg(unix)] {
/// use std::env;
/// use std::fs;
/// use std::os::unix::fs::FileTypeExt;
/// use std::time::{SystemTime, UNIX_EPOCH};
///
/// let unique = SystemTime::now()
///     .duration_since(UNIX_EPOCH)
///     .unwrap()
///     .as_nanos();
/// let path = env::temp_dir().join(format!("rsync_fifo_{unique}"));
/// # let _ = fs::remove_file(&path);
/// apple_fs::mkfifo(&path, 0o600).unwrap();
/// let metadata = fs::metadata(&path).unwrap();
/// assert!(metadata.file_type().is_fifo());
/// fs::remove_file(&path).unwrap();
/// # }
/// ```
pub fn mkfifo(path: &Path, mode: libc::mode_t) -> io::Result<()> {
    unix::mkfifo(path, mode)
}

#[cfg(unix)]
#[cfg_attr(docsrs, doc(cfg(unix)))]
/// Creates a filesystem node at `path` with the supplied `mode` and `device`.
///
/// This wrapper exposes the subset of `mknod(2)` used by the rsync
/// implementation. Passing `libc::S_IFIFO` as the mode creates a named pipe.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] if the path cannot be converted to a
/// C string. Other failures surface directly from the `mknod(2)` syscall.
///
/// # Examples
///
/// ```rust
/// # #[cfg(unix)] {
/// use std::env;
/// use std::fs;
/// use std::os::unix::fs::FileTypeExt;
/// use std::time::{SystemTime, UNIX_EPOCH};
///
/// let unique = SystemTime::now()
///     .duration_since(UNIX_EPOCH)
///     .unwrap()
///     .as_nanos();
/// let path = env::temp_dir().join(format!("rsync_mknod_{unique}"));
/// # let _ = fs::remove_file(&path);
/// apple_fs::mknod(&path, libc::S_IFIFO | 0o600, 0).unwrap();
/// let metadata = fs::metadata(&path).unwrap();
/// assert!(metadata.file_type().is_fifo());
/// fs::remove_file(&path).unwrap();
/// # }
/// ```
pub fn mknod(path: &Path, mode: libc::mode_t, device: libc::dev_t) -> io::Result<()> {
    unix::mknod(path, mode, device)
}

#[cfg(not(unix))]
/// Stub implementation that reports the lack of FIFO support on non-Unix
/// platforms.
///
/// # Errors
///
/// Always returns an [`io::ErrorKind::Unsupported`] error to mirror the
/// behaviour of upstream rsync on unsupported targets.
pub fn mkfifo(_path: &Path, _mode: ModeType) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "mkfifo is only implemented on Unix platforms",
    ))
}

#[cfg(not(unix))]
/// Stub implementation that reports the lack of `mknod` support on non-Unix
/// platforms.
///
/// # Errors
///
/// Always returns an [`io::ErrorKind::Unsupported`] error.
pub fn mknod(_path: &Path, _mode: ModeType, _device: DeviceType) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "mknod is only implemented on Unix platforms",
    ))
}

/// Normalizes a filename to NFC (composed) Unicode form.
///
/// macOS HFS+/APFS stores filenames in NFD (decomposed) form, while Linux
/// and most other systems use NFC (composed). When transferring files between
/// platforms, the same visual filename (e.g., "cafe\u{0301}" vs "caf\u{00e9}")
/// may have different byte representations, causing delete-pass and quick-check
/// filename comparisons to fail.
///
/// On macOS this normalizes both the `read_dir` result and the file-list entry
/// name to NFC before comparison. On all other platforms this is a no-op that
/// returns the input unchanged, avoiding any allocation overhead.
///
/// # Upstream Reference
///
/// Upstream rsync handles this via `--iconv` when transferring between
/// systems with different filename encodings. This function provides the
/// equivalent normalization for the common macOS NFD case.
#[cfg(target_os = "macos")]
pub fn normalize_filename(name: &std::ffi::OsStr) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStrExt;
    use unicode_normalization::UnicodeNormalization;

    let bytes = name.as_bytes();
    // Fast path: if all bytes are ASCII, no normalization needed.
    if bytes.iter().all(|&b| b.is_ascii()) {
        return name.to_os_string();
    }
    // Convert to UTF-8 for normalization. Non-UTF-8 filenames pass through
    // unchanged - they cannot contain decomposed Unicode sequences.
    match std::str::from_utf8(bytes) {
        Ok(s) => {
            let normalized: String = s.nfc().collect();
            std::ffi::OsString::from(normalized)
        }
        Err(_) => name.to_os_string(),
    }
}

/// No-op stub on non-macOS platforms - returns the input unchanged.
#[cfg(not(target_os = "macos"))]
pub fn normalize_filename(name: &std::ffi::OsStr) -> std::ffi::OsString {
    name.to_os_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::env;
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    fn unique_path(prefix: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        env::temp_dir().join(format!("{prefix}_{unique}"))
    }

    #[cfg(unix)]
    #[test]
    fn mkfifo_creates_named_pipe() -> io::Result<()> {
        use std::os::unix::fs::FileTypeExt;

        let path = unique_path("rsync_fifo");
        mkfifo(&path, 0o600)?;
        let metadata = fs::metadata(&path)?;
        assert!(metadata.file_type().is_fifo());
        fs::remove_file(&path)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn mknod_creates_fifo_when_requested() -> io::Result<()> {
        use std::os::unix::fs::FileTypeExt;

        let path = unique_path("rsync_mknod");
        mknod(&path, libc::S_IFIFO | 0o600, 0)?;
        let metadata = fs::metadata(&path)?;
        assert!(metadata.file_type().is_fifo());
        fs::remove_file(&path)?;
        Ok(())
    }

    #[cfg(not(unix))]
    #[test]
    fn non_unix_platforms_report_unsupported_operations() {
        let path = Path::new("nonexistent");
        assert_eq!(
            mkfifo(path, 0).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            mknod(path, 0, 0).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }

    #[test]
    fn normalize_filename_ascii_unchanged() {
        let name = std::ffi::OsStr::new("hello.txt");
        let result = normalize_filename(name);
        assert_eq!(result, name);
    }

    #[test]
    fn normalize_filename_nfc_unchanged() {
        // "caf\u{00e9}" is already NFC (composed e-acute)
        let name = std::ffi::OsStr::new("caf\u{00e9}");
        let result = normalize_filename(name);
        assert_eq!(result, name);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn normalize_filename_nfd_to_nfc() {
        // "cafe\u{0301}" is NFD (e + combining acute) - should normalize to NFC
        let nfd = std::ffi::OsStr::new("cafe\u{0301}");
        let nfc = std::ffi::OsStr::new("caf\u{00e9}");
        let result = normalize_filename(nfd);
        assert_eq!(result, nfc);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn normalize_filename_complex_nfd() {
        // "u\u{0308}ber" (u + combining diaeresis) -> "\u{00fc}ber" (u-umlaut)
        let nfd = std::ffi::OsStr::new("u\u{0308}ber");
        let nfc = std::ffi::OsStr::new("\u{00fc}ber");
        let result = normalize_filename(nfd);
        assert_eq!(result, nfc);
    }
}
