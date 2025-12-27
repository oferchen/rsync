use ::core::fmt;
use ::core::str::FromStr;

use std::string::String;

use super::error::EnvelopeError;
use super::log_code::LogCode;
use thiserror::Error;

/// Tags used for multiplexed messages flowing over the rsync protocol stream.
///
/// The numeric values mirror the upstream `enum msgcode` definitions so that
/// higher layers can reason about message semantics without translating between
/// Rust and C identifiers. Values that alias upstream `enum logcode`
/// definitions retain their historic numbering to ensure interoperability with
/// existing daemons.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum MessageCode {
    #[doc(alias = "MSG_DATA")]
    /// Raw file data written to the multiplexed stream.
    Data = 0,
    #[doc(alias = "MSG_ERROR_XFER")]
    /// Fatal transfer error (`FERROR_XFER`).
    ErrorXfer = 1,
    #[doc(alias = "MSG_INFO")]
    /// Informational log message (`FINFO`).
    Info = 2,
    #[doc(alias = "MSG_ERROR")]
    /// Non-fatal error (`FERROR`).
    Error = 3,
    #[doc(alias = "MSG_WARNING")]
    /// Warning message (`FWARNING`).
    Warning = 4,
    #[doc(alias = "MSG_ERROR_SOCKET")]
    /// Error emitted by the sibling process over the receiver/generator pipe
    /// (`FERROR_SOCKET`).
    ErrorSocket = 5,
    #[doc(alias = "MSG_LOG")]
    /// Log message only written to the daemon logs (`FLOG`).
    Log = 6,
    #[doc(alias = "MSG_CLIENT")]
    /// Client-only message (`FCLIENT`).
    Client = 7,
    #[doc(alias = "MSG_ERROR_UTF8")]
    /// UTF-8 conversion problem reported by a sibling (`FERROR_UTF8`).
    ErrorUtf8 = 8,
    #[doc(alias = "MSG_REDO")]
    /// Request to reprocess a specific file-list index.
    Redo = 9,
    #[doc(alias = "MSG_STATS")]
    /// Transfer statistics destined for the generator.
    Stats = 10,
    #[doc(alias = "MSG_IO_ERROR")]
    /// Sender encountered an I/O error while accessing the source tree.
    IoError = 22,
    #[doc(alias = "MSG_IO_TIMEOUT")]
    /// Daemon communicating its timeout to the peer.
    IoTimeout = 33,
    #[doc(alias = "MSG_NOOP")]
    /// Legacy no-op message (protocol 30 compatibility).
    NoOp = 42,
    #[doc(alias = "MSG_ERROR_EXIT")]
    /// Synchronizes an error exit across processes (protocol ≥ 31).
    ErrorExit = 86,
    #[doc(alias = "MSG_SUCCESS")]
    /// Receiver reports a successfully updated file.
    Success = 100,
    #[doc(alias = "MSG_DELETED")]
    /// Receiver reports a deleted file.
    Deleted = 101,
    #[doc(alias = "MSG_NO_SEND")]
    /// Sender failed to open a requested file.
    NoSend = 102,
}

/// Error returned when parsing a multiplexed message code from its mnemonic name fails.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
#[error("unknown multiplexed message code name: \"{invalid_name}\"")]
pub struct ParseMessageCodeError {
    invalid_name: String,
}

impl ParseMessageCodeError {
    /// Creates a parse error that records the invalid mnemonic name.
    #[must_use]
    pub fn new(invalid_name: &str) -> Self {
        Self {
            invalid_name: invalid_name.to_owned(),
        }
    }

    /// Returns the mnemonic name that failed to parse.
    #[must_use]
    pub fn invalid_name(&self) -> &str {
        &self.invalid_name
    }
}

impl MessageCode {
    /// Alias constant representing the legacy `MSG_FLUSH` identifier.
    ///
    /// Upstream rsync exposes `MSG_FLUSH` as a preprocessor macro that maps to
    /// the same numeric value as [`MessageCode::Info`]. Maintaining the alias
    /// allows callers to reference the historic name when mirroring traces or
    /// constructing golden streams while still reusing the canonical `Info`
    /// variant for on-the-wire encoding.
    pub const FLUSH: MessageCode = MessageCode::Info;

