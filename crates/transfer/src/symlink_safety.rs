#![deny(unsafe_code)]
//! Symlink target safety analysis.
//!
//! Determines whether a symlink target would escape the transfer tree,
//! mirroring upstream rsync's `util1.c:unsafe_symlink()` algorithm.
//!
//! Used by:
//! - **Sender** (`--copy-unsafe-links`): follow unsafe symlinks as regular files
//! - **Receiver** (`--safe-links`): skip creating unsafe symlinks at destination
//!
//! # Upstream Reference
//!
//! - `util1.c:1329` - `unsafe_symlink(dest, src)`

use std::ffi::OsStr;

/// Returns `true` if `target` would escape the transfer tree when resolved
/// relative to `link_path`.
///
/// A symlink is considered unsafe when:
/// 1. The target is empty or absolute
/// 2. The target contains `/../` after any leading `../` prefix - this prevents
///    canonicalization attacks where intermediate path components could be
///    replaced with symlinks (upstream 3.4.1 security hardening)
/// 3. The target ends with `/..` after non-leading content
/// 4. Walking the target's `..` components would escape above the link's
///    directory depth in the transfer tree
///
/// # Upstream Reference
///
/// - `util1.c:1329` - `unsafe_symlink(dest, src)`
/// - Comment: "reject names such as a/b/../x/y. This needs to be done as
///   the leading subpaths 'a' or 'b' could later be replaced with symlinks
///   such as a link to '.' resulting in the link being transferred now
///   becoming unsafe"
pub fn is_unsafe_symlink(target: &OsStr, link_path: &std::path::Path) -> bool {
    let target_bytes = os_str_as_bytes(target);

    // upstream: "all absolute and null symlinks are unsafe"
    if target_bytes.is_empty() || target_bytes[0] == b'/' {
        return true;
    }

    // upstream 3.4.1: reject /../ mid-path after skipping leading ../
    // segments. Prevents canonicalization attacks where intermediate
    // directories could be symlinks themselves.
    if has_mid_path_dotdot(&target_bytes) {
        return true;
    }

    // upstream: util1.c:1356-1383
    let depth = compute_link_depth(link_path);
    !is_target_within_depth(&target_bytes, depth)
}

/// Computes the directory depth budget for a symlink within the transfer tree.
///
/// The link's filename itself is not a directory level, so the final component
/// is excluded from the count. A `..` component resets depth to zero.
///
/// # Upstream Reference
///
/// - `util1.c:1356-1366` - source path depth computation
fn compute_link_depth(link_path: &std::path::Path) -> i64 {
    use std::path::Component;

    let mut depth: i64 = 0;
    for component in link_path.components() {
        match component {
            Component::Normal(_) => depth += 1,
            // upstream: ".." segment starts the count over
            Component::ParentDir => depth = 0,
            _ => {}
        }
    }
    // Exclude the symlink filename from the depth budget.
    (depth - 1).max(0)
}

/// Checks whether the target path stays within the given depth budget.
///
/// Walks each path segment: `..` decrements depth, normal segments increment.
/// Returns `false` if depth ever goes negative (target escapes the tree).
///
/// # Upstream Reference
///
/// - `util1.c:1368-1383` - destination path depth check
fn is_target_within_depth(target: &[u8], mut depth: i64) -> bool {
    for segment in ByteSegments::new(target) {
        if segment == b".." {
            depth -= 1;
            if depth < 0 {
                return false;
            }
        } else if segment != b"." {
            depth += 1;
        }
    }
    depth >= 0
}

/// Detects `/../` or trailing `/..` appearing after any leading `../` prefix.
///
/// Upstream 3.4.1 added this as a security hardening measure: intermediate
/// path components (e.g., `a` in `a/../../../etc`) could themselves be
/// symlinks, making the depth-tracking check insufficient on its own.
///
/// # Upstream Reference
///
/// - `util1.c:1338-1353` - mid-path dotdot rejection
fn has_mid_path_dotdot(target: &[u8]) -> bool {
    // Skip leading ../ segments (and redundant slashes between them).
    let mut pos = 0;
    while pos + 2 < target.len()
        && target[pos] == b'.'
        && target[pos + 1] == b'.'
        && target[pos + 2] == b'/'
    {
        pos += 3;
        while pos < target.len() && target[pos] == b'/' {
            pos += 1;
        }
    }
    // Handle the case where target is exactly ".." (no trailing slash).
    // This is just leading ../, not mid-path - skip it.
    if pos == 0 && target == b".." {
        return false;
    }

    let rest = &target[pos..];

    // upstream: `strstr(dest2, "/../")`
    if contains_subsequence(rest, b"/../") {
        return true;
    }

    // upstream: `dlen > 3 && strcmp(&dest[dlen-3], "/..") == 0`
    if rest.len() >= 3 && rest.ends_with(b"/..") {
        return true;
    }

    false
}

