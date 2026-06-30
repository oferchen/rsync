//! Symlink-target munge prefix handling for the generator role.
//!
//! Strips the `/rsyncd-munged/` prefix from symlink targets when the daemon
//! module has `munge symlinks = yes`, restoring the original target before it
//! is encoded onto the wire.

use std::path::PathBuf;

/// Strips the `/rsyncd-munged/` prefix from a symlink target when munging is active.
///
/// Mirrors upstream `flist.c:222-226`: when the daemon module has
/// `munge symlinks = yes`, the sender restores the original target before it
/// is encoded onto the wire so the receiver can reapply the prefix on its
/// side. The receiver-side write path uses the matching
/// [`crate::receiver::directory::apply_symlink_munge_prefix`] helper.
///
/// Targets that lack the prefix pass through unchanged; this is the
/// `llen > SYMLINK_PREFIX_LEN && strncmp(...) == 0` branch in upstream's
/// `readlink_stat()`.
pub(super) fn strip_symlink_munge_prefix(munge_symlinks: bool, target: PathBuf) -> PathBuf {
    if !munge_symlinks {
        return target;
    }
    let prefix_len = ::metadata::SYMLINK_MUNGE_PREFIX.len();
    // upstream: flist.c:222 - the strncmp guard requires `llen > SYMLINK_PREFIX_LEN`,
    // so a target consisting of exactly the prefix is left untouched.
    #[cfg(unix)]
    {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let bytes = target.as_os_str().as_bytes();
        let prefix = ::metadata::SYMLINK_MUNGE_PREFIX.as_bytes();
        if bytes.len() > prefix_len && bytes.starts_with(prefix) {
            let tail = OsStr::from_bytes(&bytes[prefix_len..]);
            return PathBuf::from(tail);
        }
        target
    }
    #[cfg(not(unix))]
    {
        match target.to_str() {
            Some(text)
                if text.len() > prefix_len
                    && text.starts_with(::metadata::SYMLINK_MUNGE_PREFIX) =>
            {
                PathBuf::from(&text[prefix_len..])
            }
            _ => target,
        }
    }
}

#[cfg(test)]
mod munge_strip_tests {
    use super::*;

    #[test]
    fn passes_through_when_disabled() {
        let original = PathBuf::from("/rsyncd-munged/etc/passwd");
        let returned = strip_symlink_munge_prefix(false, original.clone());
        assert_eq!(returned, original);
    }

    #[test]
    fn strips_prefix_when_enabled() {
        let original = PathBuf::from("/rsyncd-munged/etc/passwd");
        let returned = strip_symlink_munge_prefix(true, original);
        assert_eq!(returned, PathBuf::from("etc/passwd"));
    }

    #[test]
    fn passes_through_unprefixed_target_when_enabled() {
        let original = PathBuf::from("../sibling");
        let returned = strip_symlink_munge_prefix(true, original.clone());
        assert_eq!(returned, original);
    }

    #[test]
    fn does_not_strip_exact_prefix_match() {
        // upstream: flist.c:222 - `llen > SYMLINK_PREFIX_LEN` is strict, so a
        // target whose entire content is the prefix is left untouched. Without
        // the strict check we would emit an empty target onto the wire and
        // diverge from upstream byte-for-byte.
        let original = PathBuf::from("/rsyncd-munged/");
        let returned = strip_symlink_munge_prefix(true, original.clone());
        assert_eq!(returned, original);
    }
}