    /// Returns the numeric representation expected on the wire.
    #[must_use]
    #[inline]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Attempts to construct a [`MessageCode`] from its on-the-wire numeric representation.
    #[must_use]
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Data),
            1 => Some(Self::ErrorXfer),
            2 => Some(Self::Info),
            3 => Some(Self::Error),
            4 => Some(Self::Warning),
            5 => Some(Self::ErrorSocket),
            6 => Some(Self::Log),
            7 => Some(Self::Client),
            8 => Some(Self::ErrorUtf8),
            9 => Some(Self::Redo),
            10 => Some(Self::Stats),
            22 => Some(Self::IoError),
            33 => Some(Self::IoTimeout),
            42 => Some(Self::NoOp),
            86 => Some(Self::ErrorExit),
            100 => Some(Self::Success),
            101 => Some(Self::Deleted),
            102 => Some(Self::NoSend),
            _ => None,
        }
    }

    /// Ordered list of all message codes understood by rsync 3.4.1.
    ///
    /// The variants are arranged by their numeric value so that callers can
    /// iterate deterministically when constructing golden multiplexed streams
    /// or exhaustively testing round-trips. The ordering mirrors upstream's
    /// `enum msgcode` definitions to preserve byte-level parity.
    pub const ALL: [MessageCode; 18] = [
        MessageCode::Data,
        MessageCode::ErrorXfer,
        MessageCode::Info,
        MessageCode::Error,
        MessageCode::Warning,
        MessageCode::ErrorSocket,
        MessageCode::Log,
        MessageCode::Client,
        MessageCode::ErrorUtf8,
        MessageCode::Redo,
        MessageCode::Stats,
        MessageCode::IoError,
        MessageCode::IoTimeout,
        MessageCode::NoOp,
        MessageCode::ErrorExit,
        MessageCode::Success,
        MessageCode::Deleted,
        MessageCode::NoSend,
    ];

    /// Returns the ordered list of all known message codes.
    #[must_use]
    pub const fn all() -> &'static [MessageCode; 18] {
        &Self::ALL
    }

    /// Reports whether this message carries human-readable logging output.
    #[must_use]
    pub const fn is_logging(self) -> bool {
        self.log_code().is_some()
    }

    /// Returns the log code associated with this message code when the payload
    /// represents logging output.
    #[must_use]
    pub const fn log_code(self) -> Option<LogCode> {
        match self {
            MessageCode::ErrorXfer => Some(LogCode::ErrorXfer),
            MessageCode::Info => Some(LogCode::Info),
            MessageCode::Error => Some(LogCode::Error),
            MessageCode::Warning => Some(LogCode::Warning),
            MessageCode::ErrorSocket => Some(LogCode::ErrorSocket),
            MessageCode::Log => Some(LogCode::Log),
            MessageCode::Client => Some(LogCode::Client),
            MessageCode::ErrorUtf8 => Some(LogCode::ErrorUtf8),
            _ => None,
        }
    }

    /// Returns the multiplexed message code associated with a log code when a
    /// one-to-one mapping exists.
    #[must_use]
    pub const fn from_log_code(log: LogCode) -> Option<Self> {
        match log {
            LogCode::ErrorXfer => Some(Self::ErrorXfer),
            LogCode::Info => Some(Self::Info),
            LogCode::Error => Some(Self::Error),
            LogCode::Warning => Some(Self::Warning),
            LogCode::ErrorSocket => Some(Self::ErrorSocket),
            LogCode::Log => Some(Self::Log),
            LogCode::Client => Some(Self::Client),
            LogCode::ErrorUtf8 => Some(Self::ErrorUtf8),
            LogCode::None => None,
        }
    }

    /// Returns the upstream `MSG_*` identifier associated with this code.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            MessageCode::Data => "MSG_DATA",
            MessageCode::ErrorXfer => "MSG_ERROR_XFER",
            MessageCode::Info => "MSG_INFO",
            MessageCode::Error => "MSG_ERROR",
            MessageCode::Warning => "MSG_WARNING",
            MessageCode::ErrorSocket => "MSG_ERROR_SOCKET",
            MessageCode::Log => "MSG_LOG",
            MessageCode::Client => "MSG_CLIENT",
            MessageCode::ErrorUtf8 => "MSG_ERROR_UTF8",
            MessageCode::Redo => "MSG_REDO",
            MessageCode::Stats => "MSG_STATS",
            MessageCode::IoError => "MSG_IO_ERROR",
            MessageCode::IoTimeout => "MSG_IO_TIMEOUT",
            MessageCode::NoOp => "MSG_NOOP",
            MessageCode::ErrorExit => "MSG_ERROR_EXIT",
            MessageCode::Success => "MSG_SUCCESS",
            MessageCode::Deleted => "MSG_DELETED",
            MessageCode::NoSend => "MSG_NO_SEND",
        }
    }
}

