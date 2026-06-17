use std::path::PathBuf;

use thiserror::Error;

use crate::local_copy::filter_program::{DirMergeOptions, ExcludeIfPresentRule};
use filters::FilterRule;

/// AST node produced by parsing a single line of a per-directory merge file.
#[derive(Debug)]
pub(crate) enum ParsedFilterDirective {
    /// A concrete filter rule (`+`, `-`, `include`, `exclude`, `show`, `hide`,
    /// `protect`, `risk`).
    Rule(FilterRule),
    /// A `merge` directive that pulls in another filter file eagerly,
    /// resolved against the enclosing file's parent directory.
    Merge {
        /// Path to the merged file, resolved relative to the enclosing file's
        /// parent directory unless absolute.
        path: PathBuf,
        /// Optional parser overrides (modifiers such as `,n`, `,e`, `,w`).
        /// `None` indicates the merged file inherits the current parser
        /// configuration unchanged.
        options: Option<DirMergeOptions>,
    },
    /// A `dir-merge` (or `:` short form, `per-dir`) directive that registers
    /// a per-directory merge filename to be looked up in each subdirectory
    /// visited beneath the enclosing scope.
    ///
    /// upstream: exclude.c:1419-1428 - `FILTRULE_PERDIR_MERGE` rules are added
    /// to the rule list with `parse_merge_name` rather than being expanded
    /// immediately. They fire when the receiver descends into a subdirectory
    /// that contains a matching file.
    DirMerge {
        /// Bare merge-file name (e.g. `.filt2`) as it appears in the directive,
        /// NOT a parent-relative path. The actual file is resolved against
        /// each subdirectory entered.
        pattern: PathBuf,
        /// Parser configuration for the registered per-directory merge rule.
        options: DirMergeOptions,
    },
    /// An `exclude-if-present` directive naming a marker file whose presence
    /// excludes the containing directory.
    ExcludeIfPresent(ExcludeIfPresentRule),
    /// A list-clearing directive (`!` or `clear`) that wipes inherited rules.
    Clear,
}

/// Error returned when a filter directive cannot be parsed.
#[derive(Debug, Error)]
#[error("{message}")]
pub(crate) struct FilterParseError {
    message: String,
}

impl FilterParseError {
    /// Constructs a new parse error from any value convertible to `String`.
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_parse_error_new_from_str() {
        let err = FilterParseError::new("test error");
        let display = format!("{err}");
        assert!(display.contains("test error"));
    }

    #[test]
    fn filter_parse_error_new_from_string() {
        let err = FilterParseError::new(String::from("error message"));
        let display = format!("{err}");
        assert!(display.contains("error message"));
    }

    #[test]
    fn filter_parse_error_debug() {
        let err = FilterParseError::new("debug test");
        let debug = format!("{err:?}");
        assert!(debug.contains("FilterParseError"));
    }

    #[test]
    fn parsed_filter_directive_rule_debug() {
        let directive = ParsedFilterDirective::Clear;
        let debug = format!("{directive:?}");
        assert!(debug.contains("Clear"));
    }

    #[test]
    fn parsed_filter_directive_merge_debug() {
        let directive = ParsedFilterDirective::Merge {
            path: PathBuf::from("/test"),
            options: None,
        };
        let debug = format!("{directive:?}");
        assert!(debug.contains("Merge"));
    }
}
