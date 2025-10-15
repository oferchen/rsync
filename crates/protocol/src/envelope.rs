use core::convert::TryFrom;
use core::fmt;
use core::str::FromStr;

use std::string::String;

/// Number of bytes in a multiplexed rsync message header.
pub const HEADER_LEN: usize = 4;

/// Maximum payload length representable in a multiplexed header.
pub const MAX_PAYLOAD_LENGTH: u32 = 0x00FF_FFFF;

const MPLEX_BASE: u8 = 7;
const PAYLOAD_MASK: u32 = 0x00FF_FFFF;

/// Tags used for multiplexed messages flowing over the rsync protocol stream.
///
/// The numeric values mirror the upstream `enum msgcode` definitions so that
/// higher layers can reason about message semantics without translating between
/// Rust and C identifiers. Values that alias upstream `enum logcode`
/// definitions retain their historic numbering to ensure interoperability with
/// existing daemons.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum MessageCode {
    /// Raw file data written to the multiplexed stream.
    Data = 0,
    /// Fatal transfer error (`FERROR_XFER`).
    ErrorXfer = 1,
    /// Informational log message (`FINFO`).
    Info = 2,
    /// Non-fatal error (`FERROR`).
    Error = 3,
    /// Warning message (`FWARNING`).
    Warning = 4,
    /// Error emitted by the sibling process over the receiver/generator pipe
    /// (`FERROR_SOCKET`).
    ErrorSocket = 5,
    /// Log message only written to the daemon logs (`FLOG`).
    Log = 6,
    /// Client-only message (`FCLIENT`).
    Client = 7,
    /// UTF-8 conversion problem reported by a sibling (`FERROR_UTF8`).
    ErrorUtf8 = 8,
    /// Request to reprocess a specific file-list index.
    Redo = 9,
    /// Transfer statistics destined for the generator.
    Stats = 10,
    /// Sender encountered an I/O error while accessing the source tree.
    IoError = 22,
    /// Daemon communicating its timeout to the peer.
    IoTimeout = 33,
    /// Legacy no-op message (protocol 30 compatibility).
    NoOp = 42,
    /// Synchronizes an error exit across processes (protocol ≥ 31).
    ErrorExit = 86,
    /// Receiver reports a successfully updated file.
    Success = 100,
    /// Receiver reports a deleted file.
    Deleted = 101,
    /// Sender failed to open a requested file.
    NoSend = 102,
}

/// Error returned when parsing a multiplexed message code from its mnemonic name fails.
#[derive(Clone, Debug, Eq, PartialEq)]
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

impl fmt::Display for ParseMessageCodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown multiplexed message code name: \"{}\"",
            self.invalid_name
        )
    }
}

impl std::error::Error for ParseMessageCodeError {}

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
    ///
    /// The mapping mirrors the upstream `enum msgcode` table and can be used in const
    /// contexts where [`TryFrom<u8>`] is not available. Invalid values yield `None`, allowing
    /// callers to fall back to [`EnvelopeError::UnknownMessageCode`] for diagnostics.
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
    ///
    /// Upstream rsync routes multiplexed messages tagged with the `FINFO`,
    /// `FERROR*`, `FWARNING`, `FLOG`, and `FCLIENT` log codes directly to the
    /// logging subsystem inside `read_a_msg()` (see `io.c`). Messages in this
    /// category contain UTF-8 text payloads destined for the user or daemon
    /// logs rather than control data. The Rust implementation mirrors that
    /// classification so higher layers can dispatch log records without
    /// duplicating the tag match logic.
    #[must_use]
    pub const fn is_logging(self) -> bool {
        matches!(
            self,
            MessageCode::ErrorXfer
                | MessageCode::Info
                | MessageCode::Error
                | MessageCode::Warning
                | MessageCode::ErrorSocket
                | MessageCode::ErrorUtf8
                | MessageCode::Log
                | MessageCode::Client
        )
    }

    /// Returns the upstream `MSG_*` identifier associated with this code.
    ///
    /// The mapping mirrors `enum msgcode` in upstream rsync's `rsync.h` so that
    /// diagnostics and tracing can surface the same mnemonic names that C
    /// implementations print through helpers such as `get_mplex_name()`. Keeping
    /// the strings identical makes it easier to compare mixed Rust/C traces when
    /// validating parity.
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

