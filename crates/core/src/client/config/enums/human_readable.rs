use std::str::FromStr;

use thiserror::Error;

/// Controls how byte counters are rendered for user-facing output.
///
/// Upstream `rsync` accepts optional levels for `--human-readable` that either
/// disable humanisation entirely, enable suffix-based formatting, or emit both
/// the humanised and exact decimal value.  The enum mirrors those levels so the
/// CLI can propagate the caller's preference to both the local renderer and any
/// fallback `rsync` invocations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[doc(alias = "--human-readable")]
pub enum HumanReadableMode {
    /// Disable human-readable formatting and display exact decimal values.
    Disabled,
    /// Enable suffix-based formatting (e.g. `1.23K`, `4.56M`).
    Enabled,
    /// Display both the human-readable value and the exact decimal value.
    Combined,
}

impl HumanReadableMode {
    /// Parses a human-readable level from textual input.
    ///
    /// The parser trims ASCII whitespace before interpreting the value and
    /// accepts the numeric levels used by upstream `rsync`. A dedicated error
    /// type captures empty inputs and out-of-range values so callers can emit
    /// diagnostics that match the original CLI.
    pub fn parse(text: &str) -> Result<Self, HumanReadableModeParseError> {
        let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
        if trimmed.is_empty() {
            return Err(HumanReadableModeParseError::Empty);
        }

        match trimmed {
            "0" => Ok(Self::Disabled),
            "1" => Ok(Self::Enabled),
            "2" => Ok(Self::Combined),
            other => Err(HumanReadableModeParseError::Invalid {
                value: other.to_owned(),
            }),
        }
    }

    /// Reports whether human-readable formatting should be used.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        !matches!(self, Self::Disabled)
    }

    /// Reports whether the exact decimal value should be included alongside the
    /// human-readable representation.
    #[must_use]
    pub const fn includes_exact(self) -> bool {
        matches!(self, Self::Combined)
    }
}

impl FromStr for HumanReadableMode {
    type Err = HumanReadableModeParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Errors produced when parsing a [`HumanReadableMode`] from text.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum HumanReadableModeParseError {
    /// The provided value was empty after trimming ASCII whitespace.
    #[error("human-readable level must not be empty")]
    Empty,
    /// The provided value did not match an accepted human-readable level.
    #[error("invalid human-readable level '{value}': expected 0, 1, or 2")]
    Invalid {
        /// The invalid value supplied by the caller after trimming ASCII whitespace.
        value: String,
    },
}

impl HumanReadableModeParseError {
    /// Returns the invalid value supplied by the caller when available.
    pub const fn invalid_value(&self) -> Option<&str> {
        match self {
            Self::Invalid { value } => Some(value.as_str()),
            Self::Empty => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_level_0() {
        assert_eq!(
            HumanReadableMode::parse("0").unwrap(),
            HumanReadableMode::Disabled
        );
    }

    #[test]
    fn parse_level_1() {
        assert_eq!(
            HumanReadableMode::parse("1").unwrap(),
            HumanReadableMode::Enabled
        );
    }

    #[test]
    fn parse_level_2() {
        assert_eq!(
            HumanReadableMode::parse("2").unwrap(),
            HumanReadableMode::Combined
        );
    }

    #[test]
    fn parse_with_whitespace() {
        assert_eq!(
            HumanReadableMode::parse("  1  ").unwrap(),
            HumanReadableMode::Enabled
        );
    }

    #[test]
    fn parse_empty_returns_error() {
        let result = HumanReadableMode::parse("");
        assert!(matches!(result, Err(HumanReadableModeParseError::Empty)));
    }

    #[test]
    fn parse_invalid_returns_error() {
        let result = HumanReadableMode::parse("3");
        assert!(matches!(
            result,
            Err(HumanReadableModeParseError::Invalid { .. })
        ));
    }

    #[test]
    fn from_str_works() {
        use std::str::FromStr;
        assert_eq!(
            HumanReadableMode::from_str("1").unwrap(),
            HumanReadableMode::Enabled
        );
    }

    #[test]
    fn is_enabled_disabled() {
        assert!(!HumanReadableMode::Disabled.is_enabled());
    }

    #[test]
    fn is_enabled_enabled() {
        assert!(HumanReadableMode::Enabled.is_enabled());
    }

    #[test]
    fn is_enabled_combined() {
        assert!(HumanReadableMode::Combined.is_enabled());
    }

    #[test]
    fn includes_exact_disabled() {
        assert!(!HumanReadableMode::Disabled.includes_exact());
    }

    #[test]
    fn includes_exact_enabled() {
        assert!(!HumanReadableMode::Enabled.includes_exact());
    }

    #[test]
    fn includes_exact_combined() {
        assert!(HumanReadableMode::Combined.includes_exact());
    }

    #[test]
    fn parse_error_invalid_value() {
        let err = HumanReadableMode::parse("foo").unwrap_err();
        assert_eq!(err.invalid_value(), Some("foo"));
    }

    #[test]
    fn parse_error_empty_value() {
        let err = HumanReadableMode::parse("").unwrap_err();
        assert_eq!(err.invalid_value(), None);
    }

    #[test]
    fn error_display() {
        let empty_err = HumanReadableModeParseError::Empty;
        assert!(empty_err.to_string().contains("must not be empty"));

        let invalid_err = HumanReadableModeParseError::Invalid {
            value: "3".to_owned(),
        };
        assert!(invalid_err.to_string().contains("invalid"));
    }
}
