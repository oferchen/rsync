use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// Describes how secluded argument mode is advertised in `--version` output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecludedArgsMode {
    /// Secluded arguments are available when explicitly requested.
    Optional,
    /// Secluded arguments are enabled by default, matching upstream's maintainer builds.
    Default,
}

impl SecludedArgsMode {
    const fn label_eq(label: &str, expected: &str) -> bool {
        let lhs = label.as_bytes();
        let rhs = expected.as_bytes();

        if lhs.len() != rhs.len() {
            return false;
        }

        let mut index = 0;
        while index < lhs.len() {
            if lhs[index] != rhs[index] {
                return false;
            }
            index += 1;
        }

        true
    }

    /// Returns the canonical label rendered in `--version` output.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Optional => "optional secluded-args",
            Self::Default => "default secluded-args",
        }
    }

    /// Parses a label produced by [`Self::label`] back into its variant.
    #[must_use]
    pub const fn from_label(label: &str) -> Option<Self> {
        if Self::label_eq(label, "optional secluded-args") {
            Some(Self::Optional)
        } else if Self::label_eq(label, "default secluded-args") {
            Some(Self::Default)
        } else {
            None
        }
    }
}

/// Error returned when parsing a [`SecludedArgsMode`] from text fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
#[error("unrecognised secluded-args mode")]
pub struct ParseSecludedArgsModeError {
    _private: (),
}

impl fmt::Display for SecludedArgsMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for SecludedArgsMode {
    type Err = ParseSecludedArgsModeError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::from_label(input).ok_or(ParseSecludedArgsModeError { _private: () })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests for SecludedArgsMode::label
    #[test]
    fn optional_label() {
        assert_eq!(SecludedArgsMode::Optional.label(), "optional secluded-args");
    }

    #[test]
    fn default_label() {
        assert_eq!(SecludedArgsMode::Default.label(), "default secluded-args");
    }

    // Tests for SecludedArgsMode::from_label
    #[test]
    fn from_label_optional() {
        let result = SecludedArgsMode::from_label("optional secluded-args");
        assert_eq!(result, Some(SecludedArgsMode::Optional));
    }

    #[test]
    fn from_label_default() {
        let result = SecludedArgsMode::from_label("default secluded-args");
        assert_eq!(result, Some(SecludedArgsMode::Default));
    }

    #[test]
    fn from_label_invalid() {
        assert_eq!(SecludedArgsMode::from_label("invalid"), None);
    }

    #[test]
    fn from_label_empty() {
        assert_eq!(SecludedArgsMode::from_label(""), None);
    }

    #[test]
    fn from_label_partial() {
        assert_eq!(SecludedArgsMode::from_label("optional"), None);
        assert_eq!(SecludedArgsMode::from_label("default"), None);
    }

    // Tests for Display trait
    #[test]
    fn display_optional() {
        assert_eq!(
            format!("{}", SecludedArgsMode::Optional),
            "optional secluded-args"
        );
    }

    #[test]
    fn display_default() {
        assert_eq!(
            format!("{}", SecludedArgsMode::Default),
            "default secluded-args"
        );
    }

    // Tests for FromStr trait
    #[test]
    fn parse_optional() {
        let result: Result<SecludedArgsMode, _> = "optional secluded-args".parse();
        assert_eq!(result.unwrap(), SecludedArgsMode::Optional);
    }

    #[test]
    fn parse_default() {
        let result: Result<SecludedArgsMode, _> = "default secluded-args".parse();
        assert_eq!(result.unwrap(), SecludedArgsMode::Default);
    }

    #[test]
    fn parse_invalid_fails() {
        let result: Result<SecludedArgsMode, _> = "invalid".parse();
        assert!(result.is_err());
    }

    #[test]
    fn parse_empty_fails() {
        let result: Result<SecludedArgsMode, _> = "".parse();
        assert!(result.is_err());
    }

    // Tests for trait implementations
    #[test]
    fn mode_is_clone() {
        let mode = SecludedArgsMode::Optional;
        let cloned = mode;
        assert_eq!(mode, cloned);
    }

    #[test]
    fn mode_is_copy() {
        let mode = SecludedArgsMode::Optional;
        let copied = mode;
        assert_eq!(mode, copied);
    }

    #[test]
    fn mode_debug_contains_variant() {
        let debug = format!("{:?}", SecludedArgsMode::Optional);
        assert!(debug.contains("Optional"));
    }

    #[test]
    fn modes_are_not_equal() {
        assert_ne!(SecludedArgsMode::Optional, SecludedArgsMode::Default);
    }

    // Tests for ParseSecludedArgsModeError
    #[test]
    fn error_display() {
        let err = "invalid".parse::<SecludedArgsMode>().unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unrecognised"));
    }

    #[test]
    fn error_is_clone() {
        let err = "invalid".parse::<SecludedArgsMode>().unwrap_err();
        let cloned = err;
        assert_eq!(err, cloned);
    }

    #[test]
    fn error_is_copy() {
        let err = "invalid".parse::<SecludedArgsMode>().unwrap_err();
        let copied = err;
        assert_eq!(err, copied);
    }

    // Tests for roundtrip
    #[test]
    fn label_roundtrip_optional() {
        let mode = SecludedArgsMode::Optional;
        let label = mode.label();
        let parsed = SecludedArgsMode::from_label(label);
        assert_eq!(parsed, Some(mode));
    }

    #[test]
    fn label_roundtrip_default() {
        let mode = SecludedArgsMode::Default;
        let label = mode.label();
        let parsed = SecludedArgsMode::from_label(label);
        assert_eq!(parsed, Some(mode));
    }

    #[test]
    fn from_str_roundtrip() {
        for mode in [SecludedArgsMode::Optional, SecludedArgsMode::Default] {
            let label = mode.to_string();
            let parsed: SecludedArgsMode = label.parse().unwrap();
            assert_eq!(parsed, mode);
        }
    }
}