impl fmt::Display for MessageCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl TryFrom<u8> for MessageCode {
    type Error = EnvelopeError;

    fn try_from(value: u8) -> Result<Self, EnvelopeError> {
        Self::from_u8(value).ok_or(EnvelopeError::UnknownMessageCode(value))
    }
}

impl FromStr for MessageCode {
    type Err = ParseMessageCodeError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "MSG_DATA" => Ok(Self::Data),
            "MSG_ERROR_XFER" => Ok(Self::ErrorXfer),
            "MSG_INFO" => Ok(Self::Info),
            "MSG_FLUSH" => Ok(Self::Info),
            "MSG_ERROR" => Ok(Self::Error),
            "MSG_WARNING" => Ok(Self::Warning),
            "MSG_ERROR_SOCKET" => Ok(Self::ErrorSocket),
            "MSG_LOG" => Ok(Self::Log),
            "MSG_CLIENT" => Ok(Self::Client),
            "MSG_ERROR_UTF8" => Ok(Self::ErrorUtf8),
            "MSG_REDO" => Ok(Self::Redo),
            "MSG_STATS" => Ok(Self::Stats),
            "MSG_IO_ERROR" => Ok(Self::IoError),
            "MSG_IO_TIMEOUT" => Ok(Self::IoTimeout),
            "MSG_NOOP" => Ok(Self::NoOp),
            "MSG_ERROR_EXIT" => Ok(Self::ErrorExit),
            "MSG_SUCCESS" => Ok(Self::Success),
            "MSG_DELETED" => Ok(Self::Deleted),
            "MSG_NO_SEND" => Ok(Self::NoSend),
            other => Err(ParseMessageCodeError::new(other)),
        }
    }
}

