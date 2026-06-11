//! Windows extended-length path (`\\?\`) helper.
//!
//! The Win32 file APIs (`CreateFileW`, `DeleteFileW`, `MoveFileW`, etc.) cap
//! plain paths at `MAX_PATH` = 260 characters. To address paths longer than
//! that limit, callers must opt in by prefixing the absolute path with `\\?\`
//! for local drives or `\\?\UNC\` for UNC shares. The `\\?\` prefix instructs
//! the Object Manager to pass the path through verbatim - no `.`/`..`
//! collapsing, no forward-slash conversion, no implicit current-directory
//! resolution.
//!
//! [`to_extended_path`] is the single entry point for adding the prefix. It
//! is idempotent: a path that already starts with `\\?\` or `\\.\` is returned
//! unchanged. On non-Windows targets the helper compiles to an identity
//! pass-through so call sites do not need `#[cfg]` guards.
//!
//! References:
//! - <https://learn.microsoft.com/windows/win32/fileio/naming-a-file>
//! - <https://learn.microsoft.com/windows/win32/fileio/maximum-file-path-limitation>
//!
//! # Semantics
//!
//! On Windows, `to_extended_path` **always** rewrites unprefixed absolute
//! paths even when they are shorter than `MAX_PATH`. Choosing a single,
//! consistent rule simplifies the call-site contract and avoids the trap
//! where a short path that happens to grow at runtime (e.g. via appending a
//! filename) silently loses long-path support. The owned-path cost is paid
//! only on paths that actually need conversion; already-prefixed paths and
//! all non-Windows inputs return a [`std::borrow::Cow::Borrowed`].

use std::borrow::Cow;
use std::path::Path;

/// Returns the input path with a `\\?\` (or `\\?\UNC\`) prefix added when
/// needed for Windows long-path support.
///
/// On non-Windows targets the helper is an identity no-op so call sites can
/// be uniform across platforms. On Windows, behaviour is:
///
/// - Paths already starting with `\\?\` or `\\.\` are returned unchanged
///   (idempotent).
/// - UNC paths (`\\server\share\...`) are rewritten to `\\?\UNC\server\share\...`.
/// - Drive-letter paths (`C:\...`) are rewritten to `\\?\C:\...`.
/// - Relative paths and other shapes that the extended-prefix form cannot
///   represent are returned unchanged so the OS sees the same input it
///   would have seen without the helper.
/// - Forward slashes in the rewritten portion are converted to backslashes
///   because the `\\?\` prefix disables the kernel's automatic conversion.
pub fn to_extended_path(p: &Path) -> Cow<'_, Path> {
    #[cfg(windows)]
    {
        to_extended_path_windows(p)
    }
    #[cfg(not(windows))]
    {
        Cow::Borrowed(p)
    }
}

