//! crates/protocol/src/multiplex/codec.rs
//!
//! Async codec for multiplexed rsync protocol frames using tokio-util.
//!
//! This module provides [`MultiplexCodec`], a [`tokio_util::codec::Decoder`] and
//! [`tokio_util::codec::Encoder`] implementation that handles the rsync multiplexed
//! message format. The codec reads/writes 4-byte little-endian headers followed by
//! variable-length payloads, matching the wire format used by upstream rsync.

use bytes::{Buf, BufMut, BytesMut};
use std::io;
use tokio_util::codec::{Decoder, Encoder};

use crate::envelope::{HEADER_LEN, MAX_PAYLOAD_LENGTH, MessageCode, MessageHeader};

use super::frame::MessageFrame;
use super::helpers::{map_envelope_error, map_envelope_error_for_input};

/// Async codec for multiplexed rsync protocol frames.
///
/// Implements both [`Decoder`] and [`Encoder`] from tokio-util to enable
/// bidirectional multiplexed I/O over async streams. The codec handles:
///
/// - **Decoding**: Reads 4-byte headers to determine message type and payload
///   length, then accumulates payload bytes until the full frame is available.
/// - **Encoding**: Writes the 4-byte header followed by payload bytes.
///
/// # Wire Format
///
/// Each frame consists of:
/// - 4 bytes: Little-endian header
///   - High byte: `MPLEX_BASE` (7) + [`MessageCode`] value
///   - Lower 24 bits: Payload length (max 16MB)
/// - N bytes: Payload data
///
/// # Example
///
/// ```ignore
/// use tokio_util::codec::Framed;
/// use protocol::multiplex::MultiplexCodec;
///
/// async fn example(stream: impl AsyncRead + AsyncWrite + Unpin) {
///     let mut framed = Framed::new(stream, MultiplexCodec::new());
///
///     // Send a frame
///     framed.send(MessageFrame::new(MessageCode::Info, b"hello".to_vec())?).await?;
///
///     // Receive a frame
///     if let Some(frame) = framed.next().await {
///         println!("Received: {:?}", frame?);
///     }
/// }
/// ```
#[derive(Clone, Debug, Default)]
pub struct MultiplexCodec {
    /// Maximum payload size to accept when decoding.
    ///
    /// Defaults to [`MAX_PAYLOAD_LENGTH`] (16MB) matching upstream rsync limits.
    /// Can be reduced for memory-constrained environments.
    max_payload_len: u32,
}

impl MultiplexCodec {
    /// Creates a new codec with default settings.
    ///
    /// The maximum payload length is set to [`MAX_PAYLOAD_LENGTH`] (16MB),
    /// matching upstream rsync's limit.
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_payload_len: MAX_PAYLOAD_LENGTH,
        }
    }

    /// Creates a codec with a custom maximum payload length.
    ///
    /// Use this for memory-constrained environments where accepting 16MB
    /// payloads would be problematic. The limit is clamped to
    /// [`MAX_PAYLOAD_LENGTH`] since the wire format cannot represent
    /// larger values.
    #[must_use]
    pub fn with_max_payload_len(max_payload_len: u32) -> Self {
        Self {
            max_payload_len: max_payload_len.min(MAX_PAYLOAD_LENGTH),
        }
    }

    /// Returns the maximum payload length this codec will accept.
    #[must_use]
    pub const fn max_payload_len(&self) -> u32 {
        self.max_payload_len
    }
}

impl Decoder for MultiplexCodec {
    type Item = MessageFrame;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        // Need at least the header to proceed
        if src.len() < HEADER_LEN {
            return Ok(None);
        }

        // Peek at header without consuming (in case payload isn't ready)
        let header_bytes: [u8; HEADER_LEN] = src[..HEADER_LEN].try_into().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "failed to read header bytes")
        })?;

        let header = MessageHeader::decode(&header_bytes).map_err(map_envelope_error)?;
        let payload_len = header.payload_len();

        // Validate payload length against our limit
        if payload_len > self.max_payload_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "payload length {} exceeds maximum {}",
                    payload_len, self.max_payload_len
                ),
            ));
        }

        let total_len = HEADER_LEN + payload_len as usize;

        // Check if we have the complete frame
        if src.len() < total_len {
            // Reserve space for the rest of the frame
            src.reserve(total_len - src.len());
            return Ok(None);
        }

        // Consume the header
        src.advance(HEADER_LEN);

        // Extract the payload
        let payload = src.split_to(payload_len as usize).to_vec();

        // MessageFrame::new already returns io::Error
        let frame = MessageFrame::new(header.code(), payload)?;

        Ok(Some(frame))
    }
}

impl Encoder<MessageFrame> for MultiplexCodec {
    type Error = io::Error;

    fn encode(&mut self, item: MessageFrame, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let header = item.header()?;
        let payload = item.payload();

        // Reserve space for header + payload
        dst.reserve(HEADER_LEN + payload.len());

        // Write header
        dst.put_slice(&header.encode());

        // Write payload
        dst.put_slice(payload);

        Ok(())
    }
}

/// Encoder implementation for borrowed frames to avoid cloning.
impl Encoder<&MessageFrame> for MultiplexCodec {
    type Error = io::Error;

    fn encode(&mut self, item: &MessageFrame, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let header = item.header()?;
        let payload = item.payload();

        dst.reserve(HEADER_LEN + payload.len());
        dst.put_slice(&header.encode());
        dst.put_slice(payload);

        Ok(())
    }
}

/// Encoder implementation for (MessageCode, &[u8]) tuples for zero-copy sending.
impl Encoder<(MessageCode, &[u8])> for MultiplexCodec {
    type Error = io::Error;