/// Failures encountered while parsing or constructing multiplexed message headers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvelopeError {
    /// Fewer than [`HEADER_LEN`] bytes were provided when attempting to decode a
    /// header.
    TruncatedHeader {
        /// Number of bytes that were available when decoding began.
        actual: usize,
    },
    /// The high tag byte did not include the required [`MPLEX_BASE`] offset.
    InvalidTag(u8),
    /// The encoded message code is not understood by rsync 3.4.1.
    UnknownMessageCode(u8),
    /// The payload length exceeded the representable range.
    OversizedPayload(u32),
}

impl fmt::Display for EnvelopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TruncatedHeader { actual } => {
                write!(
                    f,
                    "multiplexed header truncated: expected {HEADER_LEN} bytes, got {actual}"
                )
            }
            Self::InvalidTag(tag) => {
                write!(f, "multiplexed header contained invalid tag byte {tag}")
            }
            Self::UnknownMessageCode(code) => {
                write!(f, "unknown multiplexed message code {code}")
            }
            Self::OversizedPayload(len) => {
                write!(
                    f,
                    "multiplexed payload length {len} exceeds maximum {MAX_PAYLOAD_LENGTH}"
                )
            }
        }
    }
}

impl std::error::Error for EnvelopeError {}

/// A fully decoded multiplexed message header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MessageHeader {
    code: MessageCode,
    payload_len: u32,
}

impl MessageHeader {
    /// Creates a new header for `code` with the provided payload length.
    ///
    /// The constructor is `const` so golden multiplexed streams can be
    /// assembled at compile time, matching upstream rsync's use of static
    /// header tables for protocol tests.
    pub const fn new(code: MessageCode, payload_len: u32) -> Result<Self, EnvelopeError> {
        if payload_len > MAX_PAYLOAD_LENGTH {
            return Err(EnvelopeError::OversizedPayload(payload_len));
        }

        Ok(Self { code, payload_len })
    }

    /// Parses a header from the beginning of `bytes`.
    pub fn decode(bytes: &[u8]) -> Result<Self, EnvelopeError> {
        if bytes.len() < HEADER_LEN {
            return Err(EnvelopeError::TruncatedHeader {
                actual: bytes.len(),
            });
        }

        let mut encoded = [0u8; HEADER_LEN];
        encoded.copy_from_slice(&bytes[..HEADER_LEN]);
        let raw = u32::from_le_bytes(encoded);
        let tag = (raw >> 24) as u8;
        if tag < MPLEX_BASE {
            return Err(EnvelopeError::InvalidTag(tag));
        }

        let code_value = tag - MPLEX_BASE;
        let code = MessageCode::try_from(code_value)?;
        let payload_len = raw & PAYLOAD_MASK;

        Self::new(code, payload_len)
    }

    /// Encodes this header into the little-endian format used on the wire.
    #[must_use]
    pub const fn encode(self) -> [u8; HEADER_LEN] {
        let tag = (MPLEX_BASE as u32) + (self.code as u32);
        let raw = (tag << 24) | (self.payload_len & PAYLOAD_MASK);
        raw.to_le_bytes()
    }

    /// Returns the decoded message code.
    #[must_use]
    #[inline]
    pub const fn code(self) -> MessageCode {
        self.code
    }

