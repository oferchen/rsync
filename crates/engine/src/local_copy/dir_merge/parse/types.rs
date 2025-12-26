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
