/// Prefix prepended to symlink targets when munging is active.
///
/// Upstream rsync uses this exact prefix in `clientserver.c` to prevent
/// symlinks from escaping the module root when `use chroot = no`.
pub const SYMLINK_MUNGE_PREFIX: &str = "/rsyncd-munged/";

/// Prepends the munge prefix to a symlink target.
///
/// This transforms a symlink target so it cannot resolve to a path outside
/// the module directory. The prefix is always prepended, even if the target
/// is relative; upstream rsync behaves identically.
///
/// # upstream: `clientserver.c:munge_symlink()`
#[must_use]
pub fn munge_symlink(target: &str) -> String {
    let mut munged = String::with_capacity(SYMLINK_MUNGE_PREFIX.len() + target.len());
    munged.push_str(SYMLINK_MUNGE_PREFIX);
    munged.push_str(target);
    munged
}

/// Strips the munge prefix from a symlink target if present.
///
/// Returns `Some(original)` when the prefix was found and removed, or `None`
/// if the target was not munged.
///
/// # upstream: `clientserver.c:unmunge_symlink()`
#[must_use]
pub fn unmunge_symlink(target: &str) -> Option<String> {
    target
        .strip_prefix(SYMLINK_MUNGE_PREFIX)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- munge_symlink tests ---

    #[test]
    fn munge_prepends_prefix_to_absolute_path() {
        let result = munge_symlink("/etc/passwd");
        assert_eq!(result, "/rsyncd-munged//etc/passwd");
    }

    #[test]
    fn munge_prepends_prefix_to_relative_path() {
        let result = munge_symlink("../secret");
        assert_eq!(result, "/rsyncd-munged/../secret");
    }

    #[test]
    fn munge_prepends_prefix_to_simple_filename() {
        let result = munge_symlink("file.txt");
        assert_eq!(result, "/rsyncd-munged/file.txt");
    }

    #[test]
    fn munge_handles_empty_target() {
        let result = munge_symlink("");
        assert_eq!(result, "/rsyncd-munged/");
    }

    #[test]
    fn munge_handles_dot_target() {
        let result = munge_symlink(".");
        assert_eq!(result, "/rsyncd-munged/.");
    }

    // --- unmunge_symlink tests ---

    #[test]
    fn unmunge_strips_prefix() {
        let result = unmunge_symlink("/rsyncd-munged//etc/passwd");
        assert_eq!(result, Some("/etc/passwd".to_owned()));
    }

    #[test]
    fn unmunge_strips_prefix_from_relative() {
        let result = unmunge_symlink("/rsyncd-munged/../secret");
        assert_eq!(result, Some("../secret".to_owned()));
    }

    #[test]
    fn unmunge_returns_none_for_non_munged() {
        let result = unmunge_symlink("/etc/passwd");
        assert!(result.is_none());
    }

    #[test]
    fn unmunge_returns_none_for_partial_prefix() {
        let result = unmunge_symlink("/rsyncd-munged");
        assert!(result.is_none());
    }

    #[test]
    fn unmunge_returns_none_for_empty() {
        let result = unmunge_symlink("");
        assert!(result.is_none());
    }

    #[test]
    fn unmunge_strips_prefix_leaving_empty() {
        let result = unmunge_symlink("/rsyncd-munged/");
        assert_eq!(result, Some(String::new()));
    }

    // --- roundtrip tests ---

    #[test]
    fn roundtrip_absolute_path() {
        let original = "/usr/local/bin/tool";
        let munged = munge_symlink(original);
        let restored = unmunge_symlink(&munged);
        assert_eq!(restored, Some(original.to_owned()));
    }

    #[test]
    fn roundtrip_relative_path() {
        let original = "../../other/dir/file";
        let munged = munge_symlink(original);
        let restored = unmunge_symlink(&munged);
        assert_eq!(restored, Some(original.to_owned()));
    }

    #[test]
    fn roundtrip_empty_target() {
        let original = "";
        let munged = munge_symlink(original);
        let restored = unmunge_symlink(&munged);
        assert_eq!(restored, Some(original.to_owned()));
    }

    #[test]
    fn roundtrip_preserves_unicode() {
        let original = "/data/\u{00e9}l\u{00e8}ve/notes";
        let munged = munge_symlink(original);
        let restored = unmunge_symlink(&munged);
        assert_eq!(restored, Some(original.to_owned()));
    }
}
