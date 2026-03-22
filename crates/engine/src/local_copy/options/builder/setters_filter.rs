//! Setter methods for filter and exclusion options.

use filters::FilterSet;

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
}