/// Checks whether `haystack` contains `needle` as a contiguous subsequence.
fn contains_subsequence(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Iterator over `/`-separated path segments, skipping empty segments from
/// consecutive slashes. Mirrors upstream's `strchr(name, '/')` loop.
struct ByteSegments<'a> {
    remaining: &'a [u8],
}

impl<'a> ByteSegments<'a> {
    fn new(path: &'a [u8]) -> Self {
        Self { remaining: path }
    }
}

impl<'a> Iterator for ByteSegments<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        // Skip leading slashes.
        while self.remaining.first() == Some(&b'/') {
            self.remaining = &self.remaining[1..];
        }
        if self.remaining.is_empty() {
            return None;
        }
        match self.remaining.iter().position(|&b| b == b'/') {
            Some(slash) => {
                let segment = &self.remaining[..slash];
                self.remaining = &self.remaining[slash + 1..];
                Some(segment)
            }
            None => {
                let segment = self.remaining;
                self.remaining = &[];
                Some(segment)
            }
        }
    }
}

/// Converts an `OsStr` to bytes for path analysis.
///
/// Returns an owned `Vec<u8>` for a consistent return type across platforms.
/// This is not a hot path - called once per symlink during transfer.
fn os_str_as_bytes(s: &OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        s.as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        s.to_string_lossy().as_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn unsafe_link(target: &str, link: &str) -> bool {
        is_unsafe_symlink(OsStr::new(target), Path::new(link))
    }

    #[test]
    fn absolute_target_is_unsafe() {
        assert!(unsafe_link("/etc/passwd", "a/link"));
    }

    #[test]
    fn empty_target_is_unsafe() {
        assert!(unsafe_link("", "a/link"));
    }

    #[test]
    fn sibling_file_is_safe() {
        assert!(!unsafe_link("hello.txt", "dir/link"));
    }

    #[test]
    fn child_path_is_safe() {
        assert!(!unsafe_link("sub/file.txt", "dir/link"));
    }

    #[test]
    fn dotdot_within_depth_is_safe() {
        // link at a/b/link, target ../file -> a/file (still in tree)
        assert!(!unsafe_link("../file", "a/b/link"));
    }

    #[test]
    fn dotdot_escaping_tree_is_unsafe() {
        // link at a/link, target ../../etc -> escapes
        assert!(unsafe_link("../../etc", "a/link"));
    }

    #[test]
    fn deep_dotdot_escape_is_unsafe() {
        assert!(unsafe_link("../../../etc/passwd", "a/b/link"));
    }

    #[test]
    fn mid_path_dotdot_is_unsafe() {
        // a/b/../../../etc - the /../ after non-leading content is rejected
        assert!(unsafe_link("a/b/../../../etc", "deep/nested/dir/link"));
    }

    #[test]
    fn trailing_dotdot_is_unsafe() {
        // a/b/.. - trailing /.. after non-leading content
        assert!(unsafe_link("a/b/..", "dir/link"));
    }

    #[test]
    fn leading_dotdot_not_rejected_by_mid_path_check() {
        // ../../file - only leading ../, no mid-path /../
        // Still checked by depth tracking
        assert!(!unsafe_link("../../file", "a/b/c/link"));
    }

    #[test]
    fn upstream_test_simple_relative() {
        assert!(!unsafe_link("foo", "bar/baz"));
    }

    #[test]
    fn upstream_test_dotdot_safe() {
        assert!(!unsafe_link("../foo", "bar/baz"));
    }

    #[test]
    fn upstream_test_dotdot_unsafe() {
        assert!(unsafe_link("../../foo", "bar/baz"));
    }

    #[test]
    fn upstream_test_deep_path_safe() {
        assert!(!unsafe_link("../../../foo", "a/b/c/d/link"));
    }

    #[test]
    fn top_level_link_dotdot_is_unsafe() {
        // link at top level, any .. escapes
        assert!(unsafe_link("../foo", "link"));
    }

    #[test]
    fn dot_segment_is_safe() {
        assert!(!unsafe_link("./foo", "dir/link"));
    }

    #[test]
    fn mid_path_dotdot_with_leading_dotdot() {
        // ../a/../../../etc - has /../ after non-leading content
        assert!(unsafe_link("../a/../../../etc", "a/b/c/link"));
    }

    #[test]
    fn pure_leading_dotdot_chain_no_mid_path() {
        // ../../../file - only leading ../, no mid-path /../
        assert!(unsafe_link("../../../file", "a/b/link"));
    }

    #[test]
    fn consecutive_slashes_handled() {
        assert!(!unsafe_link("foo//bar", "dir/link"));
    }
}
