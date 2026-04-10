//! Path sanitization mirroring upstream `util1.c:sanitize_path()`.
//!
//! When rsync runs in daemon mode, all paths received from the network are
//! passed through `sanitize_path` to prevent directory traversal attacks.
//! The function makes a path appear as if a chroot had occurred:
//!
//! - Leading `/` is removed (or replaced with the module root)
//! - `..` components that would escape the module root are collapsed
//! - Redundant `.` components and duplicate slashes are removed
//! - The result is always a relative path confined within the module root
//!
//! # Upstream Reference
//!
//! - `util1.c:1035` - `sanitize_path(dest, p, rootdir, depth, flags)`
//! - `flist.c:762` - applied to received file names in daemon mode
//! - `flist.c:2264` - applied to `--files-from` entries
//! - `flist.c:1154` - applied to symlink targets when `sanitize_paths && !munge_symlinks`

/// Sanitizes a path to prevent directory traversal beyond the module root.
///
/// Mirrors upstream `util1.c:sanitize_path()` with `depth=0` and
/// `SP_DEFAULT` flags (dot dirs are dropped). The function:
///
/// 1. Strips any leading `/` (absolute paths become relative)
/// 2. Collapses `..` components - those at the start with no remaining
///    depth budget are silently dropped; those deeper in the path back
///    up one level like `clean_fname` with `CFN_COLLAPSE_DOT_DOT_DIRS`
/// 3. Removes `.` components and collapses duplicate slashes
/// 4. Returns `"."` if the result would otherwise be empty
///
/// # Upstream Reference
///
/// - `util1.c:1035-1108` - `sanitize_path()`
pub fn sanitize_path(path: &str) -> String {
    sanitize_path_with_depth(path, 0, false)
}

/// Sanitizes a path preserving leading `.` directories.
///
/// Equivalent to upstream `sanitize_path()` with `SP_KEEP_DOT_DIRS` flag.
/// Used for `--files-from` entries where `./` prefixes are meaningful.
///
/// # Upstream Reference
///
/// - `flist.c:2264` - `sanitize_path(fbuf, fbuf, "", 0, SP_KEEP_DOT_DIRS)`
pub fn sanitize_path_keep_dot_dirs(path: &str) -> String {
    sanitize_path_with_depth(path, 0, true)
}

