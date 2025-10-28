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
