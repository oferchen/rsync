//! Boundary adapters between raw pattern bytes and [`Path`](std::path::Path)
//! values.
//!
//! The filter model stores patterns and matches names as raw bytes, mirroring
//! upstream rsync's `char *` model (`exclude.c:1002` `rule_matches()`,
//! `lib/wildmatch.c:64` `dowild()`). Callers that interface with
//! [`std::path`] types convert at the edge through these adapters so the one
//! platform-specific conversion has a single owner.
//!
//! On Unix the conversion is exact in both directions
//! (`std::os::unix::ffi::OsStrExt`). On non-Unix targets (no upstream daemon;
//! native paths are not byte strings) the conversion renders through UTF-8,
//! replacing invalid sequences - the historical behaviour, kept only where an
//! exact mapping does not exist.

use std::borrow::Cow;
use std::path::Path;

/// Converts raw pattern bytes to a [`Path`] for filesystem access
/// (e.g. resolving a merge-file location named by a rule's pattern).
#[must_use]
pub fn pattern_path(bytes: &[u8]) -> Cow<'_, Path> {
    #[cfg(unix)]
    {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        Cow::Borrowed(Path::new(OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        Cow::Owned(std::path::PathBuf::from(
            String::from_utf8_lossy(bytes).into_owned(),
        ))
    }
}

/// Converts a [`Path`] to the raw byte string the filter model stores and
/// matches (the inverse of [`pattern_path`]).
#[must_use]
pub fn path_pattern_bytes(path: &Path) -> Cow<'_, [u8]> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Cow::Borrowed(path.as_os_str().as_bytes())
    }
    #[cfg(not(unix))]
    {
        Cow::Owned(path.to_string_lossy().into_owned().into_bytes())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{path_pattern_bytes, pattern_path};
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    #[test]
    fn non_utf8_bytes_round_trip_exactly() {
        let raw: &[u8] = b"caf\xe9/sub\xff";
        let path = pattern_path(raw);
        assert_eq!(path.as_ref(), Path::new(OsStr::from_bytes(raw)));
        assert_eq!(path_pattern_bytes(&path).as_ref(), raw);
    }
}
