//! Platform-normalized path encoding for the rsync wire protocol.
//!
//! On Unix, filesystem paths use `/` as the only separator, so converting a
//! `Path` to its wire-format byte representation is a zero-copy borrow of the
//! underlying `OsStr` bytes. On Windows, paths constructed via
//! [`std::path::PathBuf::push`] or [`std::path::Path::join`] use the native
//! `\` separator. The rsync wire format requires `/` (upstream
//! `flist.c:send_file_entry()` writes filename bytes verbatim - upstream's
//! Windows port runs under Cygwin's POSIX layer, which presents `/`-separated
//! paths to the application). oc-rsync targets native Win32 directly, so the
//! sender must perform the separator normalization explicitly.
//!
//! This module mirrors the identity-on-Unix / convert-on-Windows pattern of
//! the sibling `wire_mode` module.
//!
//! # Upstream Reference
//!
//! - `flist.c:534-570` `send_file_entry()` filename emission - the wire bytes
//!   are written verbatim with no separator normalisation.
//! - `util1.c:955-961` `__CYGWIN__` block - the only `\` handling in upstream
//!   lives on the Cygwin POSIX boundary, which oc-rsync does not run under.

use std::borrow::Cow;
use std::path::Path;

/// Returns the wire-format byte representation of a filesystem path.
///
/// On Unix, this is a zero-copy borrow of the path's `OsStr` bytes.
///
/// On non-Unix platforms (Windows), `\` separators are translated to `/` so
/// the bytes match the format a POSIX peer expects to read on the wire.
/// Allocation is avoided when the path contains no `\` byte.
#[cfg(unix)]
#[inline]
#[must_use]
pub(crate) fn path_bytes_to_wire(p: &Path) -> Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt;
    Cow::Borrowed(p.as_os_str().as_bytes())
}

/// Returns the wire-format byte representation of a filesystem path.
///
/// On non-Unix platforms (Windows), `\` separators are translated to `/` so
/// the bytes match the format a POSIX peer expects to read on the wire.
/// Allocation is avoided when the path contains no `\` byte.
#[cfg(not(unix))]
#[inline]
#[must_use]
pub(crate) fn path_bytes_to_wire(p: &Path) -> Cow<'_, [u8]> {
    let raw = p.as_os_str().as_encoded_bytes();
    if raw.contains(&b'\\') {
        let mut out = Vec::with_capacity(raw.len());
        for &b in raw {
            out.push(if b == b'\\' { b'/' } else { b });
        }
        Cow::Owned(out)
    } else {
        Cow::Borrowed(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn forward_slash_path_is_identity() {
        let p = PathBuf::from("subdir/file.txt");
        let bytes = path_bytes_to_wire(&p);
        assert_eq!(&*bytes, b"subdir/file.txt");
    }

    #[test]
    fn empty_path_yields_empty_bytes() {
        let p = PathBuf::new();
        let bytes = path_bytes_to_wire(&p);
        assert_eq!(&*bytes, b"");
    }

    #[test]
    fn dot_path_is_identity() {
        let p = PathBuf::from(".");
        let bytes = path_bytes_to_wire(&p);
        assert_eq!(&*bytes, b".");
    }

    #[cfg(unix)]
    #[test]
    fn unix_borrows_without_allocation() {
        let p = PathBuf::from("a/b/c");
        let bytes = path_bytes_to_wire(&p);
        assert!(matches!(bytes, Cow::Borrowed(_)));
        assert_eq!(&*bytes, b"a/b/c");
    }

    #[cfg(unix)]
    #[test]
    fn unix_preserves_backslash_byte_in_filename() {
        // On Unix, `\` is a legitimate filename byte and must NOT be rewritten.
        let p = PathBuf::from("weird\\name");
        let bytes = path_bytes_to_wire(&p);
        assert_eq!(&*bytes, b"weird\\name");
    }

    #[cfg(windows)]
    #[test]
    fn windows_backslash_is_translated_to_forward_slash() {
        let mut p = PathBuf::from("subdir");
        p.push("file.txt");
        let bytes = path_bytes_to_wire(&p);
        assert_eq!(&*bytes, b"subdir/file.txt");
    }

    #[cfg(windows)]
    #[test]
    fn windows_deep_path_is_translated() {
        let bytes = path_bytes_to_wire(Path::new("a\\b\\c"));
        assert_eq!(&*bytes, b"a/b/c");
    }

    #[cfg(windows)]
    #[test]
    fn windows_already_forward_slash_borrows() {
        let p = PathBuf::from("a/b/c");
        let bytes = path_bytes_to_wire(&p);
        assert!(matches!(bytes, Cow::Borrowed(_)));
        assert_eq!(&*bytes, b"a/b/c");
    }

    #[cfg(windows)]
    #[test]
    fn windows_mixed_separators_are_normalized() {
        let bytes = path_bytes_to_wire(Path::new("a/b\\c/d\\e"));
        assert_eq!(&*bytes, b"a/b/c/d/e");
    }
}
