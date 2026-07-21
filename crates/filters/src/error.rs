//! Error types for filter rule compilation.

use thiserror::Error;

/// Error slot for filter-rule compilation, retained for API stability.
///
/// This is the error type of
/// [`FilterSet::from_rules`](crate::FilterSet::from_rules) and the
/// [`FilterSetError::Filter`](crate::FilterSetError::Filter) variant. Filter
/// compilation is infallible - matching is delegated to a byte-for-byte port
/// of `lib/wildmatch.c:dowild()`, and upstream rsync never rejects a filter
/// pattern (exclude.c:add_rule stores every pattern verbatim; a malformed
/// bracket expression fails to match rather than erroring). The type is kept
/// so the fallible signature remains source-compatible for callers.
#[derive(Debug, Error)]
#[error("failed to compile filter pattern '{pattern}': {source}")]
pub struct FilterError {
    pattern: String,
    #[source]
    source: globset::Error,
}

impl FilterError {
    /// Filter pattern that triggered this error.
    #[must_use]
    pub fn pattern(&self) -> &str {
        &self.pattern
    }
}
