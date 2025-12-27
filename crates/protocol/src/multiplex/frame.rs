use ::core::ops::{Deref, DerefMut};
use std::io::{self, Write};

use crate::envelope::{HEADER_LEN, MessageCode, MessageHeader};

use super::helpers::{ensure_payload_length, map_allocation_error, map_envelope_error_for_input};

/// A decoded multiplexed message consisting of the tag and payload bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageFrame {
    pub(super) code: MessageCode,
    pub(super) payload: Vec<u8>,
}

impl MessageFrame {
    /// Constructs a frame from a message code and owned payload bytes.
    pub fn new(code: MessageCode, payload: Vec<u8>) -> Result<Self, io::Error> {
        let payload_len = ensure_payload_length(payload.len())?;
        MessageHeader::new(code, payload_len).map_err(map_envelope_error_for_input)?;
        Ok(Self { code, payload })
    }

    /// Returns the multiplexed header that matches the current frame contents.
    ///
    /// The helper recomputes the header using the upstream rsync limits so that callers can
    /// serialise the frame without reimplementing the validation logic. It is primarily useful
    /// when the payload has been mutated through [`MessageFrame::payload_mut`], as the payload
    /// length is re-checked each time. The returned header can then be passed to
    /// [`MessageHeader::encode`] or [`MessageHeader::encode_into_slice`] to obtain the on-the-wire
    /// representation.
    ///
    /// # Errors
    ///
    /// Returns [`io::ErrorKind::InvalidInput`] when the payload length exceeds the 24-bit limit
    /// enforced by upstream rsync, mirroring the error that [`crate::send_msg`] would produce in the same
    /// situation.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::{MessageCode, MessageFrame};
    ///
    /// # fn example() -> std::io::Result<()> {
    /// let frame = MessageFrame::new(MessageCode::Info, b"abc".to_vec())?;
    /// let header = frame.header()?;
    ///
    /// assert_eq!(header.code(), MessageCode::Info);
    /// assert_eq!(header.payload_len(), 3);
    /// # Ok(())
    /// # }
    /// # example().unwrap();
    /// ```
    pub fn header(&self) -> io::Result<MessageHeader> {
        let payload_len = ensure_payload_length(self.payload.len())?;
        MessageHeader::new(self.code, payload_len).map_err(map_envelope_error_for_input)
    }

    /// Returns the message code associated with the frame.
    #[must_use]
    #[inline]
    pub const fn code(&self) -> MessageCode {
        self.code
    }

    /// Returns the raw payload bytes carried by the frame.
    #[must_use]
    #[inline]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Returns a mutable view into the payload bytes carried by the frame.
    ///
    /// Upstream rsync occasionally rewrites multiplexed payloads in place (for
    /// example when decrypting or decompressing data blocks) before handing the
    /// buffer to the next pipeline stage. Exposing a mutable slice allows the
    /// Rust implementation to mirror that style without cloning the payload,
    /// keeping buffer reuse intact for larger transfers.
    #[must_use]
    #[inline]
    pub fn payload_mut(&mut self) -> &mut [u8] {
        &mut self.payload
    }

    /// Returns the length of the payload in bytes without exposing the
    /// underlying buffer. Upstream rsync frequently inspects the payload size
    /// when routing multiplexed messages, so providing this accessor helps
    /// mirror those call-sites without allocating or cloning.
    #[must_use]
    #[inline]
    pub fn payload_len(&self) -> usize {
        self.payload.len()
    }

    /// Consumes the frame and returns the owned payload bytes.
    #[must_use]
    #[inline]
    pub fn into_payload(self) -> Vec<u8> {
        self.payload
    }

    /// Consumes the frame and returns the message code together with the owned payload bytes.
    ///
    /// Upstream rsync frequently pattern matches on both the multiplexed tag and the data that
    /// follows. Providing a zero-copy destructor mirrors that style while keeping the Rust
    /// implementation efficient by avoiding payload cloning when the caller needs ownership of
    /// both values.
    #[must_use]
    #[inline]
    pub fn into_parts(self) -> (MessageCode, Vec<u8>) {
        (self.code, self.payload)
    }

