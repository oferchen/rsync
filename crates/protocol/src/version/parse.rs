//! Error types produced when parsing protocol versions from text.

use thiserror::Error;

/// Errors that can occur while parsing a protocol version from a string.
#[doc(alias = "--protocol")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseProtocolVersionErrorKind {
    /// The provided string was empty after trimming ASCII whitespace.
    Empty,
    /// The provided string contained non-digit characters.
    InvalidDigit,
    /// The provided string encoded a negative value.
    Negative,
    /// The provided string encoded an integer larger than `u8::MAX`.
    Overflow,
    /// The parsed integer fell outside upstream rsync's supported range.
    UnsupportedRange(u8),
}

/// Error type returned when parsing a [`ProtocolVersion`](super::ProtocolVersion) from text fails.
#[doc(alias = "--protocol")]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub struct ParseProtocolVersionError {
    kind: ParseProtocolVersionErrorKind,
}

impl ParseProtocolVersionError {
    pub(crate) const fn new(kind: ParseProtocolVersionErrorKind) -> Self {
        Self { kind }
    }

    /// Returns the classification describing why parsing failed.
    #[must_use]
    pub const fn kind(self) -> ParseProtocolVersionErrorKind {
        self.kind
    }

    /// Returns the unsupported protocol byte that triggered
    /// [`ParseProtocolVersionErrorKind::UnsupportedRange`], if any.
    #[must_use]
    pub const fn unsupported_value(self) -> Option<u8> {
        match self.kind {
            ParseProtocolVersionErrorKind::UnsupportedRange(value) => Some(value),
            _ => None,
        }
    }
}

impl std::fmt::Display for ParseProtocolVersionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            ParseProtocolVersionErrorKind::Empty => f.write_str("protocol version string is empty"),
            ParseProtocolVersionErrorKind::InvalidDigit => {
                f.write_str("protocol version must be an unsigned integer")
            }
            ParseProtocolVersionErrorKind::Negative => {
                f.write_str("protocol version cannot be negative")
            }
            ParseProtocolVersionErrorKind::Overflow => {
                f.write_str("protocol version value exceeds u8::MAX")
            }
            ParseProtocolVersionErrorKind::UnsupportedRange(value) => {
                let (oldest, newest) = super::ProtocolVersion::supported_range_bounds();
                write!(
                    f,
                    "protocol version {value} is outside the supported range {oldest}-{newest}"
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_kind_empty() {
        let err = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::Empty);
        assert_eq!(err.kind(), ParseProtocolVersionErrorKind::Empty);
    }

    #[test]
    fn error_kind_invalid_digit() {
        let err = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::InvalidDigit);
        assert_eq!(err.kind(), ParseProtocolVersionErrorKind::InvalidDigit);
    }

    #[test]
    fn error_kind_negative() {
        let err = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::Negative);
        assert_eq!(err.kind(), ParseProtocolVersionErrorKind::Negative);
    }

    #[test]
    fn error_kind_overflow() {
        let err = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::Overflow);
        assert_eq!(err.kind(), ParseProtocolVersionErrorKind::Overflow);
    }

    #[test]
    fn error_kind_unsupported_range() {
        let err = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::UnsupportedRange(99));
        assert_eq!(err.kind(), ParseProtocolVersionErrorKind::UnsupportedRange(99));
    }

    #[test]
    fn unsupported_value_returns_some() {
        let err = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::UnsupportedRange(42));
        assert_eq!(err.unsupported_value(), Some(42));
    }

    #[test]
    fn unsupported_value_returns_none_for_empty() {
        let err = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::Empty);
        assert_eq!(err.unsupported_value(), None);
    }

    #[test]
    fn unsupported_value_returns_none_for_invalid_digit() {
        let err = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::InvalidDigit);
        assert_eq!(err.unsupported_value(), None);
    }

    #[test]
    fn unsupported_value_returns_none_for_negative() {
        let err = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::Negative);
        assert_eq!(err.unsupported_value(), None);
    }

    #[test]
    fn unsupported_value_returns_none_for_overflow() {
        let err = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::Overflow);
        assert_eq!(err.unsupported_value(), None);
    }

    #[test]
    fn display_empty() {
        let err = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::Empty);
        let display = format!("{}", err);
        assert!(display.contains("empty"));
    }

    #[test]
    fn display_invalid_digit() {
        let err = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::InvalidDigit);
        let display = format!("{}", err);
        assert!(display.contains("unsigned integer"));
    }

    #[test]
    fn display_negative() {
        let err = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::Negative);
        let display = format!("{}", err);
        assert!(display.contains("negative"));
    }

    #[test]
    fn display_overflow() {
        let err = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::Overflow);
        let display = format!("{}", err);
        assert!(display.contains("u8::MAX"));
    }

    #[test]
    fn display_unsupported_range() {
        let err = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::UnsupportedRange(99));
        let display = format!("{}", err);
        assert!(display.contains("99"));
        assert!(display.contains("supported range"));
    }

    #[test]
    fn error_is_clone() {
        let err = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::Empty);
        let cloned = err;
        assert_eq!(cloned.kind(), ParseProtocolVersionErrorKind::Empty);
    }

    #[test]
    fn error_is_eq() {
        let err1 = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::Empty);
        let err2 = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::Empty);
        assert_eq!(err1, err2);
    }

    #[test]
    fn error_ne_different_kind() {
        let err1 = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::Empty);
        let err2 = ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::Negative);
        assert_ne!(err1, err2);
    }

    #[test]
    fn error_kind_is_clone() {
        let kind = ParseProtocolVersionErrorKind::Empty;
        let cloned = kind;
        assert_eq!(cloned, ParseProtocolVersionErrorKind::Empty);
    }

    #[test]
    fn error_kind_is_eq() {
        let kind1 = ParseProtocolVersionErrorKind::Overflow;
        let kind2 = ParseProtocolVersionErrorKind::Overflow;
        assert_eq!(kind1, kind2);
    }

    #[test]
    fn error_kind_unsupported_range_eq() {
        let kind1 = ParseProtocolVersionErrorKind::UnsupportedRange(50);
        let kind2 = ParseProtocolVersionErrorKind::UnsupportedRange(50);
        assert_eq!(kind1, kind2);
    }

    #[test]
    fn error_kind_unsupported_range_ne() {
        let kind1 = ParseProtocolVersionErrorKind::UnsupportedRange(50);
        let kind2 = ParseProtocolVersionErrorKind::UnsupportedRange(51);
        assert_ne!(kind1, kind2);
    }
}
