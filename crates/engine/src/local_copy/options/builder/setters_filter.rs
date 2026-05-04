//! Setter methods for filter and exclusion options.

use filters::FilterSet;
use protocol::iconv::FilenameConverter;

use super::LocalCopyOptionsBuilder;
use crate::local_copy::filter_program::FilterProgram;

impl LocalCopyOptionsBuilder {
    /// Sets the filter set.
    #[must_use]
    pub fn filters(mut self, filters: Option<FilterSet>) -> Self {
        self.filters = filters;
        self
    }

    /// Sets the filter program.
    #[must_use]
    pub fn filter_program(mut self, program: Option<FilterProgram>) -> Self {
        self.filter_program = program;
        self
    }

    /// Sets the optional filename charset converter resolved from
    /// `--iconv=LOCAL,REMOTE`.
    ///
    /// Pass `None` to disable transcoding (the default). When `Some`, the
    /// converter is propagated to
    /// [`LocalCopyOptions::iconv`](crate::local_copy::LocalCopyOptions::iconv)
    /// for later use by file-list emit, file-list ingest, and filter
    /// matching once those producer wirings land (#1912, #1913, #1914).
    #[must_use]
    pub fn iconv(mut self, converter: Option<FilenameConverter>) -> Self {
        self.iconv = converter;
        self
    }
}
