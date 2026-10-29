//! Constants shared across the CLI front-end.

use time::{format_description::FormatItem, macros::format_description};

/// Format string used for `--itemize-changes` output.
pub(super) const ITEMIZE_CHANGES_FORMAT: &str = "%i %n%L";

/// Default patterns excluded by `--apple-double-skip` (macOS AppleDouble sidecars).
///
/// These files (`._foo`) carry FinderInfo, resource forks, and extended
/// attributes for files on filesystems that cannot represent them natively.
/// They are rarely useful when transferred to other systems and clutter
/// destinations.
pub(super) const APPLE_DOUBLE_EXCLUDE_PATTERNS: &[&str] = &["._*"];

/// Timestamp format used for `--list-only` and `--out-format` placeholders.
pub(crate) const LIST_TIMESTAMP_FORMAT: &[FormatItem<'static>] = format_description!(
    "[year]/[month padding:zero]/[day padding:zero] [hour padding:zero]:[minute padding:zero]:[second padding:zero]"
);

#[cfg(test)]
#[allow(clippy::const_is_empty)]
mod tests {
    use super::*;

    #[test]
    fn itemize_changes_format_valid() {
        assert!(ITEMIZE_CHANGES_FORMAT.contains("%i"));
        assert!(ITEMIZE_CHANGES_FORMAT.contains("%n"));
    }

    #[test]
    fn list_timestamp_format_not_empty() {
        assert!(!LIST_TIMESTAMP_FORMAT.is_empty());
    }

    #[test]
    fn apple_double_exclude_patterns_not_empty() {
        assert!(!APPLE_DOUBLE_EXCLUDE_PATTERNS.is_empty());
    }

    #[test]
    fn apple_double_exclude_patterns_contains_dot_underscore() {
        assert!(APPLE_DOUBLE_EXCLUDE_PATTERNS.contains(&"._*"));
    }
}
