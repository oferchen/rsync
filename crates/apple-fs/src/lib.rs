#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(clippy::undocumented_unsafe_blocks)]
#![doc = include_str!("../README.md")]

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

pub mod apple_double;
pub mod resource_fork;

pub use resource_fork::{
    FINDER_INFO_LEN, FINDER_INFO_XATTR, RESOURCE_FORK_XATTR, read_finder_info, read_resource_fork,
    remove_finder_info, remove_resource_fork, write_finder_info, write_resource_fork,
};

/// Filename prefix used for AppleDouble (`._foo`) sidecar files.
///
/// macOS prefixes the partner file's name with `._` when it has to write
/// resource-fork or Finder metadata onto a filesystem that does not support
/// extended attributes natively (FAT, SMB, some NFS exports). The prefix is
/// the same on every platform that produces or consumes AppleDouble streams.
pub const APPLE_DOUBLE_PREFIX: &str = "._";

#[cfg(not(unix))]
type ModeType = libc::c_uint;
#[cfg(not(unix))]
type DeviceType = libc::c_uint;

#[cfg(unix)]
mod unix {
    use super::{Path, io};
    use nix::sys::stat::{Mode, SFlag, mknod as nix_mknod};
    use nix::unistd::mkfifo as nix_mkfifo;

    pub(super) fn mkfifo(path: &Path, mode: libc::mode_t) -> io::Result<()> {
        let mode = Mode::from_bits_truncate(mode);
        nix_mkfifo(path, mode).map_err(io::Error::from)
    }

    pub(super) fn mknod(path: &Path, mode: libc::mode_t, device: libc::dev_t) -> io::Result<()> {
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

/// Returns `true` when `name` looks like an AppleDouble (`._foo`) sidecar.
///
/// The check is purely lexical and applies to every platform: the same
/// pattern is used for filtering on the sender side, for sibling-pairing on
/// the receiver side, and for downstream tooling. The `._` itself - i.e. an
/// empty payload after the prefix - is not considered an AppleDouble file
/// because no real partner exists.
///
/// # Examples
///
/// ```
/// use std::ffi::OsStr;
/// use apple_fs::is_apple_double_name;
///
/// assert!(is_apple_double_name(OsStr::new("._Info.plist")));
/// assert!(!is_apple_double_name(OsStr::new("Info.plist")));
/// assert!(!is_apple_double_name(OsStr::new("._")));
/// ```
pub fn is_apple_double_name(name: &OsStr) -> bool {
    name.to_str()
        .map(|s| s.len() > APPLE_DOUBLE_PREFIX.len() && s.starts_with(APPLE_DOUBLE_PREFIX))
        .unwrap_or(false)
}

/// Returns the AppleDouble companion path for `path`.
///
/// - For a regular file `dir/foo`, returns `Some(dir/._foo)`.
/// - For an AppleDouble sidecar `dir/._foo`, returns `Some(dir/foo)` (the
///   data fork it pairs with).
/// - Returns `None` for paths whose final component is empty, is `.`, is
///   `..`, or whose name cannot be encoded as UTF-8.
///
/// The returned path is purely lexical - the filesystem is not consulted -
/// so the helper is safe to call on either platform regardless of whether
/// the partner file actually exists.
///
/// # Examples
///
/// ```
/// use std::path::{Path, PathBuf};
/// use apple_fs::apple_double_companion;
///
/// assert_eq!(
///     apple_double_companion(Path::new("dir/Info.plist")),
///     Some(PathBuf::from("dir/._Info.plist")),
/// );
/// assert_eq!(
///     apple_double_companion(Path::new("dir/._Info.plist")),
///     Some(PathBuf::from("dir/Info.plist")),
/// );
/// ```
pub fn apple_double_companion(path: &Path) -> Option<PathBuf> {
    let file_name = path.file_name()?;
    let name_str = file_name.to_str()?;
    if name_str.is_empty() {
        return None;
    }
    let companion_name = if let Some(stripped) = name_str.strip_prefix(APPLE_DOUBLE_PREFIX) {
        if stripped.is_empty() {
            return None;
        }
        stripped.to_string()
    } else {
        format!("{APPLE_DOUBLE_PREFIX}{name_str}")
    };
    let mut companion = path.to_path_buf();
    companion.set_file_name(companion_name);
    Some(companion)
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

    #[test]
    fn is_apple_double_name_detects_prefix() {
        assert!(is_apple_double_name(OsStr::new("._Info.plist")));
        assert!(is_apple_double_name(OsStr::new("._x")));
    }

    #[test]
    fn is_apple_double_name_rejects_non_sidecars() {
        assert!(!is_apple_double_name(OsStr::new("Info.plist")));
        assert!(!is_apple_double_name(OsStr::new(".")));
        assert!(!is_apple_double_name(OsStr::new(".hidden")));
        assert!(!is_apple_double_name(OsStr::new("")));
        // The bare prefix "._" with no payload is not an AppleDouble file.
        assert!(!is_apple_double_name(OsStr::new("._")));
    }

    #[test]
    fn apple_double_companion_pairs_data_to_sidecar() {
        let data = std::path::PathBuf::from("dir/Info.plist");
        let sidecar = apple_double_companion(&data).expect("companion");
        assert_eq!(sidecar, std::path::PathBuf::from("dir/._Info.plist"));
    }

    #[test]
    fn apple_double_companion_pairs_sidecar_to_data() {
        let sidecar = std::path::PathBuf::from("dir/._Info.plist");
        let data = apple_double_companion(&sidecar).expect("companion");
        assert_eq!(data, std::path::PathBuf::from("dir/Info.plist"));
    }

    #[test]
    fn apple_double_companion_round_trip() {
        let original = std::path::PathBuf::from("a/b/c.txt");
        let sidecar = apple_double_companion(&original).expect("forward");
        let back = apple_double_companion(&sidecar).expect("reverse");
        assert_eq!(back, original);
    }

    #[test]
    fn apple_double_companion_returns_none_for_root() {
        assert!(apple_double_companion(Path::new("/")).is_none());
    }

    #[test]
    fn apple_double_companion_returns_none_for_bare_prefix() {
        // "._" has no payload to pair against.
        assert!(apple_double_companion(Path::new("dir/._")).is_none());
    }

    #[test]
    fn apple_double_companion_preserves_parent_directory() {
        let path = Path::new("/Users/me/Documents/draft.txt");
        let sidecar = apple_double_companion(path).expect("companion");
        assert_eq!(
            sidecar,
            std::path::PathBuf::from("/Users/me/Documents/._draft.txt"),
        );
    }
}
