use filters::FilterSet;
use protocol::iconv::FilenameConverter;

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

    /// Attaches a filename charset converter to the local-copy options.
    ///
    /// Mirrors the SSH/daemon path's
    /// `transfer::config::ConnectionConfig::iconv`, which is populated from
    /// `apply_common_server_flags`. The local-copy path bypasses that
    /// bridge, so this setter is the only route by which a converter
    /// resolved from
    /// `core::client::config::IconvSetting::resolve_converter` reaches the
    /// engine. Pass `None` to disable transcoding.
    ///
    /// This setter does not yet apply the converter on emit, ingest, or
    /// filter matching; those producer wirings are tracked under
    /// trackers #1912, #1913, and #1914 respectively.
    #[must_use]
    pub fn with_iconv(mut self, converter: Option<FilenameConverter>) -> Self {
        self.iconv = converter;
        self
    }

    /// Returns the configured filter set, if any.
    pub const fn filter_set(&self) -> Option<&FilterSet> {
        self.filters.as_ref()
    }

    /// Returns the configured filter program, if any.
    pub const fn filter_program(&self) -> Option<&FilterProgram> {
        self.filter_program.as_ref()
    }

    /// Returns the configured filename charset converter, if any.
    ///
    /// Returns `None` when `--iconv` was not set, when `--no-iconv` was set,
    /// or when the user-supplied charset spec failed to resolve to a
    /// supported `encoding_rs` encoding (in which case the resolver also
    /// emits a `tracing::warn!`).
    pub const fn iconv(&self) -> Option<&FilenameConverter> {
        self.iconv.as_ref()
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

    #[test]
    fn iconv_returns_none_by_default() {
        let options = LocalCopyOptions::new();
        assert!(options.iconv().is_none());
    }

    #[test]
    fn with_iconv_attaches_converter() {
        let converter = FilenameConverter::identity();
        let options = LocalCopyOptions::new().with_iconv(Some(converter.clone()));
        assert_eq!(options.iconv(), Some(&converter));
    }

    #[test]
    fn with_iconv_none_clears_converter() {
        let converter = FilenameConverter::identity();
        let options = LocalCopyOptions::new()
            .with_iconv(Some(converter))
            .with_iconv(None);
        assert!(options.iconv().is_none());
    }
}
