use crate::local_copy::filter_program::{DirMergeOptions, ExcludeIfPresentRule};
use filters::FilterRule;
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

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

#[derive(Debug)]
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

impl fmt::Display for FilterParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for FilterParseError {}
