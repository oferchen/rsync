//! Constants shared across the CLI front-end.

use time::{format_description::FormatItem, macros::format_description};

/// Format string used for `--itemize-changes` output.
pub(super) const ITEMIZE_CHANGES_FORMAT: &str = "%i %n%L";

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
}
