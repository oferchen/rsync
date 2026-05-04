//! Error types for [`ServerConfigBuilder`](super::ServerConfigBuilder) validation.

/// Errors that can occur when building a [`ServerConfig`](super::ServerConfig).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BuilderError {
    /// Mutually exclusive options were specified.
    ConflictingOptions {
        /// The first conflicting option.
        option1: &'static str,
        /// The second conflicting option.
        option2: &'static str,
    },
    /// An invalid combination of options was specified.
    InvalidCombination {
        /// Description of the invalid combination.
        message: String,
    },
}

impl std::fmt::Display for BuilderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConflictingOptions { option1, option2 } => {
                write!(f, "conflicting options: {option1} and {option2}")
            }
            Self::InvalidCombination { message } => {
                write!(f, "invalid option combination: {message}")
            }
        }
    }
}

impl std::error::Error for BuilderError {}
