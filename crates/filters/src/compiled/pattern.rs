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
/// - It contains any `/` character (including trailing `/`)
///
/// upstream: exclude.c:195-209 counts slashes in the raw pattern BEFORE
/// stripping leading/trailing slashes. A pattern like `new/` has
/// slash_cnt=1, which triggers FILTRULE_ABS_PATH under XFLG_ABS_IF_SLASH.
pub(super) fn normalise_pattern(pattern: &str) -> (bool, bool, Cow<'_, str>) {
    let starts_with_slash = pattern.starts_with('/');
    let directory_only = pattern.ends_with('/');

    // upstream: exclude.c:195-198 - count slashes in the RAW pattern before
    // any stripping. This determines anchoring.
    let slash_count = pattern.chars().filter(|&c| c == '/').count();
    let anchored = slash_count > 0;

    if !starts_with_slash && !directory_only {
        return (anchored, false, Cow::Borrowed(pattern));
    }

    let start = if starts_with_slash { 1 } else { 0 };
    let end = if directory_only && pattern.len() > start {
        pattern.len() - 1
    } else {
        pattern.len()
    };

    if start == 0 && end == pattern.len() {
        (anchored, directory_only, Cow::Borrowed(pattern))
    } else {
        (
            anchored,
            directory_only,
            Cow::Owned(pattern[start..end].to_string()),
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
        // upstream: exclude.c:195-198 counts the trailing `/` as a slash,
        // so `foo/` has slash_cnt=1 and is anchored under XFLG_ABS_IF_SLASH.
        let (anchored, dir_only, core) = normalise_pattern("foo/");
        assert!(anchored);
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
        // Pattern contains internal slash, so it should be anchored
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
}
