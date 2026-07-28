//! Upstream rsync's `enum logcode` vocabulary.
//!
//! Every upstream diagnostic carries a logcode that `rwrite()` uses to pick
//! its destination: the client's stdout/stderr, the daemon log or `--log-file`,
//! or the multiplexed message stream (upstream: rsync.h:275-282, log.c:rwrite()).
//! This module is the single canonical definition of that vocabulary; the
//! `protocol` crate re-exports it for wire-side `MSG_*` conversions and the
//! diagnostics event model tags every [`DiagnosticEvent`](crate::DiagnosticEvent)
//! with one of these codes.

use std::fmt;
use std::str::FromStr;

/// Log classification used by upstream rsync's `enum logcode` table.
///
/// The numeric values mirror the identifiers found in `rsync.h:275-282` so the
/// logging subsystem can translate between multiplexed tags and the log
/// destinations used by upstream traces. While only a subset of log codes flow
/// over the multiplexed stream, the complete enum is provided for parity
/// (including `FNONE`, which upstream reserves for internal use).
///
/// Each variant documents its routing rule from `log.c:rwrite()`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum LogCode {
    #[doc(alias = "FNONE")]
    /// Placeholder that is never passed to `rwrite()` and never transmitted
    /// on the wire (`FNONE`, upstream: rsync.h:276 "never sent").
    None = 0,
    #[doc(alias = "FERROR_XFER")]
    /// Transfer error (`FERROR_XFER`). Routed to stderr and records that a
    /// transfer error occurred (upstream: log.c:310-316 sets `got_xfer_error`);
    /// sent over the socket as `MSG_ERROR_XFER` at every protocol
    /// (upstream: rsync.h:277).
    ErrorXfer = 1,
    #[doc(alias = "FINFO")]
    /// Informational message (`FINFO`). Routed to the client's stdout unless
    /// `--quiet` (upstream: log.c:317-320) and additionally to the daemon
    /// log/`--log-file` when one is active (upstream: log.c:290-305).
    Info = 2,
    #[doc(alias = "FERROR")]
    /// Non-transfer error (`FERROR`). Routed to stderr (upstream:
    /// log.c:313-316); sent over the socket as `MSG_ERROR` at protocols >= 30
    /// and downgraded to `MSG_ERROR_XFER` below (upstream: log.c:332-335).
    Error = 3,
    #[doc(alias = "FWARNING")]
    /// Warning (`FWARNING`). Routed to stderr (upstream: log.c:314-316); sent
    /// over the socket as `MSG_WARNING` at protocols >= 30 and downgraded to
    /// `MSG_INFO` below (upstream: log.c:332-337).
    Warning = 4,
    #[doc(alias = "FERROR_SOCKET")]
    /// Error travelling over the receiver -> generator pipe (`FERROR_SOCKET`);
    /// a non-sibling simplifies it to `FERROR` on arrival (upstream:
    /// log.c:281-282, rsync.h:279).
    ErrorSocket = 5,
    #[doc(alias = "FLOG")]
    /// Log-file-only message (`FLOG`). Written to the daemon log/`--log-file`
    /// and never to the client's stdout; discarded outright when no log
    /// destination is active (upstream: log.c:290-307).
    Log = 6,
    #[doc(alias = "FCLIENT")]
    /// Client-only message (`FCLIENT`). Never transmitted on the wire and
    /// converted to `FINFO` before dispatch, skipping the daemon-log branch
    /// (upstream: log.c:288-289, rsync.h:281).
    Client = 7,
    #[doc(alias = "FERROR_UTF8")]
    /// UTF-8-encoded error travelling over the receiver -> generator pipe
    /// (`FERROR_UTF8`); flags the text as UTF-8 and becomes `FERROR`
    /// (upstream: log.c:283-286, rsync.h:280).
    ErrorUtf8 = 8,
}

impl LogCode {
    /// Ordered list of all log codes understood by rsync 3.4.4.
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
    /// The provided numeric identifier is not known to rsync 3.4.4.
    #[error("unknown log code value: {0}")]
    InvalidValue(u8),
    /// The provided mnemonic name is not known to rsync 3.4.4.
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
    pub const fn invalid_value(&self) -> Option<u8> {
        match self {
            Self::InvalidValue(value) => Some(*value),
            Self::InvalidName(_) => None,
        }
    }

    /// Returns the mnemonic name that failed to parse, when available.
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

    /// Pins the variant set 1:1 against upstream's `enum logcode` table
    /// (upstream: rsync.h:275-282). A new upstream code, a renamed code, or a
    /// changed discriminant must fail here.
    #[test]
    fn variant_set_matches_upstream_enum_logcode() {
        const UPSTREAM: [(&str, u8); 9] = [
            ("FNONE", 0),         // rsync.h:276 - never sent
            ("FERROR_XFER", 1),   // rsync.h:277 - sent over socket for any protocol
            ("FINFO", 2),         // rsync.h:277 - sent over socket for any protocol
            ("FERROR", 3),        // rsync.h:278 - sent over socket for protocols >= 30
            ("FWARNING", 4),      // rsync.h:278 - sent over socket for protocols >= 30
            ("FERROR_SOCKET", 5), // rsync.h:279 - receiver -> generator pipe only
            ("FLOG", 6),          // rsync.h:279 - receiver -> generator pipe only
            ("FCLIENT", 7),       // rsync.h:281 - never transmitted
            ("FERROR_UTF8", 8),   // rsync.h:280 - receiver -> generator pipe only
        ];
        assert_eq!(LogCode::ALL.len(), UPSTREAM.len());
        for (code, (name, value)) in LogCode::ALL.iter().zip(UPSTREAM) {
            assert_eq!(code.name(), name);
            assert_eq!(code.as_u8(), value);
        }
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