    /// Encodes the frame into the caller-provided buffer using the upstream rsync envelope format.
    ///
    /// The buffer is extended with the four-byte multiplexed header followed by the payload bytes
    /// without clearing any existing contents. Capacity is grown with [`Vec::try_reserve`] to avoid
    /// panicking on allocation failure, matching the error semantics used throughout the crate.
    /// This mirrors [`crate::send_frame`], making it convenient for test fixtures and golden transcripts
    /// that need to capture the exact byte representation of a frame without going through an I/O
    /// handle.
    ///
    /// # Examples
    ///
    /// Encode an informational frame and decode it back from the produced bytes.
    ///
    /// ```
    /// # use std::io;
    /// use protocol::{MessageCode, MessageFrame};
    ///
    /// # fn example() -> io::Result<()> {
    /// let frame = MessageFrame::new(MessageCode::Info, b"abc".to_vec())?;
    /// let mut bytes = Vec::new();
    /// frame.encode_into_vec(&mut bytes)?;
    ///
    /// let (decoded, remainder) = MessageFrame::decode_from_slice(&bytes)?;
    /// assert_eq!(decoded.code(), MessageCode::Info);
    /// assert_eq!(decoded.payload(), b"abc");
    /// assert!(remainder.is_empty());
    /// # Ok(())
    /// # }
    /// # assert!(example().is_ok());
    /// ```
    pub fn encode_into_vec(&self, out: &mut Vec<u8>) -> io::Result<()> {
        out.try_reserve(HEADER_LEN + self.payload.len())
            .map_err(map_allocation_error)?;

        let header = self.header()?;
        out.extend_from_slice(&header.encode());
        out.extend_from_slice(&self.payload);

        Ok(())
    }

    /// Writes the frame into an [`io::Write`] implementor using the multiplexed envelope.
    ///
    /// The helper mirrors [`crate::send_frame`] but lives on [`MessageFrame`] so callers that already own
    /// the decoded frame do not need to split it manually into tag and payload slices. The payload
    /// length is revalidated to guard against mutations performed through [`DerefMut`], and the
    /// upstream vectored-write strategy is reused via [`crate::send_msg`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use std::io;
    /// use protocol::{MessageCode, MessageFrame};
    ///
    /// # fn example() -> io::Result<()> {
    /// let frame = MessageFrame::new(MessageCode::Info, b"abc".to_vec())?;
    /// let mut bytes = Vec::new();
    /// frame.encode_into_writer(&mut bytes)?;
    ///
    /// assert_eq!(bytes.len(), 7);
    /// # Ok(())
    /// # }
    /// # assert!(example().is_ok());
    /// ```
    pub fn encode_into_writer<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        super::io::send_frame(writer, self)
    }

    /// Decodes a multiplexed frame from the beginning of `bytes`.
    ///
    /// The function mirrors [`crate::recv_msg`] but operates on an in-memory slice, making it
    /// convenient for test fixtures and golden transcript comparisons that already capture
    /// the full frame without going through `Read`. The returned tuple contains the decoded
    /// frame together with a slice pointing at the remaining, unread bytes. Callers that wish
    /// to parse exactly one frame can invoke [`TryFrom<&[u8]>`] to receive an error when extra
    /// trailing data is present. Use [`crate::BorrowedMessageFrame::decode_from_slice`] to parse without
    /// allocating a new buffer when a borrowed view suffices.
    pub fn decode_from_slice(bytes: &[u8]) -> io::Result<(Self, &[u8])> {
        let (header, payload, remainder) = super::helpers::decode_frame_parts(bytes)?;
        let frame = MessageFrame::new(header.code(), payload.to_vec())?;
        Ok((frame, remainder))
    }
}

impl AsRef<[u8]> for MessageFrame {
    fn as_ref(&self) -> &[u8] {
        self.payload()
    }
}

impl AsMut<[u8]> for MessageFrame {
    fn as_mut(&mut self) -> &mut [u8] {
        self.payload_mut()
    }
}

impl std::convert::TryFrom<(MessageCode, Vec<u8>)> for MessageFrame {
    type Error = io::Error;

    fn try_from((code, payload): (MessageCode, Vec<u8>)) -> Result<Self, Self::Error> {
        Self::new(code, payload)
    }
}

impl From<MessageFrame> for (MessageCode, Vec<u8>) {
    fn from(frame: MessageFrame) -> Self {
        frame.into_parts()
    }
}

impl std::convert::TryFrom<&[u8]> for MessageFrame {
    type Error = io::Error;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        let (frame, remainder) = MessageFrame::decode_from_slice(bytes)?;
        if remainder.is_empty() {
            Ok(frame)
        } else {
            Err(super::helpers::trailing_frame_data_error(remainder.len()))
        }
    }
}

