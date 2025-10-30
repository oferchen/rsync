use std::fmt;

/// Error produced when a rule cannot be compiled into a matcher.
#[derive(Debug)]
pub struct FilterError {
    pattern: String,
    source: globset::Error,
}

impl FilterError {
    /// Creates a new [`FilterError`] for the given pattern and source error.
    pub(crate) fn new(pattern: String, source: globset::Error) -> Self {
        Self { pattern, source }
    }

    /// Returns the offending pattern.
    #[must_use]
    pub fn pattern(&self) -> &str {
        &self.pattern
    }
}

impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "failed to compile filter pattern '{}': {}",
            self.pattern, self.source
        )
    }
}

impl std::error::Error for FilterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
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
