use ::core::convert::TryFrom;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_valid_header() {
        let header = MessageHeader::new(MessageCode::Info, 100).unwrap();
        assert_eq!(header.code(), MessageCode::Info);
        assert_eq!(header.payload_len(), 100);
    }

    #[test]
    fn new_rejects_oversized_payload() {
        let result = MessageHeader::new(MessageCode::Info, MAX_PAYLOAD_LENGTH + 1);
        assert!(result.is_err());
    }

    #[test]
    fn new_accepts_max_payload() {
        let result = MessageHeader::new(MessageCode::Info, MAX_PAYLOAD_LENGTH);
        assert!(result.is_ok());
    }

    #[test]
    fn new_accepts_zero_payload() {
        let header = MessageHeader::new(MessageCode::Error, 0).unwrap();
        assert_eq!(header.payload_len(), 0);
    }

    #[test]
    fn encode_decode_roundtrip() {
        let original = MessageHeader::new(MessageCode::Warning, 12345).unwrap();
        let bytes = original.encode();
        let decoded = MessageHeader::decode(&bytes).unwrap();
        assert_eq!(decoded.code(), original.code());
        assert_eq!(decoded.payload_len(), original.payload_len());
    }

    #[test]
    fn decode_rejects_truncated_input() {
        let result = MessageHeader::decode(&[0x00, 0x01, 0x02]);
        assert!(matches!(result, Err(EnvelopeError::TruncatedHeader { .. })));
    }

    #[test]
    fn from_raw_rejects_invalid_tag() {
        // A tag less than MPLEX_BASE (7) is invalid
        let raw = 0x00_12_34_56_u32;
        let result = MessageHeader::from_raw(raw);
        assert!(result.is_err());
    }

    #[test]
    fn encode_into_slice_success() {
        let header = MessageHeader::new(MessageCode::Info, 10).unwrap();
        let mut buffer = [0u8; 8];
        let result = header.encode_into_slice(&mut buffer);
        assert!(result.is_ok());
        assert_eq!(&buffer[..4], &header.encode());
    }

    #[test]
    fn encode_into_slice_rejects_small_buffer() {
        let header = MessageHeader::new(MessageCode::Info, 10).unwrap();
        let mut buffer = [0u8; 2];
        let result = header.encode_into_slice(&mut buffer);
        assert!(result.is_err());
    }

    #[test]
    fn encode_raw_and_from_raw_roundtrip() {
        let original = MessageHeader::new(MessageCode::Stats, 999).unwrap();
        let raw = original.encode_raw();
        let decoded = MessageHeader::from_raw(raw).unwrap();
        assert_eq!(decoded.code(), original.code());
        assert_eq!(decoded.payload_len(), original.payload_len());
    }

    #[test]
    fn payload_len_usize_matches() {
        let header = MessageHeader::new(MessageCode::Info, 1000).unwrap();
        assert_eq!(header.payload_len_usize(), 1000);
    }

    #[test]
    fn try_from_array_works() {
        let header = MessageHeader::new(MessageCode::Info, 50).unwrap();
        let bytes = header.encode();
        let decoded: MessageHeader = bytes.try_into().unwrap();
        assert_eq!(decoded.code(), MessageCode::Info);
        assert_eq!(decoded.payload_len(), 50);
    }

    #[test]
    fn try_from_array_ref_works() {
        let header = MessageHeader::new(MessageCode::Error, 25).unwrap();
        let bytes = header.encode();
        let decoded: MessageHeader = (&bytes).try_into().unwrap();
        assert_eq!(decoded.code(), MessageCode::Error);
    }

    #[test]
    fn all_message_codes_encode_decode() {
        for code in MessageCode::ALL {
            let header = MessageHeader::new(code, 100).unwrap();
            let bytes = header.encode();
            let decoded = MessageHeader::decode(&bytes).unwrap();
            assert_eq!(decoded.code(), code);
        }
    }
}