    fn encode(
        &mut self,
        (code, payload): (MessageCode, &[u8]),
        dst: &mut BytesMut,
    ) -> Result<(), Self::Error> {
        let payload_len = payload.len();
        if payload_len > MAX_PAYLOAD_LENGTH as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("payload length {payload_len} exceeds maximum {MAX_PAYLOAD_LENGTH}"),
            ));
        }

        let header =
            MessageHeader::new(code, payload_len as u32).map_err(map_envelope_error_for_input)?;

        dst.reserve(HEADER_LEN + payload_len);
        dst.put_slice(&header.encode());
        dst.put_slice(payload);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_empty_payload() {
        let mut codec = MultiplexCodec::new();
        let mut buf = BytesMut::new();

        // Encode a frame with empty payload
        let header = MessageHeader::new(MessageCode::NoOp, 0).unwrap();
        buf.extend_from_slice(&header.encode());

        let frame = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(frame.code(), MessageCode::NoOp);
        assert!(frame.payload().is_empty());
        assert!(buf.is_empty());
    }

    #[test]
    fn decode_with_payload() {
        let mut codec = MultiplexCodec::new();
        let mut buf = BytesMut::new();

        let payload = b"hello world";
        let header = MessageHeader::new(MessageCode::Info, payload.len() as u32).unwrap();
        buf.extend_from_slice(&header.encode());
        buf.extend_from_slice(payload);

        let frame = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(frame.code(), MessageCode::Info);
        assert_eq!(frame.payload(), payload);
        assert!(buf.is_empty());
    }

    #[test]
    fn decode_incomplete_header() {
        let mut codec = MultiplexCodec::new();
        let mut buf = BytesMut::new();

        buf.extend_from_slice(&[0x07, 0x00]); // Only 2 bytes of header

        let result = codec.decode(&mut buf).unwrap();
        assert!(result.is_none());
        assert_eq!(buf.len(), 2); // Buffer unchanged
    }

    #[test]
    fn decode_incomplete_payload() {
        let mut codec = MultiplexCodec::new();
        let mut buf = BytesMut::new();

        // Header says 10 bytes payload, but only 5 available
        let header = MessageHeader::new(MessageCode::Data, 10).unwrap();
        buf.extend_from_slice(&header.encode());
        buf.extend_from_slice(b"hello");

        let result = codec.decode(&mut buf).unwrap();
        assert!(result.is_none());
        assert_eq!(buf.len(), HEADER_LEN + 5); // Buffer unchanged
    }

    #[test]
    fn decode_multiple_frames() {
        let mut codec = MultiplexCodec::new();
        let mut buf = BytesMut::new();

        // First frame
        let header1 = MessageHeader::new(MessageCode::Info, 3).unwrap();
        buf.extend_from_slice(&header1.encode());
        buf.extend_from_slice(b"abc");

        // Second frame
        let header2 = MessageHeader::new(MessageCode::Data, 2).unwrap();
        buf.extend_from_slice(&header2.encode());
        buf.extend_from_slice(b"xy");

        let frame1 = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(frame1.code(), MessageCode::Info);
        assert_eq!(frame1.payload(), b"abc");

        let frame2 = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(frame2.code(), MessageCode::Data);
        assert_eq!(frame2.payload(), b"xy");

        assert!(buf.is_empty());
    }

    #[test]
    fn decode_rejects_oversized_payload() {
        let mut codec = MultiplexCodec::with_max_payload_len(100);
        let mut buf = BytesMut::new();

        // Header claims 200 bytes payload
        let header = MessageHeader::new(MessageCode::Data, 200).unwrap();
        buf.extend_from_slice(&header.encode());

        let result = codec.decode(&mut buf);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn encode_frame() {
        let mut codec = MultiplexCodec::new();
        let mut buf = BytesMut::new();

        let frame = MessageFrame::new(MessageCode::Info, b"test".to_vec()).unwrap();
        codec.encode(frame, &mut buf).unwrap();

        assert_eq!(buf.len(), HEADER_LEN + 4);

        // Verify we can decode what we encoded
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded.code(), MessageCode::Info);
        assert_eq!(decoded.payload(), b"test");
    }

    #[test]
    fn encode_borrowed_frame() {
        let mut codec = MultiplexCodec::new();
        let mut buf = BytesMut::new();

        let frame = MessageFrame::new(MessageCode::Warning, b"warn".to_vec()).unwrap();
        codec.encode(&frame, &mut buf).unwrap();

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded.code(), MessageCode::Warning);
        assert_eq!(decoded.payload(), b"warn");
    }

    #[test]
    fn encode_tuple() {
        let mut codec = MultiplexCodec::new();
        let mut buf = BytesMut::new();

        codec
            .encode((MessageCode::Error, b"oops".as_slice()), &mut buf)
            .unwrap();

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded.code(), MessageCode::Error);
        assert_eq!(decoded.payload(), b"oops");
    }

    #[test]
    fn roundtrip_all_message_codes() {
        let mut codec = MultiplexCodec::new();

        for code in MessageCode::all() {
            let mut buf = BytesMut::new();
            let payload = format!("payload for {code:?}");

            let frame = MessageFrame::new(*code, payload.as_bytes().to_vec()).unwrap();
            codec.encode(frame, &mut buf).unwrap();

            let decoded = codec.decode(&mut buf).unwrap().unwrap();
            assert_eq!(decoded.code(), *code);
            assert_eq!(decoded.payload(), payload.as_bytes());
        }
    }

    #[test]
    fn max_payload_len_clamped() {
        let codec = MultiplexCodec::with_max_payload_len(u32::MAX);
        assert_eq!(codec.max_payload_len(), MAX_PAYLOAD_LENGTH);
    }
}
