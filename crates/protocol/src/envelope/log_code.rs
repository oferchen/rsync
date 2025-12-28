use ::core::convert::TryFrom;
use ::core::fmt;
use ::core::str::FromStr;

use std::string::String;

/// Log classification used by upstream rsync's `enum logcode` table.
///
/// The numeric values mirror the identifiers found in `rsync.h` so the logging
/// subsystem can translate between multiplexed tags and the log severities used
/// by upstream traces. While only a subset of log codes flow over the
/// multiplexed stream, the complete enum is provided for parity (including
/// `FNONE`, which upstream reserves for internal use).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum LogCode {
    #[doc(alias = "FNONE")]
    /// Placeholder that is never transmitted on the wire (`FNONE`).
    None = 0,
    #[doc(alias = "FERROR_XFER")]
    /// Fatal transfer error (`FERROR_XFER`).
    ErrorXfer = 1,
    #[doc(alias = "FINFO")]
    /// Informational log message (`FINFO`).
    Info = 2,
    #[doc(alias = "FERROR")]
    /// Non-fatal error (`FERROR`).
    Error = 3,
    #[doc(alias = "FWARNING")]
    /// Warning message (`FWARNING`).
    Warning = 4,
    #[doc(alias = "FERROR_SOCKET")]
    /// Error emitted by the sibling process over the receiver/generator pipe
    /// (`FERROR_SOCKET`).
    ErrorSocket = 5,
    #[doc(alias = "FLOG")]
    /// Log message only written to the daemon logs (`FLOG`).
    Log = 6,
    #[doc(alias = "FCLIENT")]
    /// Client-only message (`FCLIENT`).
    Client = 7,
    #[doc(alias = "FERROR_UTF8")]
    /// UTF-8 conversion problem reported by a sibling (`FERROR_UTF8`).
    ErrorUtf8 = 8,
}

impl LogCode {
    /// Ordered list of all log codes understood by rsync 3.4.1.
    pub const ALL: [LogCode; 9] = [
        LogCode::None,
        LogCode::ErrorXfer,
        LogCode::Info,
        LogCode::Error,
        LogCode::Warning,
        LogCode::ErrorSocket,
        LogCode::Log,
        LogCode::Client,
        LogCode::ErrorUtf8,
    ];

    /// Returns the ordered list of all log codes.
    #[must_use]
    pub const fn all() -> &'static [LogCode; 9] {
        &Self::ALL
    }

    /// Returns the numeric representation expected on the wire.
    #[must_use]
    #[inline]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Attempts to construct a [`LogCode`] from its numeric representation.
    #[must_use]
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::None),
            1 => Some(Self::ErrorXfer),
            2 => Some(Self::Info),
            3 => Some(Self::Error),
            4 => Some(Self::Warning),
            5 => Some(Self::ErrorSocket),
            6 => Some(Self::Log),
            7 => Some(Self::Client),
            8 => Some(Self::ErrorUtf8),
            _ => None,
        }
    }

    /// Returns the upstream `F*` identifier associated with this log code.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            LogCode::None => "FNONE",
            LogCode::ErrorXfer => "FERROR_XFER",
            LogCode::Info => "FINFO",
            LogCode::Error => "FERROR",
            LogCode::Warning => "FWARNING",
            LogCode::ErrorSocket => "FERROR_SOCKET",
            LogCode::Log => "FLOG",
            LogCode::Client => "FCLIENT",
            LogCode::ErrorUtf8 => "FERROR_UTF8",
        }
    }
}

impl fmt::Display for LogCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl TryFrom<u8> for LogCode {
    type Error = ParseLogCodeError;

    fn try_from(value: u8) -> Result<Self, ParseLogCodeError> {
        Self::from_u8(value).ok_or_else(|| ParseLogCodeError::new(value))
    }
}

impl From<LogCode> for u8 {
    fn from(value: LogCode) -> Self {
        value.as_u8()
    }
}

impl FromStr for LogCode {
    type Err = ParseLogCodeError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "FNONE" => Ok(Self::None),
            "FERROR_XFER" => Ok(Self::ErrorXfer),
            "FINFO" => Ok(Self::Info),
            "FERROR" => Ok(Self::Error),
            "FWARNING" => Ok(Self::Warning),
            "FERROR_SOCKET" => Ok(Self::ErrorSocket),
            "FLOG" => Ok(Self::Log),
            "FCLIENT" => Ok(Self::Client),
            "FERROR_UTF8" => Ok(Self::ErrorUtf8),
            other => Err(ParseLogCodeError::new_name(other)),
        }
    }
}

/// Error returned when parsing a log code from its representation fails.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ParseLogCodeError {
    /// The provided numeric identifier is not known to rsync 3.4.1.
    #[error("unknown log code value: {0}")]
    InvalidValue(u8),
    /// The provided mnemonic name is not known to rsync 3.4.1.
    #[error("unknown log code name: \"{0}\"")]
    InvalidName(String),
}

