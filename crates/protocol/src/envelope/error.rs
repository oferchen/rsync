use ::core::fmt;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_display(error: EnvelopeError, expected: String) {
        assert_eq!(error.to_string(), expected);
    }

    #[test]
    fn display_formats_truncated_header() {
        assert_display(
            EnvelopeError::TruncatedHeader { actual: 3 },
            format!("multiplexed header truncated: expected {HEADER_LEN} bytes, got 3"),
        );
    }

    #[test]
    fn display_formats_invalid_tag() {
        assert_display(
            EnvelopeError::InvalidTag(0x12),
            String::from("multiplexed header contained invalid tag byte 18"),
        );
    }

    #[test]
    fn display_formats_unknown_message_code() {
        assert_display(
            EnvelopeError::UnknownMessageCode(0xAA),
            String::from("unknown multiplexed message code 170"),
        );
    }

    #[test]
    fn display_formats_oversized_payload() {
        assert_display(
            EnvelopeError::OversizedPayload(42),
            format!("multiplexed payload length 42 exceeds maximum {MAX_PAYLOAD_LENGTH}"),
        );
    }
}
