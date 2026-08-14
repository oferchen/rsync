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
//! - `util1.c:1138` - `sanitize_path(dest, p, rootdir, depth, flags)`
//! - `flist.c:774` - applied to received file names in daemon mode
//! - `flist.c:2299` - applied to `--files-from` entries
//! - `flist.c:1182` - applied to symlink targets when `sanitize_paths && !munge_symlinks`

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
/// - `util1.c:1138-1211` - `sanitize_path()`
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
/// - `flist.c:2299` - `sanitize_path(fbuf, fbuf, "", 0, SP_KEEP_DOT_DIRS)`
pub fn sanitize_path_keep_dot_dirs(path: &str) -> String {
    sanitize_path_with_depth(path, 0, true)
}

/// Byte-exact variant of [`sanitize_path_keep_dot_dirs`].
///
/// Used for `--files-from` entries whose names may not be valid UTF-8. A
/// filename is a raw byte string on Unix (upstream carries it as `char*`),
/// so the sanitizer must preserve every non-separator byte verbatim rather
/// than route through `String`, which would drop non-UTF-8 names. The
/// traversal-guard logic is identical to the `&str` path - both delegate to
/// [`sanitize_path_bytes`], which only ever inspects the ASCII `/` and `.`
/// bytes.
///
/// # Upstream Reference
///
/// - `flist.c:2299` - `sanitize_path(fbuf, fbuf, "", 0, SP_KEEP_DOT_DIRS)`
pub fn sanitize_path_keep_dot_dirs_bytes(path: &[u8]) -> Vec<u8> {
    sanitize_path_bytes(path, 0, true)
}

/// Byte-exact variant of [`sanitize_path`] (`SP_DEFAULT`).
///
/// Used for symlink targets received from the network, which are raw byte
/// strings on the wire (upstream carries them as `char*`) and need not be
/// valid UTF-8. Routing them through `String` would replace non-UTF-8 bytes
/// and rewrite the very value the sanitizer exists to make safe, so this
/// entry point stays on `&[u8]` end to end. Shares
/// [`sanitize_path_bytes`] with the `&str` form, so the traversal guard
/// cannot diverge between the two.
///
/// # Upstream Reference
///
/// - `flist.c:1182` - `sanitize_path(bp, bp, "", lastdir_depth, SP_DEFAULT)`
///   applied to a received symlink target when `sanitize_paths &&
///   !munge_symlinks`.
pub fn sanitize_path_bytes_default(path: &[u8]) -> Vec<u8> {
    sanitize_path_bytes(path, 0, false)
}

/// Core sanitization with configurable depth budget and dot-dir handling.
///
/// - `depth`: number of leading `..` segments allowed (0 for daemon mode)
/// - `keep_dot_dirs`: when true, preserves leading `./` (SP_KEEP_DOT_DIRS)
///
/// The `&str` input is valid UTF-8 and [`sanitize_path_bytes`] only rewrites
/// ASCII separators/dot components, so the result is always valid UTF-8.
///
/// # Upstream Reference
///
/// - `util1.c:1138-1211`
fn sanitize_path_with_depth(path: &str, depth: i32, keep_dot_dirs: bool) -> String {
    let out = sanitize_path_bytes(path.as_bytes(), depth, keep_dot_dirs);
    String::from_utf8(out).unwrap_or_else(|_| ".".to_string())
}

