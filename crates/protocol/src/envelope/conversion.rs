use ::core::convert::TryFrom;

use super::log_code::LogCode;
use super::message_code::MessageCode;
use thiserror::Error;

/// Errors that arise when converting between [`LogCode`] and [`MessageCode`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum LogCodeConversionError {
    /// The [`LogCode`] has no multiplexed [`MessageCode`] equivalent.
    #[error("log code {0} has no multiplexed message equivalent")]
    NoMessageEquivalent(LogCode),
    /// The [`MessageCode`] does not map to a [`LogCode`].
    #[error("message code {0} has no log code equivalent")]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_code_returns_some_for_no_message_equivalent() {
        let err = LogCodeConversionError::NoMessageEquivalent(LogCode::Error);
        assert_eq!(err.log_code(), Some(LogCode::Error));
    }

    #[test]
    fn log_code_returns_none_for_no_log_equivalent() {
        let err = LogCodeConversionError::NoLogEquivalent(MessageCode::Data);
        assert_eq!(err.log_code(), None);
    }

    #[test]
    fn message_code_returns_some_for_no_log_equivalent() {
        let err = LogCodeConversionError::NoLogEquivalent(MessageCode::Data);
        assert_eq!(err.message_code(), Some(MessageCode::Data));
    }

    #[test]
    fn message_code_returns_none_for_no_message_equivalent() {
        let err = LogCodeConversionError::NoMessageEquivalent(LogCode::Error);
        assert_eq!(err.message_code(), None);
    }

    #[test]
    fn error_clone() {
        let err = LogCodeConversionError::NoMessageEquivalent(LogCode::Warning);
        let cloned = err;
        assert_eq!(err, cloned);
    }

    #[test]
    fn error_debug() {
        let err = LogCodeConversionError::NoMessageEquivalent(LogCode::Error);
        let debug = format!("{:?}", err);
        assert!(debug.contains("NoMessageEquivalent"));
    }

    #[test]
    fn error_display() {
        let err = LogCodeConversionError::NoMessageEquivalent(LogCode::Error);
        let display = format!("{}", err);
        assert!(display.contains("no multiplexed message"));
    }

    #[test]
    fn try_from_log_code_error_succeeds() {
        let result = MessageCode::try_from(LogCode::Error);
        assert!(result.is_ok());
    }

    #[test]
    fn try_from_message_code_data_fails() {
        let result = LogCode::try_from(MessageCode::Data);
        assert!(result.is_err());
    }

    #[test]
    fn try_from_message_code_error_succeeds() {
        let result = LogCode::try_from(MessageCode::Error);
        assert!(result.is_ok());
    }
}
