//! Error types produced when parsing protocol versions from text.

use core::fmt;

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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

impl fmt::Display for ParseProtocolVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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

impl std::error::Error for ParseProtocolVersionError {}
