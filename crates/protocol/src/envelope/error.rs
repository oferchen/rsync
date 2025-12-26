use super::constants::{HEADER_LEN, MAX_PAYLOAD_LENGTH};
use thiserror::Error;

/// Failures encountered while parsing or constructing multiplexed message headers.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum EnvelopeError {
    /// Fewer than [`HEADER_LEN`] bytes were provided when attempting to encode or decode a header.
    #[error("multiplexed header truncated: expected {HEADER_LEN} bytes, got {actual}")]
    TruncatedHeader {
        /// Number of bytes that were available when the operation began.
        actual: usize,
    },
    /// The high tag byte did not include the required [`super::MPLEX_BASE`] offset.
    #[error("multiplexed header contained invalid tag byte {0}")]
    InvalidTag(u8),
    /// The encoded message code is not understood by rsync 3.4.1.
    #[error("unknown multiplexed message code {0}")]
    UnknownMessageCode(u8),
    /// The payload length exceeded the representable range.
    #[error("multiplexed payload length {0} exceeds maximum {MAX_PAYLOAD_LENGTH}")]
    OversizedPayload(u32),
}

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
