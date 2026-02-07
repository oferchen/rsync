use thiserror::Error;

/// Error produced when a [`FilterRule`](crate::FilterRule) cannot be compiled
/// into a glob matcher.
///
/// This error is returned by [`FilterSet::from_rules`](crate::FilterSet::from_rules)
/// when a pattern expands to an invalid glob expression. The error retains the
/// offending pattern and the underlying [`globset::Error`] for diagnostics.
#[derive(Debug, Error)]
#[error("failed to compile filter pattern '{pattern}': {source}")]
pub struct FilterError {
    pattern: String,
    #[source]
    source: globset::Error,
}

impl FilterError {
    /// Creates a new [`FilterError`] for the given pattern and source error.
    pub(crate) const fn new(pattern: String, source: globset::Error) -> Self {
        Self { pattern, source }
    }

    /// Returns the offending pattern.
    #[must_use]
    pub fn pattern(&self) -> &str {
        &self.pattern
    }
}

#[cfg(test)]
mod tests {
    use super::FilterError;
    use globset::GlobBuilder;
    use std::error::Error as _;

    #[test]
    fn filter_error_preserves_pattern_and_source() {
        let glob_err = GlobBuilder::new("[").build().unwrap_err();
        let error = FilterError::new("[".into(), glob_err.clone());

        assert_eq!(error.pattern(), "[");
        assert!(error.to_string().contains("failed to compile"));
        assert!(error.source().is_some());
        assert_eq!(error.source().unwrap().to_string(), glob_err.to_string());
    }
}
