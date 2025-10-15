use core::convert::TryFrom;
use core::fmt;

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

impl MessageCode {
    /// Returns the numeric representation expected on the wire.
    #[must_use]
    #[inline]
    pub const fn as_u8(self) -> u8 {
        self as u8
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
}

impl TryFrom<u8> for MessageCode {
    type Error = EnvelopeError;

    fn try_from(value: u8) -> Result<Self, EnvelopeError> {
        match value {
            0 => Ok(Self::Data),
            1 => Ok(Self::ErrorXfer),
            2 => Ok(Self::Info),
            3 => Ok(Self::Error),
            4 => Ok(Self::Warning),
            5 => Ok(Self::ErrorSocket),
            6 => Ok(Self::Log),
            7 => Ok(Self::Client),
            8 => Ok(Self::ErrorUtf8),
            9 => Ok(Self::Redo),
            10 => Ok(Self::Stats),
            22 => Ok(Self::IoError),
            33 => Ok(Self::IoTimeout),
            42 => Ok(Self::NoOp),
            86 => Ok(Self::ErrorExit),
            100 => Ok(Self::Success),
            101 => Ok(Self::Deleted),
            102 => Ok(Self::NoSend),
            other => Err(EnvelopeError::UnknownMessageCode(other)),
        }
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
    pub fn new(code: MessageCode, payload_len: u32) -> Result<Self, EnvelopeError> {
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
    pub fn encode(self) -> [u8; HEADER_LEN] {
        let tag = u32::from(MPLEX_BASE) + u32::from(self.code.as_u8());
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
}
