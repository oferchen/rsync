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