/// Byte-level core of [`sanitize_path_with_depth`].
///
/// Operates purely on the raw bytes, inspecting only the ASCII `/` (0x2F)
/// separator and `.` (0x2E) dot bytes; every other byte is copied through
/// verbatim, so non-UTF-8 filenames survive intact. This is the single
/// implementation of the traversal guard - the `&str` and `&[u8]` public
/// entry points both delegate here so the security behaviour cannot diverge.
///
/// # Upstream Reference
///
/// - `util1.c:1138-1211`
fn sanitize_path_bytes(bytes: &[u8], mut depth: i32, keep_dot_dirs: bool) -> Vec<u8> {
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
        // upstream: util1.c:1103 - an empty result becomes ".".
        b".".to_vec()
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn absolute_path_stripped() {
        assert_eq!(sanitize_path("/etc/passwd"), "etc/passwd");
    }

    #[test]
    fn root_only_becomes_dot() {
        assert_eq!(sanitize_path("/"), ".");
    }

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

    /// Every upstream `clean_fname(name, CFN_COLLAPSE_DOT_DOT_DIRS)` case must
    /// collapse identically through `sanitize_path`.
    ///
    /// `sanitize_path` is the daemon-side traversal guard: it is what turns a
    /// peer-supplied name into the path actually opened under the module root.
    /// It shares its `..`-collapse rule with upstream's `clean_fname`, and
    /// upstream shipped an off-by-one there that left the collapse dead for
    /// every multi-component and absolute path. A collapse that silently does
    /// nothing means the retained name still carries `..` segments into the
    /// open, so this is a confinement invariant, not formatting.
    ///
    /// `sanitize_path` additionally strips the leading `/` (that is the
    /// `sanitize_path`-vs-`clean_fname` difference, not a collapse difference),
    /// so the absolute row's expectation is de-anchored before comparison.
    /// The table is shared (`test_support::COLLAPSE_CASES`) so a new edge case
    /// is one row and reaches every oc-rsync copy of this rule at once.
    // upstream: util1.c clean_fname() CFN_COLLAPSE_DOT_DOT_DIRS; t_clean_fname.c
    #[test]
    fn upstream_collapse_cases_consume_the_preceding_component() {
        for (input, expected) in test_support::COLLAPSE_CASES {
            assert_eq!(
                sanitize_path(input),
                expected.trim_start_matches('/'),
                "clean_fname collapse case {input:?}"
            );
        }
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

    #[test]
    fn duplicate_slashes_collapsed() {
        assert_eq!(sanitize_path("foo//bar"), "foo/bar");
    }

    #[test]
    fn triple_slashes_collapsed() {
        assert_eq!(sanitize_path("foo///bar///baz"), "foo/bar/baz");
    }

    // Parity with upstream rsync 3.4.2 "removal of multiple leading slashes"
    // fix (commit d4c4f67, NEWS.md:71). Pins the leading-slash collapse
    // behaviour against any future regression to a pair-replace strategy.
    // upstream: util1.c sanitize_path leading-slash skip + main-loop discard
    #[test]
    fn leading_double_slash_collapsed() {
        assert_eq!(sanitize_path("//foo"), "foo");
    }

    #[test]
    fn leading_triple_slash_collapsed() {
        assert_eq!(sanitize_path("///foo"), "foo");
    }

    #[test]
    fn leading_quintuple_slash_collapsed() {
        assert_eq!(sanitize_path("/////bar"), "bar");
    }

    #[test]
    fn leading_slashes_with_interior_double_slash() {
        assert_eq!(sanitize_path("///foo//bar"), "foo/bar");
    }

    #[test]
    fn all_slashes_becomes_dot() {
        assert_eq!(sanitize_path("/////"), ".");
    }

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

    #[test]
    fn unicode_path_preserved() {
        assert_eq!(sanitize_path("datos/archivo.txt"), "datos/archivo.txt");
    }

    #[test]
    fn unicode_with_dotdot() {
        assert_eq!(sanitize_path("datos/../secreto"), "secreto");
    }

    #[test]
    fn symlink_target_absolute_stripped() {
        assert_eq!(sanitize_path("/etc/hosts"), "etc/hosts");
    }

    #[test]
    fn symlink_target_traversal_collapsed() {
        assert_eq!(sanitize_path("../../../etc/passwd"), "etc/passwd");
    }

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

    // --- Byte-level entry point: preservation + traversal guard parity ---
    //
    // `--files-from` names may be non-UTF-8 (a legal Unix filename), so the
    // sender sanitizes them through `sanitize_path_keep_dot_dirs_bytes`. These
    // tests pin two invariants: (1) non-separator bytes (including 0xFF and a
    // literal backslash) round-trip verbatim, and (2) the traversal guard is
    // byte-identical to the `&str` path - `../` and absolute escapes are still
    // collapsed even when the surviving component carries non-UTF-8 bytes.

    #[test]
    fn bytes_preserves_non_utf8_component() {
        assert_eq!(
            sanitize_path_keep_dot_dirs_bytes(b"foo\xffbar.txt"),
            b"foo\xffbar.txt".to_vec()
        );
    }

    #[test]
    fn bytes_preserves_backslash_component() {
        // A backslash is an ordinary filename byte on Unix, not a separator.
        assert_eq!(
            sanitize_path_keep_dot_dirs_bytes(b"back\\slash.txt"),
            b"back\\slash.txt".to_vec()
        );
    }

    #[test]
    fn bytes_matches_str_path_for_ascii() {
        // Same input, both entry points: identical bytes out.
        let ascii = "a/./b/../c";
        assert_eq!(
            sanitize_path_keep_dot_dirs_bytes(ascii.as_bytes()),
            sanitize_path_keep_dot_dirs(ascii).into_bytes()
        );
    }

    #[test]
    fn bytes_dotdot_traversal_blocked_with_non_utf8() {
        // `../` escape is collapsed to the root even though the retained
        // component (`\xff\xfe`) is not valid UTF-8. The guard must not be
        // weakened for non-UTF-8 input.
        assert_eq!(
            sanitize_path_keep_dot_dirs_bytes(b"../../\xff\xfe/passwd"),
            b"\xff\xfe/passwd".to_vec()
        );
    }

    #[test]
    fn bytes_absolute_traversal_stripped_with_non_utf8() {
        // Leading `/` is stripped (absolute -> relative) with a non-UTF-8 tail.
        assert_eq!(
            sanitize_path_keep_dot_dirs_bytes(b"/\xff\xfe/secret"),
            b"\xff\xfe/secret".to_vec()
        );
    }

    #[test]
    fn bytes_interior_dotdot_collapses_non_utf8_component() {
        // `a/b/../c` collapses `b`, preserving the non-UTF-8 bytes in `a`/`c`.
        assert_eq!(
            sanitize_path_keep_dot_dirs_bytes(b"\xffa/b/../\xfec"),
            b"\xffa/\xfec".to_vec()
        );
    }

    #[test]
    fn bytes_deep_dotdot_cannot_escape_root_non_utf8() {
        assert_eq!(
            sanitize_path_keep_dot_dirs_bytes(b"a/../../../../\xff/shadow"),
            b"\xff/shadow".to_vec()
        );
    }
}