#[cfg(windows)]
fn to_extended_path_windows(p: &Path) -> Cow<'_, Path> {
    use std::path::PathBuf;

    let raw = match p.to_str() {
        Some(s) => s,
        // Non-UTF-8 paths are rare on Windows and the prefix logic operates on
        // ASCII anchors only; pass through unchanged rather than corrupt the
        // OsStr by lossy conversion.
        None => return Cow::Borrowed(p),
    };

    if raw.starts_with(r"\\?\") || raw.starts_with(r"\\.\") {
        return Cow::Borrowed(p);
    }

    // UNC paths can arrive with either backslash (`\\server\share`) or
    // forward-slash (`//server/share`) separators because Win32 accepts both
    // in unprefixed paths and our callers may receive paths in either form
    // from cross-platform sources. The `\\?\UNC\` prefix itself disables
    // kernel slash conversion, so the suffix must be normalised here.
    if let Some(rest) = raw
        .strip_prefix(r"\\")
        .or_else(|| raw.strip_prefix("//"))
    {
        // UNC path: \\server\share\... -> \\?\UNC\server\share\...
        let normalised: String = rest
            .chars()
            .map(|c| if c == '/' { '\\' } else { c })
            .collect();
        let mut buf = String::with_capacity(normalised.len() + 8);
        buf.push_str(r"\\?\UNC\");
        buf.push_str(&normalised);
        return Cow::Owned(PathBuf::from(buf));
    }

    if is_drive_letter_absolute(raw) {
        let normalised: String = raw
            .chars()
            .map(|c| if c == '/' { '\\' } else { c })
            .collect();
        let mut buf = String::with_capacity(normalised.len() + 4);
        buf.push_str(r"\\?\");
        buf.push_str(&normalised);
        return Cow::Owned(PathBuf::from(buf));
    }

    // Relative paths or other shapes the extended prefix cannot encode: leave
    // them alone so callers fall back to standard MAX_PATH semantics.
    Cow::Borrowed(p)
}

#[cfg(windows)]
fn is_drive_letter_absolute(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() < 3 {
        return false;
    }
    bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/')
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::borrow::Cow;
    use std::path::{Path, PathBuf};

    #[test]
    fn short_drive_path_is_prefixed() {
        let input = Path::new(r"C:\short");
        let out = to_extended_path(input);
        assert!(matches!(out, Cow::Owned(_)));
        assert_eq!(out.as_ref(), Path::new(r"\\?\C:\short"));
    }

    #[test]
    fn long_drive_path_is_prefixed() {
        let mut deep = PathBuf::from(r"C:\");
        for i in 0..30 {
            deep.push(format!("segment_{:0>5}", i));
        }
        assert!(deep.as_os_str().len() > 260);
        let out = to_extended_path(&deep);
        assert!(matches!(out, Cow::Owned(_)));
        let expected = format!(r"\\?\{}", deep.display());
        assert_eq!(out.as_ref(), Path::new(&expected));
    }

    #[test]
    fn unc_path_gets_unc_prefix() {
        let input = Path::new(r"\\server\share\file.txt");
        let out = to_extended_path(input);
        assert!(matches!(out, Cow::Owned(_)));
        assert_eq!(out.as_ref(), Path::new(r"\\?\UNC\server\share\file.txt"));
    }

    #[test]
    fn already_extended_path_is_identity() {
        let input = Path::new(r"\\?\C:\foo\bar");
        let out = to_extended_path(input);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out.as_ref(), input);
    }

    #[test]
    fn already_device_path_is_identity() {
        let input = Path::new(r"\\.\PhysicalDrive0");
        let out = to_extended_path(input);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out.as_ref(), input);
    }

    #[test]
    fn forward_slashes_converted_for_drive_paths() {
        let input = Path::new("C:/foo/bar");
        let out = to_extended_path(input);
        assert!(matches!(out, Cow::Owned(_)));
        assert_eq!(out.as_ref(), Path::new(r"\\?\C:\foo\bar"));
    }

    #[test]
    fn forward_slashes_converted_for_unc_paths() {
        let input = Path::new("//server/share/file");
        let out = to_extended_path(input);
        assert!(matches!(out, Cow::Owned(_)));
        assert_eq!(out.as_ref(), Path::new(r"\\?\UNC\server\share\file"));
    }

    #[test]
    fn relative_path_is_identity() {
        let input = Path::new(r"relative\path");
        let out = to_extended_path(input);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out.as_ref(), input);
    }

    #[test]
    fn drive_letter_without_separator_is_identity() {
        // `C:foo` is a drive-relative path; the extended prefix cannot
        // represent it because the prefix requires a fully-qualified path.
        let input = Path::new("C:foo");
        let out = to_extended_path(input);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out.as_ref(), input);
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;
    use std::borrow::Cow;
    use std::path::Path;

    #[test]
    fn non_windows_is_identity_for_drive_path() {
        let input = Path::new("C:/foo/bar");
        let out = to_extended_path(input);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out.as_ref(), input);
    }

    #[test]
    fn non_windows_is_identity_for_posix_path() {
        let input = Path::new("/usr/local/bin/oc-rsync");
        let out = to_extended_path(input);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out.as_ref(), input);
    }

    #[test]
    fn non_windows_is_identity_for_unc_like_path() {
        let input = Path::new(r"\\server\share\file");
        let out = to_extended_path(input);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out.as_ref(), input);
    }
}