impl From<MessageCode> for u8 {
    fn from(value: MessageCode) -> Self {
        value.as_u8()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_u8_returns_correct_value() {
        assert_eq!(MessageCode::Data.as_u8(), 0);
        assert_eq!(MessageCode::ErrorXfer.as_u8(), 1);
        assert_eq!(MessageCode::Info.as_u8(), 2);
        assert_eq!(MessageCode::Error.as_u8(), 3);
        assert_eq!(MessageCode::Warning.as_u8(), 4);
        assert_eq!(MessageCode::Success.as_u8(), 100);
        assert_eq!(MessageCode::Deleted.as_u8(), 101);
        assert_eq!(MessageCode::NoSend.as_u8(), 102);
    }

    #[test]
    fn from_u8_roundtrips_all_codes() {
        for code in MessageCode::ALL {
            let value = code.as_u8();
            assert_eq!(MessageCode::from_u8(value), Some(code));
        }
    }

    #[test]
    fn from_u8_returns_none_for_unknown() {
        assert!(MessageCode::from_u8(11).is_none());
        assert!(MessageCode::from_u8(99).is_none());
        assert!(MessageCode::from_u8(200).is_none());
    }

    #[test]
    fn all_contains_18_codes() {
        assert_eq!(MessageCode::ALL.len(), 18);
        assert_eq!(MessageCode::all().len(), 18);
    }

    #[test]
    fn flush_alias_equals_info() {
        assert_eq!(MessageCode::FLUSH, MessageCode::Info);
        assert_eq!(MessageCode::FLUSH.as_u8(), 2);
    }

    #[test]
    fn is_logging_for_log_codes() {
        assert!(MessageCode::ErrorXfer.is_logging());
        assert!(MessageCode::Info.is_logging());
        assert!(MessageCode::Error.is_logging());
        assert!(MessageCode::Warning.is_logging());
        assert!(MessageCode::Log.is_logging());
        assert!(MessageCode::Client.is_logging());
    }

    #[test]
    fn is_logging_false_for_non_log_codes() {
        assert!(!MessageCode::Data.is_logging());
        assert!(!MessageCode::Redo.is_logging());
        assert!(!MessageCode::Stats.is_logging());
        assert!(!MessageCode::Success.is_logging());
        assert!(!MessageCode::Deleted.is_logging());
    }

    #[test]
    fn log_code_returns_correct_mapping() {
        assert_eq!(MessageCode::ErrorXfer.log_code(), Some(LogCode::ErrorXfer));
        assert_eq!(MessageCode::Info.log_code(), Some(LogCode::Info));
        assert_eq!(MessageCode::Error.log_code(), Some(LogCode::Error));
        assert_eq!(MessageCode::Warning.log_code(), Some(LogCode::Warning));
        assert_eq!(MessageCode::Data.log_code(), None);
        assert_eq!(MessageCode::Success.log_code(), None);
    }

    #[test]
    fn from_log_code_roundtrips() {
        assert_eq!(
            MessageCode::from_log_code(LogCode::ErrorXfer),
            Some(MessageCode::ErrorXfer)
        );
        assert_eq!(
            MessageCode::from_log_code(LogCode::Info),
            Some(MessageCode::Info)
        );
        assert_eq!(
            MessageCode::from_log_code(LogCode::Error),
            Some(MessageCode::Error)
        );
        assert_eq!(MessageCode::from_log_code(LogCode::None), None);
    }

    #[test]
    fn name_returns_msg_prefix() {
        assert_eq!(MessageCode::Data.name(), "MSG_DATA");
        assert_eq!(MessageCode::ErrorXfer.name(), "MSG_ERROR_XFER");
        assert_eq!(MessageCode::Info.name(), "MSG_INFO");
        assert_eq!(MessageCode::NoOp.name(), "MSG_NOOP");
        assert_eq!(MessageCode::NoSend.name(), "MSG_NO_SEND");
    }

    #[test]
    fn display_matches_name() {
        for code in MessageCode::ALL {
            assert_eq!(format!("{}", code), code.name());
        }
    }

    #[test]
    fn from_str_parses_all_names() {
        for code in MessageCode::ALL {
            let parsed: MessageCode = code.name().parse().unwrap();
            assert_eq!(parsed, code);
        }
    }

    #[test]
    fn from_str_accepts_msg_flush() {
        let parsed: MessageCode = "MSG_FLUSH".parse().unwrap();
        assert_eq!(parsed, MessageCode::Info);
    }

    #[test]
    fn from_str_rejects_unknown() {
        let result: Result<MessageCode, _> = "MSG_UNKNOWN".parse();
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert_eq!(err.invalid_name(), "MSG_UNKNOWN");
    }

    #[test]
    fn try_from_u8_success() {
        let code: Result<MessageCode, _> = 0_u8.try_into();
        assert_eq!(code.unwrap(), MessageCode::Data);
    }

    #[test]
    fn try_from_u8_error() {
        let code: Result<MessageCode, _> = 255_u8.try_into();
        assert!(code.is_err());
    }

    #[test]
    fn into_u8_works() {
        let value: u8 = MessageCode::Success.into();
        assert_eq!(value, 100);
    }

    #[test]
    fn parse_message_code_error_new() {
        let err = ParseMessageCodeError::new("invalid");
        assert_eq!(err.invalid_name(), "invalid");
    }

    #[test]
    fn parse_message_code_error_display() {
        let err = ParseMessageCodeError::new("BAD_CODE");
        let display = format!("{}", err);
        assert!(display.contains("BAD_CODE"));
    }
}
