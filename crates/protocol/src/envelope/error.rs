use core::fmt;

use super::constants::{HEADER_LEN, MAX_PAYLOAD_LENGTH};

/// Failures encountered while parsing or constructing multiplexed message headers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvelopeError {
    /// Fewer than [`HEADER_LEN`] bytes were provided when attempting to encode or decode a header.
    TruncatedHeader {
        /// Number of bytes that were available when the operation began.
        actual: usize,
    },
    /// The high tag byte did not include the required [`super::MPLEX_BASE`] offset.
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