/// Core sanitization with configurable depth budget and dot-dir handling.
///
/// - `depth`: number of leading `..` segments allowed (0 for daemon mode)
/// - `keep_dot_dirs`: when true, preserves leading `./` (SP_KEEP_DOT_DIRS)
///
/// # Upstream Reference
///
/// - `util1.c:1035-1108`
fn sanitize_path_with_depth(path: &str, mut depth: i32, keep_dot_dirs: bool) -> String {
    let bytes = path.as_bytes();
    let mut p = 0usize;

    // upstream: util1.c:1051 - strip leading slash (absolute -> relative)
    if p < bytes.len() && bytes[p] == b'/' {
        p += 1;
    }

    // upstream: util1.c:1061 - drop leading "./" unless SP_KEEP_DOT_DIRS
    if !keep_dot_dirs {
        while p + 1 < bytes.len() && bytes[p] == b'.' && bytes[p + 1] == b'/' {
            p += 2;
            while p < bytes.len() && bytes[p] == b'/' {
                p += 1;
            }
        }
    }

    let mut result = Vec::with_capacity(bytes.len());
    // Segments before `start` are committed leading `..` within the depth budget.
    let mut start = 0usize;

    while p < bytes.len() {
        if bytes[p] == b'/' {
            p += 1;
            continue;
        }

        if !keep_dot_dirs && bytes[p] == b'.' && (p + 1 >= bytes.len() || bytes[p + 1] == b'/') {
            p += 1;
            continue;
        }

        if bytes[p] == b'.'
            && p + 1 < bytes.len()
            && bytes[p + 1] == b'.'
            && (p + 2 >= bytes.len() || bytes[p + 2] == b'/')
        {
            if depth <= 0 || result.len() > start {
                p += 2;
                if result.len() > start {
                    if result.last() == Some(&b'/') {
                        result.pop();
                    }
                    while result.len() > start && result.last() != Some(&b'/') {
                        result.pop();
                    }
                }
                continue;
            }
            depth -= 1;
            result.extend_from_slice(b"..");
            if p + 2 < bytes.len() {
                result.push(b'/');
            }
            p += 2;
            start = result.len();
            continue;
        }

        while p < bytes.len() && bytes[p] != b'/' {
            result.push(bytes[p]);
            p += 1;
        }
        if p < bytes.len() {
            result.push(b'/');
            p += 1;
        }
    }

    if result.len() > 1 && result.last() == Some(&b'/') {
        result.pop();
    }

    if result.is_empty() {
        ".".to_string()
    } else {
        // SAFETY: input is &str (valid UTF-8), and we only manipulate ASCII
        // path separators and dot components, preserving UTF-8 validity.
        String::from_utf8(result).unwrap_or_else(|_| ".".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Basic sanitization ---

    #[test]
    fn empty_path_becomes_dot() {
        assert_eq!(sanitize_path(""), ".");
    }

    #[test]
    fn dot_path_stays_dot() {
        assert_eq!(sanitize_path("."), ".");
    }

    #[test]
    fn simple_relative_path_unchanged() {
        assert_eq!(sanitize_path("foo/bar"), "foo/bar");
    }

    #[test]
    fn trailing_slash_removed() {
        assert_eq!(sanitize_path("foo/bar/"), "foo/bar");
    }

    // --- Absolute path stripping ---

    #[test]
    fn absolute_path_stripped() {
        assert_eq!(sanitize_path("/etc/passwd"), "etc/passwd");
    }

    #[test]
    fn root_only_becomes_dot() {
        assert_eq!(sanitize_path("/"), ".");
    }

    // --- Dot component removal ---

    #[test]
    fn interior_dot_removed() {
        assert_eq!(sanitize_path("foo/./bar"), "foo/bar");
    }

    #[test]
    fn leading_dot_slash_removed() {
        assert_eq!(sanitize_path("./foo"), "foo");
    }

    #[test]
    fn multiple_leading_dot_slash_removed() {
        assert_eq!(sanitize_path("./././foo"), "foo");
    }

    // --- Dotdot collapsing ---

    #[test]
    fn dotdot_at_start_dropped_depth_zero() {
        assert_eq!(sanitize_path("../foo"), "foo");
    }

    #[test]
    fn multiple_dotdot_at_start_dropped() {
        assert_eq!(sanitize_path("../../foo"), "foo");
    }

    #[test]
    fn dotdot_collapses_previous_component() {
        assert_eq!(sanitize_path("a/b/../c"), "a/c");
    }

    #[test]
    fn dotdot_at_end_collapses() {
        assert_eq!(sanitize_path("a/b/.."), "a");
    }

    #[test]
    fn dotdot_chain_collapses_fully() {
        assert_eq!(sanitize_path("a/b/c/../../d"), "a/d");
    }

    #[test]
    fn dotdot_cannot_escape_root() {
        assert_eq!(sanitize_path("a/../../../etc/passwd"), "etc/passwd");
    }

    // --- Duplicate slashes ---

    #[test]
    fn duplicate_slashes_collapsed() {
        assert_eq!(sanitize_path("foo//bar"), "foo/bar");
    }

    #[test]
    fn triple_slashes_collapsed() {
        assert_eq!(sanitize_path("foo///bar///baz"), "foo/bar/baz");
    }

    // --- CVE-like adversarial inputs ---

    #[test]
    fn absolute_dotdot_traversal_blocked() {
        assert_eq!(sanitize_path("/../../etc/passwd"), "etc/passwd");
    }

    #[test]
    fn encoded_dotdot_not_collapsed() {
        // Literal "..%2f" is not a path separator - treated as normal component
        assert_eq!(sanitize_path("..%2fetc/passwd"), "..%2fetc/passwd");
    }

    #[test]
    fn deep_dotdot_traversal_blocked() {
        assert_eq!(
            sanitize_path("a/b/c/../../../../../../../../etc/shadow"),
            "etc/shadow"
        );
    }

    #[test]
    fn mixed_dot_and_dotdot() {
        assert_eq!(sanitize_path("./a/./b/../c/./d/../e"), "a/c/e");
    }

    #[test]
    fn only_dotdot_becomes_dot() {
        assert_eq!(sanitize_path(".."), ".");
    }

    #[test]
    fn many_dotdots_becomes_dot() {
        assert_eq!(sanitize_path("../../../.."), ".");
    }

    #[test]
    fn dotdot_with_trailing_slash() {
        assert_eq!(sanitize_path("a/b/../"), "a");
    }

    // --- keep_dot_dirs variant ---

    #[test]
    fn keep_dot_dirs_preserves_leading_dot_slash() {
        assert_eq!(sanitize_path_keep_dot_dirs("./foo"), "./foo");
    }

    #[test]
    fn keep_dot_dirs_still_strips_absolute() {
        assert_eq!(sanitize_path_keep_dot_dirs("/foo"), "foo");
    }

    #[test]
    fn keep_dot_dirs_still_collapses_dotdot() {
        assert_eq!(sanitize_path_keep_dot_dirs("../foo"), "foo");
    }

    #[test]
    fn keep_dot_dirs_preserves_interior_dots() {
        assert_eq!(sanitize_path_keep_dot_dirs("a/./b"), "a/./b");
    }

    // --- depth parameter ---

    #[test]
    fn depth_allows_leading_dotdots() {
        assert_eq!(sanitize_path_with_depth("../foo", 1, false), "../foo");
    }

    #[test]
    fn depth_limits_leading_dotdots() {
        assert_eq!(sanitize_path_with_depth("../../foo", 1, false), "../foo");
    }

    #[test]
    fn depth_zero_drops_all_leading_dotdots() {
        assert_eq!(sanitize_path_with_depth("../../foo", 0, false), "foo");
    }

    // --- Unicode paths ---

    #[test]
    fn unicode_path_preserved() {
        assert_eq!(sanitize_path("datos/archivo.txt"), "datos/archivo.txt");
    }

    #[test]
    fn unicode_with_dotdot() {
        assert_eq!(sanitize_path("datos/../secreto"), "secreto");
    }

    // --- Symlink target sanitization (daemon mode, flist.c:1154) ---

    #[test]
    fn symlink_target_absolute_stripped() {
        assert_eq!(sanitize_path("/etc/hosts"), "etc/hosts");
    }

    #[test]
    fn symlink_target_traversal_collapsed() {
        assert_eq!(sanitize_path("../../../etc/passwd"), "etc/passwd");
    }

    // --- files_from adversarial entries ---

    #[test]
    fn files_from_dotdot_traversal() {
        assert_eq!(
            sanitize_path_keep_dot_dirs("../../etc/shadow"),
            "etc/shadow"
        );
    }

    #[test]
    fn files_from_absolute_traversal() {
        assert_eq!(sanitize_path_keep_dot_dirs("/etc/shadow"), "etc/shadow");
    }

    #[test]
    fn files_from_mixed_traversal() {
        // ./a/../.. collapses: a backs up, then . backs up to root
        assert_eq!(
            sanitize_path_keep_dot_dirs("./a/../../etc/passwd"),
            "etc/passwd"
        );
    }
}
