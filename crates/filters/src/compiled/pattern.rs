use std::borrow::Cow;
use std::collections::HashSet;

use globset::{GlobBuilder, GlobMatcher};

use crate::FilterError;

/// Compiles a set of glob pattern strings into sorted, deduplicated matchers.
///
/// Patterns are sorted for deterministic evaluation order. Each pattern is
/// built with `literal_separator(true)` so that `*` does not match `/`,
/// matching upstream rsync's wildcard semantics.
pub(crate) fn compile_patterns(
    patterns: HashSet<String>,
    original: &str,
) -> Result<Vec<GlobMatcher>, FilterError> {
    let mut unique: Vec<_> = patterns.into_iter().collect();
    unique.sort();

    let mut matchers = Vec::with_capacity(unique.len());
    for pattern in unique {
        let glob = GlobBuilder::new(&pattern)
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .map_err(|error| FilterError::new(original.to_owned(), error))?;
        matchers.push(glob.compile_matcher());
    }
    Ok(matchers)
}

/// Normalizes a pattern by stripping leading `/` (anchored) and trailing `/` (directory-only).
///
/// Returns `Cow::Borrowed` when no stripping is needed (most common case),
/// avoiding a heap allocation.
///
/// A pattern is anchored if:
/// - It starts with `/`, OR
/// - It contains `/` anywhere in the pattern (besides trailing `/`)
///
/// A trailing `/***` suffix is treated as directory-only on the stem.
/// upstream: `exclude.c:936-937` - `FILTRULE_WILD3_SUFFIX` appends `/` to
/// directory names during matching, allowing `dir/***` to match both the
/// directory itself and all its contents. We normalize `dir/***` to `dir/`
/// (directory-only) so the standard descendant-matcher expansion produces
/// the correct `dir/**` content matchers.
///
/// This mirrors upstream rsync's pattern normalization in
/// `exclude.c:parse_filter_str()` where leading and trailing slashes are
/// stripped and used to set `FILTRULE_ABS_PATH` and `FILTRULE_DIRECTORY`
/// flags respectively.
pub(super) fn normalise_pattern(pattern: &str) -> (bool, bool, Cow<'_, str>) {
    let starts_with_slash = pattern.starts_with('/');

    // upstream: exclude.c:243-248 - a trailing `/***` (SLASH_WILD3_SUFFIX)
    // means "match both the directory and everything inside it". Normalize
    // by stripping `/***` and treating the result as directory-only.
    let (stripped, directory_only) = if pattern.ends_with("/***") && pattern.len() > 4 {
        // `/***` fully consumed - the stem has no trailing `/`.
        (&pattern[..pattern.len() - 4], true)
    } else if pattern.ends_with('/') && pattern.len() > 1 {
        (&pattern[..pattern.len() - 1], true)
    } else if pattern == "/" {
        (pattern, true)
    } else {
        (pattern, false)
    };

    // Strip the leading `/` if present.
    let core_pattern = if starts_with_slash {
        stripped.strip_prefix('/').unwrap_or(stripped)
    } else {
        stripped
    };

    // upstream: exclude.c:rule_matches() - FILTRULE_ABS_PATH is only set
    // for patterns that start with `/` (or when XFLG_ABS_IF_SLASH is in
    // effect, which is restricted to daemon module configs). A pattern
    // with internal slashes but no leading `/` is NOT anchored; instead
    // upstream tail-matches it against the last N+1 path components (line
    // 947-951). The glob equivalent is `**/pattern`, which our caller
    // adds for unanchored patterns.
    let anchored = starts_with_slash;

    if !starts_with_slash && !directory_only {
        // Nothing was stripped - borrow the original.
        (anchored, false, Cow::Borrowed(pattern))
    } else {
        (
            anchored,
            directory_only,
            Cow::Owned(core_pattern.to_string()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_pattern_plain() {
        let (anchored, dir_only, core) = normalise_pattern("foo");
        assert!(!anchored);
        assert!(!dir_only);
        assert_eq!(core, "foo");
    }

    #[test]
    fn normalise_pattern_anchored() {
        let (anchored, dir_only, core) = normalise_pattern("/foo");
        assert!(anchored);
        assert!(!dir_only);
        assert_eq!(core, "foo");
    }

    #[test]
    fn normalise_pattern_directory_only() {
        let (anchored, dir_only, core) = normalise_pattern("foo/");
        assert!(!anchored);
        assert!(dir_only);
        assert_eq!(core, "foo");
    }

    #[test]
    fn normalise_pattern_anchored_directory() {
        let (anchored, dir_only, core) = normalise_pattern("/foo/");
        assert!(anchored);
        assert!(dir_only);
        assert_eq!(core, "foo");
    }

    #[test]
    fn normalise_pattern_wildcard() {
        let (anchored, dir_only, core) = normalise_pattern("*.txt");
        assert!(!anchored);
        assert!(!dir_only);
        assert_eq!(core, "*.txt");
    }

    #[test]
    fn normalise_pattern_anchored_wildcard() {
        let (anchored, dir_only, core) = normalise_pattern("/*.txt");
        assert!(anchored);
        assert!(!dir_only);
        assert_eq!(core, "*.txt");
    }

    #[test]
    fn normalise_pattern_nested_path() {
        let (anchored, dir_only, core) = normalise_pattern("src/lib/");
        // upstream: internal slashes without a leading `/` are NOT anchored;
        // they use tail-matching (match last N+1 path components via `**/pattern`).
        assert!(!anchored);
        assert!(dir_only);
        assert_eq!(core, "src/lib");
    }

    #[test]
    fn normalise_pattern_anchored_nested_path() {
        // Leading `/` anchors even with internal slashes.
        let (anchored, dir_only, core) = normalise_pattern("/src/lib/");
        assert!(anchored);
        assert!(dir_only);
        assert_eq!(core, "src/lib");
    }

    #[test]
    fn normalise_pattern_empty_after_strip() {
        // Edge case: pattern is just "/"
        let (anchored, dir_only, core) = normalise_pattern("/");
        assert!(anchored);
        assert!(dir_only);
        // Core is empty but we don't strip further because it would be empty
        assert_eq!(core, "");
    }

    /// upstream: exclude.c:936-937 - FILTRULE_WILD3_SUFFIX appends `/` to
    /// directory names during matching. `dir/***` matches both the directory
    /// itself (when is_dir) and everything inside it.
    #[test]
    fn normalise_pattern_wild3_suffix() {
        let (anchored, dir_only, core) = normalise_pattern("new/lose/***");
        assert!(!anchored);
        assert!(dir_only);
        assert_eq!(core, "new/lose");
    }

    #[test]
    fn normalise_pattern_anchored_wild3_suffix() {
        let (anchored, dir_only, core) = normalise_pattern("/new/lose/***");
        assert!(anchored);
        assert!(dir_only);
        assert_eq!(core, "new/lose");
    }

    /// Bare `/***` (no directory stem) should be treated as directory-only
    /// on the empty path, matching upstream behavior.
    #[test]
    fn normalise_pattern_bare_wild3_suffix() {
        // Pattern "/***" has len 4, not > 4, so the `/***` branch does NOT
        // fire. This is by design: bare `***` is just a wildcard pattern.
        let (anchored, dir_only, core) = normalise_pattern("/***");
        assert!(anchored);
        assert!(!dir_only);
        assert_eq!(core, "***");
    }

    /// Pattern ending with `***` but without a preceding `/` is a regular
    /// wildcard, not the WILD3_SUFFIX semantic.
    #[test]
    fn normalise_pattern_trailing_triple_star_no_slash() {
        let (anchored, dir_only, core) = normalise_pattern("foo***");
        assert!(!anchored);
        assert!(!dir_only);
        assert_eq!(core, "foo***");
    }
}
