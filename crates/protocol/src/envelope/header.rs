use core::convert::TryFrom;

use super::PAYLOAD_MASK;
use super::constants::{HEADER_LEN, MAX_PAYLOAD_LENGTH, MPLEX_BASE};
use super::error::EnvelopeError;
use super::message_code::MessageCode;

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

        Self::from_raw(raw)
    }

    /// Constructs a multiplexed header from the raw 32-bit representation used on the wire.
    #[must_use = "Discard the decoded header only after handling potential validation errors"]
    #[inline]
    pub const fn from_raw(raw: u32) -> Result<Self, EnvelopeError> {
        let tag = (raw >> 24) as u8;
        if tag < MPLEX_BASE {
            return Err(EnvelopeError::InvalidTag(tag));
        }

        let code_value = tag - MPLEX_BASE;
        match MessageCode::from_u8(code_value) {
            Some(code) => Self::new(code, raw & PAYLOAD_MASK),
            None => Err(EnvelopeError::UnknownMessageCode(code_value)),
        }
    }

    /// Encodes this header into the little-endian format used on the wire.
    #[must_use]
    pub const fn encode(self) -> [u8; HEADER_LEN] {
        self.encode_raw().to_le_bytes()
    }

    /// Encodes this header into the caller-provided slice without allocating.
    pub fn encode_into_slice(self, out: &mut [u8]) -> Result<(), EnvelopeError> {
        if out.len() < HEADER_LEN {
            return Err(EnvelopeError::TruncatedHeader { actual: out.len() });
        }

        let raw = self.encode_raw();
        out[..HEADER_LEN].copy_from_slice(&raw.to_le_bytes());
        Ok(())
    }

    /// Returns the raw 32-bit representation used for little-endian multiplexed headers.
    #[must_use]
    #[inline]
    pub const fn encode_raw(self) -> u32 {
        let tag = (MPLEX_BASE as u32) + (self.code as u32);
        (tag << 24) | (self.payload_len & PAYLOAD_MASK)
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

    /// Returns the payload length encoded in the header as a native pointer-sized value.
    #[must_use]
    #[inline]
    pub fn payload_len_usize(self) -> usize {
        #[allow(clippy::assertions_on_constants)]
        {
            debug_assert!(
                usize::BITS >= 24,
                "multiplexed payloads require pointer widths of at least 24 bits"
            );
        }
        self.payload_len as usize
    }
}

impl TryFrom<[u8; HEADER_LEN]> for MessageHeader {
    type Error = EnvelopeError;

    #[inline]
    fn try_from(bytes: [u8; HEADER_LEN]) -> Result<Self, Self::Error> {
        Self::decode(&bytes)
    }
}

impl TryFrom<&[u8; HEADER_LEN]> for MessageHeader {
    type Error = EnvelopeError;

    #[inline]
    fn try_from(bytes: &[u8; HEADER_LEN]) -> Result<Self, Self::Error> {
        Self::decode(bytes)
    }
}
