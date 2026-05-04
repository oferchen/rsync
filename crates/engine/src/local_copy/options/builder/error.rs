/// Errors that can occur when building [`super::LocalCopyOptionsBuilder`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BuilderError {
    /// Conflicting options were specified.
    ConflictingOptions {
        /// Description of the first conflicting option.
        option1: &'static str,
        /// Description of the second conflicting option.
        option2: &'static str,
    },
    /// An invalid combination of options was specified.
    InvalidCombination {
        /// Description of the invalid combination.
        message: String,
    },
    /// A required option is missing.
    MissingRequiredOption {
        /// Name of the missing option.
        option: &'static str,
    },
    /// An option value is out of range.
    ValueOutOfRange {
        /// Name of the option with invalid value.
        option: &'static str,
        /// Description of the valid range.
        range: String,
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
            Self::MissingRequiredOption { option } => {
                write!(f, "missing required option: {option}")
            }
            Self::ValueOutOfRange { option, range } => {
                write!(f, "value out of range for {option}: expected {range}")
            }
        }
    }
}

impl std::error::Error for BuilderError {}
