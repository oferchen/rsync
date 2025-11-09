use ::core::convert::TryFrom;
use ::core::fmt;

use super::log_code::LogCode;
use super::message_code::MessageCode;

/// Errors that arise when converting between [`LogCode`] and [`MessageCode`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogCodeConversionError {
    /// The [`LogCode`] has no multiplexed [`MessageCode`] equivalent.
    NoMessageEquivalent(LogCode),
    /// The [`MessageCode`] does not map to a [`LogCode`].
    NoLogEquivalent(MessageCode),
}

impl LogCodeConversionError {
    /// Returns the [`LogCode`] that could not be converted, when available.
    #[must_use]
    pub const fn log_code(self) -> Option<LogCode> {
        match self {
            Self::NoMessageEquivalent(log) => Some(log),
            Self::NoLogEquivalent(_) => None,
        }
    }

    /// Returns the [`MessageCode`] that could not be converted, when available.
    #[must_use]
    pub const fn message_code(self) -> Option<MessageCode> {
        match self {
            Self::NoMessageEquivalent(_) => None,
            Self::NoLogEquivalent(code) => Some(code),
        }
    }
}

impl fmt::Display for LogCodeConversionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoMessageEquivalent(log) => {
                write!(f, "log code {log} has no multiplexed message equivalent",)
            }
            Self::NoLogEquivalent(code) => {
                write!(f, "message code {code} has no log code equivalent")
            }
        }
    }
}

impl std::error::Error for LogCodeConversionError {}

impl TryFrom<LogCode> for MessageCode {
    type Error = LogCodeConversionError;

    fn try_from(value: LogCode) -> Result<Self, LogCodeConversionError> {
        MessageCode::from_log_code(value).ok_or(LogCodeConversionError::NoMessageEquivalent(value))
    }
}

impl TryFrom<MessageCode> for LogCode {
    type Error = LogCodeConversionError;

    fn try_from(value: MessageCode) -> Result<Self, LogCodeConversionError> {
        value
            .log_code()
            .ok_or(LogCodeConversionError::NoLogEquivalent(value))
    }
}