impl Deref for MessageFrame {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.payload()
    }
}

impl DerefMut for MessageFrame {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.payload_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::TryFrom;

    #[test]
    fn message_frame_new_valid() {
        let frame = MessageFrame::new(MessageCode::Info, b"hello".to_vec()).unwrap();
        assert_eq!(frame.code(), MessageCode::Info);
        assert_eq!(frame.payload(), b"hello");
    }

    #[test]
    fn message_frame_new_empty_payload() {
        let frame = MessageFrame::new(MessageCode::Data, vec![]).unwrap();
        assert_eq!(frame.payload_len(), 0);
    }

    #[test]
    fn message_frame_code() {
        let frame = MessageFrame::new(MessageCode::Warning, b"warn".to_vec()).unwrap();
        assert_eq!(frame.code(), MessageCode::Warning);
    }

    #[test]
    fn message_frame_payload() {
        let data = b"test payload data".to_vec();
        let frame = MessageFrame::new(MessageCode::Data, data.clone()).unwrap();
        assert_eq!(frame.payload(), &data);
    }

    #[test]
    fn message_frame_payload_mut() {
        let mut frame = MessageFrame::new(MessageCode::Data, b"abc".to_vec()).unwrap();
        frame.payload_mut()[0] = b'x';
        assert_eq!(frame.payload(), b"xbc");
    }

    #[test]
    fn message_frame_payload_len() {
        let frame = MessageFrame::new(MessageCode::Info, b"12345".to_vec()).unwrap();
        assert_eq!(frame.payload_len(), 5);
    }

    #[test]
    fn message_frame_into_payload() {
        let original = b"test".to_vec();
        let frame = MessageFrame::new(MessageCode::Info, original.clone()).unwrap();
        let payload = frame.into_payload();
        assert_eq!(payload, original);
    }

    #[test]
    fn message_frame_into_parts() {
        let frame = MessageFrame::new(MessageCode::Error, b"err".to_vec()).unwrap();
        let (code, payload) = frame.into_parts();
        assert_eq!(code, MessageCode::Error);
        assert_eq!(payload, b"err");
    }

    #[test]
    fn message_frame_header() {
        let frame = MessageFrame::new(MessageCode::Info, b"abc".to_vec()).unwrap();
        let header = frame.header().unwrap();
        assert_eq!(header.code(), MessageCode::Info);
        assert_eq!(header.payload_len(), 3);
    }

    #[test]
    fn message_frame_encode_into_vec() {
        let frame = MessageFrame::new(MessageCode::Info, b"abc".to_vec()).unwrap();
        let mut bytes = Vec::new();
        frame.encode_into_vec(&mut bytes).unwrap();
        // Header is 4 bytes + 3 bytes payload
        assert_eq!(bytes.len(), 7);
    }

    #[test]
    fn message_frame_encode_into_vec_appends() {
        let frame = MessageFrame::new(MessageCode::Info, b"a".to_vec()).unwrap();
        let mut bytes = vec![0xFF, 0xFF];
        frame.encode_into_vec(&mut bytes).unwrap();
        // Original 2 bytes + header 4 + payload 1
        assert_eq!(bytes.len(), 7);
        assert_eq!(&bytes[0..2], &[0xFF, 0xFF]);
    }

    #[test]
    fn message_frame_encode_into_writer() {
        let frame = MessageFrame::new(MessageCode::Data, b"test".to_vec()).unwrap();
        let mut bytes = Vec::new();
        frame.encode_into_writer(&mut bytes).unwrap();
        assert_eq!(bytes.len(), 8); // 4 header + 4 payload
    }

    #[test]
    fn message_frame_decode_from_slice() {
        let frame = MessageFrame::new(MessageCode::Info, b"abc".to_vec()).unwrap();
        let mut encoded = Vec::new();
        frame.encode_into_vec(&mut encoded).unwrap();

        let (decoded, remainder) = MessageFrame::decode_from_slice(&encoded).unwrap();
        assert_eq!(decoded.code(), MessageCode::Info);
        assert_eq!(decoded.payload(), b"abc");
        assert!(remainder.is_empty());
    }

    #[test]
    fn message_frame_decode_from_slice_with_remainder() {
        let frame = MessageFrame::new(MessageCode::Info, b"a".to_vec()).unwrap();
        let mut encoded = Vec::new();
        frame.encode_into_vec(&mut encoded).unwrap();
        encoded.extend_from_slice(b"extra");

        let (decoded, remainder) = MessageFrame::decode_from_slice(&encoded).unwrap();
        assert_eq!(decoded.code(), MessageCode::Info);
        assert_eq!(decoded.payload(), b"a");
        assert_eq!(remainder, b"extra");
    }

