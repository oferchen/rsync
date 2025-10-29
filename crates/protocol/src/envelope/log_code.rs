use core::convert::TryFrom;
use core::fmt;
use core::str::FromStr;

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
        Self::from_u8(value).ok_or(ParseLogCodeError::new(value))
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseLogCodeError {
    /// The provided numeric identifier is not known to rsync 3.4.1.
    InvalidValue(u8),
    /// The provided mnemonic name is not known to rsync 3.4.1.
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
    pub fn invalid_name(&self) -> Option<&str> {
        match self {
            Self::InvalidValue(_) => None,
            Self::InvalidName(name) => Some(name.as_str()),
        }
    }
}

impl fmt::Display for ParseLogCodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidValue(value) => write!(f, "unknown log code value: {value}"),
            Self::InvalidName(name) => write!(f, "unknown log code name: \"{name}\""),
        }
    }
}

impl std::error::Error for ParseLogCodeError {}
