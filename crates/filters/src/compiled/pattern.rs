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
/// This mirrors upstream rsync's pattern normalization in
/// `exclude.c:parse_filter_str()` where leading and trailing slashes are
/// stripped and used to set `FILTRULE_ABS_PATH` and `FILTRULE_DIRECTORY`
/// flags respectively.
pub(super) fn normalise_pattern(pattern: &str) -> (bool, bool, Cow<'_, str>) {
    let starts_with_slash = pattern.starts_with('/');
    let directory_only = pattern.ends_with('/');

    let core_pattern = if directory_only && pattern.len() > 1 {
        &pattern[..pattern.len() - 1]
    } else {
        pattern
    };

    let core_pattern_no_leading = if starts_with_slash && core_pattern.len() > 1 {
        &core_pattern[1..]
    } else {
        core_pattern
    };

    let has_internal_slash = core_pattern_no_leading.contains('/');
    let anchored = starts_with_slash || has_internal_slash;

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