impl ParseLogCodeError {
    /// Creates a parse error that records the invalid numeric value.
    #[must_use]
    pub const fn new(invalid_value: u8) -> Self {
        Self::InvalidValue(invalid_value)
    }

    /// Creates a parse error that records the invalid mnemonic name.
    #[must_use]
    pub fn new_name(invalid_name: &str) -> Self {
        Self::InvalidName(invalid_name.to_owned())
    }

    /// Returns the numeric value that failed to parse, when available.
    #[must_use]
    pub const fn invalid_value(&self) -> Option<u8> {
        match self {
            Self::InvalidValue(value) => Some(*value),
            Self::InvalidName(_) => None,
        }
    }

    /// Returns the mnemonic name that failed to parse, when available.
    #[must_use]
    pub const fn invalid_name(&self) -> Option<&str> {
        match self {
            Self::InvalidValue(_) => None,
            Self::InvalidName(name) => Some(name.as_str()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_u8_returns_correct_values() {
        assert_eq!(LogCode::None.as_u8(), 0);
        assert_eq!(LogCode::ErrorXfer.as_u8(), 1);
        assert_eq!(LogCode::Info.as_u8(), 2);
        assert_eq!(LogCode::Error.as_u8(), 3);
        assert_eq!(LogCode::Warning.as_u8(), 4);
        assert_eq!(LogCode::ErrorSocket.as_u8(), 5);
        assert_eq!(LogCode::Log.as_u8(), 6);
        assert_eq!(LogCode::Client.as_u8(), 7);
        assert_eq!(LogCode::ErrorUtf8.as_u8(), 8);
    }

    #[test]
    fn from_u8_roundtrips_all() {
        for code in LogCode::ALL {
            let value = code.as_u8();
            assert_eq!(LogCode::from_u8(value), Some(code));
        }
    }

    #[test]
    fn from_u8_returns_none_for_unknown() {
        assert!(LogCode::from_u8(9).is_none());
        assert!(LogCode::from_u8(100).is_none());
        assert!(LogCode::from_u8(255).is_none());
    }

    #[test]
    fn all_contains_9_codes() {
        assert_eq!(LogCode::ALL.len(), 9);
        assert_eq!(LogCode::all().len(), 9);
    }

    #[test]
    fn name_returns_f_prefix() {
        assert_eq!(LogCode::None.name(), "FNONE");
        assert_eq!(LogCode::ErrorXfer.name(), "FERROR_XFER");
        assert_eq!(LogCode::Info.name(), "FINFO");
        assert_eq!(LogCode::Error.name(), "FERROR");
        assert_eq!(LogCode::Warning.name(), "FWARNING");
        assert_eq!(LogCode::ErrorSocket.name(), "FERROR_SOCKET");
        assert_eq!(LogCode::Log.name(), "FLOG");
        assert_eq!(LogCode::Client.name(), "FCLIENT");
        assert_eq!(LogCode::ErrorUtf8.name(), "FERROR_UTF8");
    }

    #[test]
    fn display_matches_name() {
        for code in LogCode::ALL {
            assert_eq!(format!("{code}"), code.name());
        }
    }

    #[test]
    fn from_str_parses_all_names() {
        for code in LogCode::ALL {
            let parsed: LogCode = code.name().parse().unwrap();
            assert_eq!(parsed, code);
        }
    }

    #[test]
    fn from_str_rejects_unknown() {
        let result: Result<LogCode, _> = "FUNKNOWN".parse();
        assert!(result.is_err());
    }

    #[test]
    fn try_from_u8_success() {
        let code: Result<LogCode, _> = 2_u8.try_into();
        assert_eq!(code.unwrap(), LogCode::Info);
    }

    #[test]
    fn try_from_u8_error() {
        let result: Result<LogCode, _> = 99_u8.try_into();
        assert!(result.is_err());
    }

    #[test]
    fn into_u8_works() {
        let value: u8 = LogCode::Warning.into();
        assert_eq!(value, 4);
    }

    #[test]
    fn parse_error_new_value() {
        let err = ParseLogCodeError::new(42);
        assert_eq!(err.invalid_value(), Some(42));
        assert!(err.invalid_name().is_none());
    }

    #[test]
    fn parse_error_new_name() {
        let err = ParseLogCodeError::new_name("BAD");
        assert!(err.invalid_value().is_none());
        assert_eq!(err.invalid_name(), Some("BAD"));
    }

    #[test]
    fn parse_error_display_value() {
        let err = ParseLogCodeError::new(99);
        let display = format!("{err}");
        assert!(display.contains("99"));
    }

    #[test]
    fn parse_error_display_name() {
        let err = ParseLogCodeError::new_name("INVALID");
        let display = format!("{err}");
        assert!(display.contains("INVALID"));
    }
}