    /// Returns the payload length encoded in the header.
    #[must_use]
    #[inline]
    pub const fn payload_len(self) -> u32 {
        self.payload_len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trips_for_info_message() {
        let header = MessageHeader::new(MessageCode::Info, 123).expect("constructible header");
        let encoded = header.encode();
        let decoded = MessageHeader::decode(&encoded).expect("decode succeeds");
        assert_eq!(decoded, header);
    }

    #[test]
    fn message_header_new_supports_const_contexts() {
        const HEADER: MessageHeader = match MessageHeader::new(MessageCode::Info, 42) {
            Ok(header) => header,
            Err(_) => panic!("valid header should be constructible in const context"),
        };

        assert_eq!(HEADER.code(), MessageCode::Info);
        assert_eq!(HEADER.payload_len(), 42);
    }

    #[test]
    fn message_header_encode_supports_const_contexts() {
        const HEADER: MessageHeader = match MessageHeader::new(MessageCode::Warning, 7) {
            Ok(header) => header,
            Err(_) => panic!("valid header should be constructible in const context"),
        };
        const ENCODED: [u8; HEADER_LEN] = HEADER.encode();

        assert_eq!(ENCODED, HEADER.encode());
    }

    #[test]
    fn decode_rejects_truncated_header() {
        let err = MessageHeader::decode(&[0u8; 2]).unwrap_err();
        assert_eq!(err, EnvelopeError::TruncatedHeader { actual: 2 });
    }

    #[test]
    fn decode_rejects_tag_without_base_offset() {
        let raw = (u32::from(MPLEX_BASE - 1) << 24) | 1;
        let err = MessageHeader::decode(&raw.to_le_bytes()).unwrap_err();
        assert_eq!(err, EnvelopeError::InvalidTag(MPLEX_BASE - 1));
    }

    #[test]
    fn decode_rejects_unknown_message_codes() {
        let unknown_code = 11u8;
        let tag = u32::from(MPLEX_BASE) + u32::from(unknown_code);
        let raw = (tag << 24) | 5;
        let err = MessageHeader::decode(&raw.to_le_bytes()).unwrap_err();
        assert_eq!(err, EnvelopeError::UnknownMessageCode(unknown_code));
    }

    #[test]
    fn encode_uses_little_endian_layout() {
        let payload_len = 0x00A1_B2C3;
        let header =
            MessageHeader::new(MessageCode::Info, payload_len).expect("constructible header");
        let encoded = header.encode();

        let expected_raw =
            ((u32::from(MPLEX_BASE) + u32::from(MessageCode::Info.as_u8())) << 24) | payload_len;
        assert_eq!(encoded, expected_raw.to_le_bytes());
    }

    #[test]
    fn decode_masks_payload_length_to_24_bits() {
        let tag = (u32::from(MPLEX_BASE) + u32::from(MessageCode::Info.as_u8())) << 24;
        let raw = tag | (MAX_PAYLOAD_LENGTH + 1);
        let header =
            MessageHeader::decode(&raw.to_le_bytes()).expect("payload length is masked to 24 bits");
        assert_eq!(header.code(), MessageCode::Info);
        assert_eq!(
            header.payload_len(),
            (MAX_PAYLOAD_LENGTH + 1) & PAYLOAD_MASK
        );
    }

    #[test]
    fn new_rejects_oversized_payloads() {
        let err = MessageHeader::new(MessageCode::Info, MAX_PAYLOAD_LENGTH + 1).unwrap_err();
        assert_eq!(err, EnvelopeError::OversizedPayload(MAX_PAYLOAD_LENGTH + 1));
    }

    #[test]
    fn message_code_variants_round_trip_through_try_from() {
        for &code in MessageCode::all() {
            let raw = code.as_u8();
            let decoded = MessageCode::try_from(raw).expect("known code");
            assert_eq!(decoded, code);
        }
    }

    #[test]
    fn message_code_into_u8_matches_as_u8() {
        for &code in MessageCode::all() {
            let converted: u8 = code.into();
            assert_eq!(converted, code.as_u8());
        }
    }

    #[test]
    fn message_code_from_u8_matches_try_from() {
        for &code in MessageCode::all() {
            let raw = code.as_u8();
            assert_eq!(MessageCode::from_u8(raw), Some(code));
            assert_eq!(MessageCode::try_from(raw).ok(), MessageCode::from_u8(raw));
        }
    }

    #[test]
    fn message_code_from_u8_rejects_unknown_values() {
        assert_eq!(MessageCode::from_u8(11), None);
        assert_eq!(MessageCode::from_u8(0xFF), None);
    }

    #[test]
    fn message_code_from_str_parses_known_names() {
        for &code in MessageCode::all() {
            let parsed: MessageCode = code.name().parse().expect("known name");
            assert_eq!(parsed, code);
        }
    }

    #[test]
    fn message_code_from_str_rejects_unknown_names() {
        let err = "MSG_SOMETHING_ELSE".parse::<MessageCode>().unwrap_err();
        assert_eq!(err.invalid_name(), "MSG_SOMETHING_ELSE");
        assert_eq!(
            err.to_string(),
            "unknown multiplexed message code name: \"MSG_SOMETHING_ELSE\""
        );
    }

    #[test]
    fn message_code_all_is_sorted_by_numeric_value() {
        let all = MessageCode::all();
        for window in all.windows(2) {
            let first = window[0];
            let second = window[1];
            assert!(
                first.as_u8() <= second.as_u8(),
                "MessageCode::all() is not sorted: {:?}",
                all
            );
        }
    }

    #[test]
    fn header_round_trips_for_all_codes_and_sample_lengths() {
        const PAYLOAD_SAMPLES: [u32; 3] = [0, 1, MAX_PAYLOAD_LENGTH];

        for &code in MessageCode::all() {
            for &len in &PAYLOAD_SAMPLES {
                let header = MessageHeader::new(code, len).expect("constructible header");
                let encoded = header.encode();
                let decoded = MessageHeader::decode(&encoded).expect("decode succeeds");
                assert_eq!(decoded.code(), code);
                assert_eq!(decoded.payload_len(), len);
            }
        }
    }

    #[test]
    fn logging_classification_matches_upstream_set() {
        const LOGGING_CODES: &[MessageCode] = &[
            MessageCode::ErrorXfer,
            MessageCode::Info,
            MessageCode::Error,
            MessageCode::Warning,
            MessageCode::ErrorSocket,
            MessageCode::ErrorUtf8,
            MessageCode::Log,
            MessageCode::Client,
        ];

        for &code in MessageCode::all() {
            let expected = LOGGING_CODES.iter().any(|candidate| *candidate == code);
            assert_eq!(code.is_logging(), expected, "mismatch for code {code:?}");
        }
    }

    #[test]
    fn message_code_name_matches_upstream_identifiers() {
        use MessageCode::*;

        let expected = [
            (Data, "MSG_DATA"),
            (ErrorXfer, "MSG_ERROR_XFER"),
            (Info, "MSG_INFO"),
            (Error, "MSG_ERROR"),
            (Warning, "MSG_WARNING"),
            (ErrorSocket, "MSG_ERROR_SOCKET"),
            (Log, "MSG_LOG"),
            (Client, "MSG_CLIENT"),
            (ErrorUtf8, "MSG_ERROR_UTF8"),
            (Redo, "MSG_REDO"),
            (Stats, "MSG_STATS"),
            (IoError, "MSG_IO_ERROR"),
            (IoTimeout, "MSG_IO_TIMEOUT"),
            (NoOp, "MSG_NOOP"),
            (ErrorExit, "MSG_ERROR_EXIT"),
            (Success, "MSG_SUCCESS"),
            (Deleted, "MSG_DELETED"),
            (NoSend, "MSG_NO_SEND"),
        ];

        for &(code, name) in &expected {
            assert_eq!(code.name(), name);
            assert_eq!(code.to_string(), name);
        }
    }

    #[test]
    fn message_code_flush_alias_matches_info() {
        assert_eq!(MessageCode::FLUSH, MessageCode::Info);
        assert_eq!(MessageCode::FLUSH.as_u8(), MessageCode::Info.as_u8());

        let parsed: MessageCode = "MSG_FLUSH".parse().expect("known alias");
        assert_eq!(parsed, MessageCode::Info);
    }
}
