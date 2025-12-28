use filters::FilterSet;

use super::types::LocalCopyOptions;
use crate::local_copy::filter_program::FilterProgram;

impl LocalCopyOptions {
    /// Applies a precompiled filter set to the execution.
    #[must_use]
    pub fn with_filters(mut self, filters: Option<FilterSet>) -> Self {
        self.filters = filters;
        self
    }

    /// Applies a filter set using the legacy builder name for compatibility.
    #[must_use]
    pub fn filters(self, filters: Option<FilterSet>) -> Self {
        self.with_filters(filters)
    }

    /// Applies an external filter program configuration.
    #[must_use]
    pub fn with_filter_program(mut self, program: Option<FilterProgram>) -> Self {
        self.filter_program = program;
        self
    }

    /// Returns the configured filter set, if any.
    #[must_use]
    pub const fn filter_set(&self) -> Option<&FilterSet> {
        self.filters.as_ref()
    }

    /// Returns the configured filter program, if any.
    #[must_use]
    pub const fn filter_program(&self) -> Option<&FilterProgram> {
        self.filter_program.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_filters_none() {
        let options = LocalCopyOptions::new().with_filters(None);
        assert!(options.filter_set().is_none());
    }

    #[test]
    fn filters_alias_works() {
        let options = LocalCopyOptions::new().filters(None);
        assert!(options.filter_set().is_none());
    }

    #[test]
    fn with_filter_program_none() {
        let options = LocalCopyOptions::new().with_filter_program(None);
        assert!(options.filter_program().is_none());
    }

    #[test]
    fn filter_set_returns_none_by_default() {
        let options = LocalCopyOptions::new();
        assert!(options.filter_set().is_none());
    }

    #[test]
    fn filter_program_returns_none_by_default() {
        let options = LocalCopyOptions::new();
        assert!(options.filter_program().is_none());
    }
}
