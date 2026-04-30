//! AppleDouble (`._foo`) sidecar exclusion patterns for macOS interoperability.
//!
//! When macOS writes files to a non-HFS+/APFS filesystem (such as a network
//! share, FAT32, or many cross-platform mounts) it stores Finder metadata,
//! resource forks, and extended attributes in a companion file whose name is
//! the original filename prefixed with `._`. These AppleDouble sidecars are
//! useful only on the Mac that produced them; replicating them onto a Linux
//! receiver or back into a fresh macOS xattr space simply litters the
//! destination with stale metadata. This module supplies the patterns used by
//! the `--apple-double-skip` option to filter them out.
//!
//! # Default Patterns
//!
//! The single canonical pattern is `._*`. It is unanchored so that AppleDouble
//! sidecars are excluded at any directory depth.
//!
//! # Perishable Rules
//!
//! Like the CVS exclusions, AppleDouble exclusions are marked as perishable
//! when running on rsync protocol version 30 or higher. Perishable rules can
//! be overridden by an explicit include rule placed earlier in the filter
//! chain, mirroring the precedence semantics used for `--cvs-exclude`.
//!
//! # References
//!
//! - Apple Technical Note TN2078: AppleDouble sidecar layout on non-HFS volumes
//! - upstream: exclude.c (filter list scaffolding reused for built-in patterns)

/// Default AppleDouble exclusion pattern: any file whose name begins with `._`.
///
/// The pattern is unanchored so the rule fires at every directory depth, which
/// matches the behaviour users expect when they enable `--apple-double-skip`.
pub const DEFAULT_APPLE_DOUBLE_PATTERN: &str = "._*";

/// Returns an iterator over the default AppleDouble exclusion patterns.
///
/// The list is intentionally tiny - only `._*` - but the iterator shape mirrors
/// [`crate::cvs::default_patterns`] so callers can treat both built-in pattern
/// sets uniformly.
///
/// # Examples
///
/// ```
/// use filters::apple_double::default_patterns;
///
/// let patterns: Vec<&str> = default_patterns().collect();
/// assert_eq!(patterns, vec!["._*"]);
/// ```
pub fn default_patterns() -> impl Iterator<Item = &'static str> {
    std::iter::once(DEFAULT_APPLE_DOUBLE_PATTERN)
}

/// Returns the number of default AppleDouble exclusion patterns.
pub fn pattern_count() -> usize {
    default_patterns().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_patterns_contains_apple_double() {
        let patterns: Vec<&str> = default_patterns().collect();
        assert!(patterns.contains(&"._*"));
    }

    #[test]
    fn default_patterns_count_is_one() {
        assert_eq!(pattern_count(), 1);
    }

    #[test]
    fn default_patterns_no_empty_entries() {
        for pattern in default_patterns() {
            assert!(!pattern.is_empty(), "found empty pattern");
            assert!(
                !pattern.contains(' '),
                "pattern contains whitespace: {pattern:?}"
            );
        }
    }

    #[test]
    fn default_apple_double_pattern_constant_matches_iterator() {
        let patterns: Vec<&str> = default_patterns().collect();
        assert_eq!(patterns[0], DEFAULT_APPLE_DOUBLE_PATTERN);
    }
}
