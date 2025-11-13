use std::fmt;
use std::str::FromStr;

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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseSecludedArgsModeError {
    _private: (),
}

impl fmt::Display for ParseSecludedArgsModeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unrecognised secluded-args mode")
    }
}

impl std::error::Error for ParseSecludedArgsModeError {}

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
