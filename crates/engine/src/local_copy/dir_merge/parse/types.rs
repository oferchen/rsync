use std::path::PathBuf;

use thiserror::Error;

use crate::local_copy::filter_program::{DirMergeOptions, ExcludeIfPresentRule};
use filters::FilterRule;

#[derive(Debug)]
pub(crate) enum ParsedFilterDirective {
    Rule(FilterRule),
    Merge {
        path: PathBuf,
        options: Option<DirMergeOptions>,
    },
    ExcludeIfPresent(ExcludeIfPresentRule),
    Clear,
}

#[derive(Debug, Error)]
#[error("{message}")]
pub(crate) struct FilterParseError {
    message: String,
}

impl FilterParseError {
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
        let display = format!("{}", err);
        assert!(display.contains("test error"));
    }

    #[test]
    fn filter_parse_error_new_from_string() {
        let err = FilterParseError::new(String::from("error message"));
        let display = format!("{}", err);
        assert!(display.contains("error message"));
    }

    #[test]
    fn filter_parse_error_debug() {
        let err = FilterParseError::new("debug test");
        let debug = format!("{:?}", err);
        assert!(debug.contains("FilterParseError"));
    }

    #[test]
    fn parsed_filter_directive_rule_debug() {
        // Just verify the enum can be created and debugged
        let directive = ParsedFilterDirective::Clear;
        let debug = format!("{:?}", directive);
        assert!(debug.contains("Clear"));
    }

    #[test]
    fn parsed_filter_directive_merge_debug() {
        let directive = ParsedFilterDirective::Merge {
            path: PathBuf::from("/test"),
            options: None,
        };
        let debug = format!("{:?}", directive);
        assert!(debug.contains("Merge"));
    }
}