    #[test]
    fn message_frame_roundtrip() {
        let original =
            MessageFrame::new(MessageCode::Warning, b"warning message".to_vec()).unwrap();
        let mut encoded = Vec::new();
        original.encode_into_vec(&mut encoded).unwrap();

        let (decoded, remainder) = MessageFrame::decode_from_slice(&encoded).unwrap();
        assert!(remainder.is_empty());
        assert_eq!(decoded.code(), original.code());
        assert_eq!(decoded.payload(), original.payload());
    }

    #[test]
    fn message_frame_as_ref() {
        let frame = MessageFrame::new(MessageCode::Data, b"data".to_vec()).unwrap();
        let slice: &[u8] = frame.as_ref();
        assert_eq!(slice, b"data");
    }

    #[test]
    fn message_frame_as_mut() {
        let mut frame = MessageFrame::new(MessageCode::Data, b"data".to_vec()).unwrap();
        let slice: &mut [u8] = frame.as_mut();
        slice[0] = b'x';
        assert_eq!(frame.payload(), b"xata");
    }

    #[test]
    fn message_frame_try_from_tuple() {
        let frame = MessageFrame::try_from((MessageCode::Info, b"test".to_vec())).unwrap();
        assert_eq!(frame.code(), MessageCode::Info);
        assert_eq!(frame.payload(), b"test");
    }

    #[test]
    fn message_frame_into_tuple() {
        let frame = MessageFrame::new(MessageCode::Error, b"err".to_vec()).unwrap();
        let (code, payload): (MessageCode, Vec<u8>) = frame.into();
        assert_eq!(code, MessageCode::Error);
        assert_eq!(payload, b"err");
    }

    #[test]
    fn message_frame_try_from_slice() {
        let original = MessageFrame::new(MessageCode::Info, b"hello".to_vec()).unwrap();
        let mut encoded = Vec::new();
        original.encode_into_vec(&mut encoded).unwrap();

        let decoded = MessageFrame::try_from(encoded.as_slice()).unwrap();
        assert_eq!(decoded.code(), MessageCode::Info);
        assert_eq!(decoded.payload(), b"hello");
    }

    #[test]
    fn message_frame_try_from_slice_with_trailing_data_fails() {
        let original = MessageFrame::new(MessageCode::Info, b"a".to_vec()).unwrap();
        let mut encoded = Vec::new();
        original.encode_into_vec(&mut encoded).unwrap();
        encoded.push(0xFF); // Extra trailing byte

        let result = MessageFrame::try_from(encoded.as_slice());
        assert!(result.is_err());
    }

    #[test]
    fn message_frame_deref() {
        let frame = MessageFrame::new(MessageCode::Data, b"abc".to_vec()).unwrap();
        let slice: &[u8] = &frame;
        assert_eq!(slice, b"abc");
    }

    #[test]
    fn message_frame_deref_mut() {
        let mut frame = MessageFrame::new(MessageCode::Data, b"abc".to_vec()).unwrap();
        (&mut *frame)[1] = b'x';
        assert_eq!(frame.payload(), b"axc");
    }

    #[test]
    fn message_frame_clone() {
        let frame = MessageFrame::new(MessageCode::Info, b"test".to_vec()).unwrap();
        let cloned = frame.clone();
        assert_eq!(frame, cloned);
    }

    #[test]
    fn message_frame_debug() {
        let frame = MessageFrame::new(MessageCode::Info, b"x".to_vec()).unwrap();
        let debug = format!("{frame:?}");
        assert!(debug.contains("MessageFrame"));
    }

    #[test]
    fn message_frame_equality() {
        let frame1 = MessageFrame::new(MessageCode::Info, b"a".to_vec()).unwrap();
        let frame2 = MessageFrame::new(MessageCode::Info, b"a".to_vec()).unwrap();
        let frame3 = MessageFrame::new(MessageCode::Info, b"b".to_vec()).unwrap();
        let frame4 = MessageFrame::new(MessageCode::Error, b"a".to_vec()).unwrap();

        assert_eq!(frame1, frame2);
        assert_ne!(frame1, frame3);
        assert_ne!(frame1, frame4);
    }
}
