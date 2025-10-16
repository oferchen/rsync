use core::ops::{Deref, DerefMut};
use std::collections::TryReserveError;
use std::io::{self, IoSlice, Read, Write};
use std::slice;

use crate::envelope::{EnvelopeError, HEADER_LEN, MAX_PAYLOAD_LENGTH, MessageCode, MessageHeader};

/// A decoded multiplexed message consisting of the tag and payload bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageFrame {
    code: MessageCode,
    payload: Vec<u8>,
}

impl MessageFrame {
    /// Constructs a frame from a message code and owned payload bytes.
    pub fn new(code: MessageCode, payload: Vec<u8>) -> Result<Self, io::Error> {
        let payload_len = ensure_payload_length(payload.len())?;
        MessageHeader::new(code, payload_len).map_err(map_envelope_error_for_input)?;
        Ok(Self { code, payload })
    }

    /// Returns the message code associated with the frame.
    #[must_use]
    pub const fn code(&self) -> MessageCode {
        self.code
    }

    /// Returns the raw payload bytes carried by the frame.
    #[must_use]
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
    pub fn into_parts(self) -> (MessageCode, Vec<u8>) {
        (self.code, self.payload)
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

/// Sends a multiplexed message to `writer` using the upstream rsync envelope format.
///
/// The payload length is validated against [`MAX_PAYLOAD_LENGTH`], mirroring the
/// 24-bit limit imposed by the C implementation. Violations result in
/// [`io::ErrorKind::InvalidInput`].
pub fn send_msg<W: Write>(writer: &mut W, code: MessageCode, payload: &[u8]) -> io::Result<()> {
    let payload_len = ensure_payload_length(payload.len())?;
    let header = MessageHeader::new(code, payload_len).map_err(map_envelope_error_for_input)?;
    let header_bytes = header.encode();

    if payload.is_empty() {
        writer.write_all(&header_bytes)?;
        return Ok(());
    }

    write_all_vectored(writer, &header_bytes, payload)
}

/// Sends an already constructed [`MessageFrame`] over `writer`.
///
/// The helper mirrors [`send_msg`] but allows callers that already decoded or constructed a
/// [`MessageFrame`] to transmit it without manually splitting the frame into its tag and payload.
/// The payload length has already been validated by [`MessageFrame::new`], so the function simply
/// forwards to [`send_msg`] to reuse the upstream-compatible envelope encoding and vectored write
/// behavior.
pub fn send_frame<W: Write>(writer: &mut W, frame: &MessageFrame) -> io::Result<()> {
    send_msg(writer, frame.code(), frame.payload())
}

/// Receives the next multiplexed message from `reader`.
///
/// The function blocks until the full header and payload are read or an I/O
/// error occurs. Invalid headers surface as [`io::ErrorKind::InvalidData`].
pub fn recv_msg<R: Read>(reader: &mut R) -> io::Result<MessageFrame> {
    let header = read_header(reader)?;
    let len = header.payload_len_usize();

    let mut payload = Vec::new();
    if len != 0 {
        reserve_payload(&mut payload, len)?;
        payload.resize(len, 0);
        reader.read_exact(&mut payload)?;
    }

    MessageFrame::new(header.code(), payload)
}

/// Receives the next multiplexed message into a caller-provided buffer.
///
/// The helper mirrors [`recv_msg`] but avoids allocating a new vector for every
/// frame. The buffer is cleared and then resized to the exact payload length,
/// reusing any existing capacity to satisfy the workspace's buffer reuse
/// guidance. The decoded message code is returned so the caller can dispatch on
/// the frame type while reading the payload from `buffer`.
pub fn recv_msg_into<R: Read>(reader: &mut R, buffer: &mut Vec<u8>) -> io::Result<MessageCode> {
    let header = read_header(reader)?;
    let len = header.payload_len_usize();

    buffer.clear();
    if len != 0 {
        reserve_payload(buffer, len)?;
        buffer.resize(len, 0);
        reader.read_exact(buffer)?;
    }

    Ok(header.code())
}

fn read_header<R: Read>(reader: &mut R) -> io::Result<MessageHeader> {
    let mut header_bytes = [0u8; HEADER_LEN];
    reader.read_exact(&mut header_bytes)?;
    MessageHeader::decode(&header_bytes).map_err(map_envelope_error)
}

fn map_envelope_error(err: EnvelopeError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err)
}

fn map_envelope_error_for_input(err: EnvelopeError) -> io::Error {
    match err {
        EnvelopeError::OversizedPayload(_) => io::Error::new(io::ErrorKind::InvalidInput, err),
        other => map_envelope_error(other),
    }
}

fn invalid_len_error(len: usize) -> io::Error {
    let len = len as u128;
    let max = u128::from(MAX_PAYLOAD_LENGTH);
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("multiplexed payload length {len} exceeds maximum {max}"),
    )
}

fn ensure_payload_length(len: usize) -> io::Result<u32> {
    if len > MAX_PAYLOAD_LENGTH as usize {
        return Err(invalid_len_error(len));
    }

    Ok(len as u32)
}

fn reserve_payload(buffer: &mut Vec<u8>, len: usize) -> io::Result<()> {
    if buffer.capacity() < len {
        let additional = len.saturating_sub(buffer.len());
        debug_assert!(
            additional > 0,
            "reserve_payload called without additional elements"
        );
        buffer
            .try_reserve_exact(additional)
            .map_err(map_allocation_error)?;
    }

    Ok(())
}

fn map_allocation_error(err: TryReserveError) -> io::Error {
    io::Error::new(io::ErrorKind::OutOfMemory, err)
}

fn write_all_vectored<W: Write + ?Sized>(
    writer: &mut W,
    mut header: &[u8],
    mut payload: &[u8],
) -> io::Result<()> {
    while !header.is_empty() || !payload.is_empty() {
        let written = loop {
            let result = if header.is_empty() {
                let slice = IoSlice::new(payload);
                writer.write_vectored(slice::from_ref(&slice))
            } else if payload.is_empty() {
                let slice = IoSlice::new(header);
                writer.write_vectored(slice::from_ref(&slice))
            } else {
                let slices = [IoSlice::new(header), IoSlice::new(payload)];
                writer.write_vectored(&slices)
            };

            match result {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "failed to write multiplexed message",
                    ));
                }
                Ok(written) => break written,
                Err(ref err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
        };

        let mut remaining = written;
        if !header.is_empty() {
            if remaining >= header.len() {
                remaining -= header.len();
                header = &[];
            } else {
                header = &header[remaining..];
                continue;
            }
        }

        if remaining > 0 && !payload.is_empty() {
            if remaining >= payload.len() {
                payload = &[];
            } else {
                payload = &payload[remaining..];
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{HEADER_LEN, MAX_PAYLOAD_LENGTH, MPLEX_BASE};
    use std::collections::VecDeque;
    use std::convert::TryFrom as _;

    #[test]
    fn send_and_receive_round_trip_info_message() {
        let mut buffer = Vec::new();
        send_msg(&mut buffer, MessageCode::Info, b"hello world").expect("send succeeds");

        let mut cursor = io::Cursor::new(buffer);
        let frame = recv_msg(&mut cursor).expect("receive succeeds");
        assert_eq!(frame.code(), MessageCode::Info);
        assert_eq!(frame.payload(), b"hello world");
        assert_eq!(frame.payload_len(), b"hello world".len());
    }

    #[test]
    fn send_msg_emits_upstream_compatible_envelope_for_common_tags() {
        struct Sample {
            code: MessageCode,
            payload: &'static [u8],
        }

        let samples = [
            Sample {
                code: MessageCode::Info,
                payload: b"abc",
            },
            Sample {
                code: MessageCode::Error,
                payload: b"",
            },
            Sample {
                code: MessageCode::Stats,
                payload: &[0x01, 0x02, 0x03, 0x04],
            },
        ];

        for sample in samples {
            let mut buffer = Vec::new();
            send_msg(&mut buffer, sample.code, sample.payload).expect("send succeeds");

            assert_eq!(buffer.len(), HEADER_LEN + sample.payload.len());

            // Upstream encodes multiplexed headers using the MPLEX_BASE constant, adds the message
            // code, shifts the tag into the high byte, and stores the payload length in
            // little-endian order. See rsync 3.4.1's io.c:send_msg().
            let tag = u32::from(MPLEX_BASE) + u32::from(sample.code.as_u8());
            let expected_header = ((tag << 24) | sample.payload.len() as u32).to_le_bytes();

            assert_eq!(&buffer[..HEADER_LEN], &expected_header);
            assert_eq!(&buffer[HEADER_LEN..], sample.payload);
        }
    }

    #[test]
    fn send_msg_prefers_vectored_writes_when_supported() {
        struct RecordingWriter {
            writes: Vec<u8>,
            write_calls: usize,
            vectored_calls: usize,
        }

        impl RecordingWriter {
            fn new() -> Self {
                Self {
                    writes: Vec::new(),
                    write_calls: 0,
                    vectored_calls: 0,
                }
            }
        }

        impl Write for RecordingWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.write_calls += 1;
                self.writes.extend_from_slice(buf);
                Ok(buf.len())
            }

            fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
                self.vectored_calls += 1;
                let mut written = 0;
                for buf in bufs {
                    self.writes.extend_from_slice(buf);
                    written += buf.len();
                }
                Ok(written)
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = RecordingWriter::new();
        let payload = b"payload";
        send_msg(&mut writer, MessageCode::Warning, payload).expect("send succeeds");

        assert_eq!(writer.write_calls, 0, "fallback write() should not be used");
        assert_eq!(writer.vectored_calls, 1, "single vectored call expected");

        let header = MessageHeader::new(MessageCode::Warning, payload.len() as u32).unwrap();
        let mut expected = Vec::from(header.encode());
        expected.extend_from_slice(payload);
        assert_eq!(writer.writes, expected);
    }

    #[test]
    fn send_msg_handles_partial_vectored_writes() {
        struct PartialWriter {
            schedule: VecDeque<usize>,
            written: Vec<u8>,
            write_calls: usize,
        }

        impl PartialWriter {
            fn new(schedule: VecDeque<usize>) -> Self {
                Self {
                    schedule,
                    written: Vec::new(),
                    write_calls: 0,
                }
            }

            fn record(&mut self, mut remaining: usize, bufs: &[IoSlice<'_>]) -> usize {
                let mut produced = 0usize;

                for buf in bufs {
                    if remaining == 0 {
                        break;
                    }
                    if buf.is_empty() {
                        continue;
                    }

                    let take = buf.len().min(remaining);
                    self.written.extend_from_slice(&buf[..take]);
                    produced += take;
                    remaining -= take;

                    if take < buf.len() {
                        break;
                    }
                }

                produced
            }
        }

        impl Write for PartialWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.write_vectored(slice::from_ref(&IoSlice::new(buf)))
            }

            fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
                self.write_calls += 1;

                let allowed = self.schedule.pop_front().unwrap_or(usize::MAX);
                debug_assert!(
                    allowed != 0,
                    "partial writer schedule must contain positive chunk sizes",
                );
                if allowed == 0 {
                    return Ok(0);
                }

                let produced = self.record(allowed, bufs);
                if produced == 0 {
                    return Ok(0);
                }

                Ok(produced)
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = PartialWriter::new(VecDeque::from(vec![2, 1, 3, 2]));
        let payload = b"chunked-payload";
        send_msg(&mut writer, MessageCode::Info, payload).expect("send succeeds");

        let header = MessageHeader::new(MessageCode::Info, payload.len() as u32).unwrap();
        let mut expected = Vec::from(header.encode());
        expected.extend_from_slice(payload);

        assert_eq!(writer.written, expected);
        assert!(
            writer.write_calls >= 4,
            "partial schedule should trigger repeated writes"
        );
    }

    #[test]
    fn reserve_payload_extends_capacity_for_empty_buffers() {
        let mut buffer = Vec::with_capacity(4);
        assert!(buffer.capacity() < 10);

        reserve_payload(&mut buffer, 10).expect("reserve succeeds");

        assert!(
            buffer.capacity() >= 10,
            "capacity {} should be at least required length",
            buffer.capacity()
        );
        assert_eq!(buffer.len(), 0, "reserve must not mutate length");
    }

    #[test]
    fn reserve_payload_extends_relative_to_current_length() {
        let mut buffer = Vec::with_capacity(8);
        buffer.extend_from_slice(&[0u8; 3]);
        assert_eq!(buffer.len(), 3);
        assert!(buffer.capacity() < 12);

        reserve_payload(&mut buffer, 12).expect("reserve succeeds");

        assert!(
            buffer.capacity() >= 12,
            "capacity {} should be at least required length",
            buffer.capacity()
        );
        assert_eq!(buffer.len(), 3, "reserve must not mutate length");
    }

    #[test]
    fn send_msg_retries_on_interrupted_vectored_writes() {
        struct InterruptOnceWriter {
            writes: Vec<u8>,
            vectored_attempts: usize,
            vectored_successes: usize,
            interrupted: bool,
        }

        impl InterruptOnceWriter {
            fn new() -> Self {
                Self {
                    writes: Vec::new(),
                    vectored_attempts: 0,
                    vectored_successes: 0,
                    interrupted: false,
                }
            }
        }

        impl Write for InterruptOnceWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.writes.extend_from_slice(buf);
                Ok(buf.len())
            }

            fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
                self.vectored_attempts += 1;
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "simulated EINTR",
                    ));
                }

                let mut written = 0;
                for buf in bufs {
                    self.writes.extend_from_slice(buf);
                    written += buf.len();
                }
                self.vectored_successes += 1;
                Ok(written)
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = InterruptOnceWriter::new();
        let payload = b"payload";
        send_msg(&mut writer, MessageCode::Info, payload).expect("retry succeeds after EINTR");

        assert!(writer.interrupted, "writer should have seen an interrupt");
        assert_eq!(writer.vectored_attempts, 2, "exactly one retry expected");
        assert_eq!(
            writer.vectored_successes, 1,
            "second attempt should succeed"
        );

        let header = MessageHeader::new(MessageCode::Info, payload.len() as u32).unwrap();
        let mut expected = Vec::from(header.encode());
        expected.extend_from_slice(payload);
        assert_eq!(writer.writes, expected);
    }

    #[test]
    fn send_frame_matches_send_msg_encoding() {
        let payload = b"payload bytes".to_vec();
        let frame = MessageFrame::new(MessageCode::Info, payload).expect("frame is valid");

        let mut via_send_msg = Vec::new();
        send_msg(&mut via_send_msg, frame.code(), frame.payload()).expect("send_msg succeeds");

        let mut via_send_frame = Vec::new();
        send_frame(&mut via_send_frame, &frame).expect("send_frame succeeds");

        assert_eq!(via_send_frame, via_send_msg);
    }

    #[test]
    fn send_frame_handles_empty_payloads() {
        let frame = MessageFrame::new(MessageCode::Error, Vec::new()).expect("frame is valid");

        let mut buffer = Vec::new();
        send_frame(&mut buffer, &frame).expect("send_frame succeeds");

        assert_eq!(buffer.len(), HEADER_LEN);
        assert_eq!(
            &buffer[..HEADER_LEN],
            &MessageHeader::new(frame.code(), 0).unwrap().encode()
        );
    }

    #[test]
    fn send_msg_vectored_handles_partial_writes() {
        struct ChunkedWriter {
            max_chunk: usize,
            data: Vec<u8>,
            calls: usize,
        }

        impl ChunkedWriter {
            fn new(max_chunk: usize) -> Self {
                Self {
                    max_chunk,
                    data: Vec::new(),
                    calls: 0,
                }
            }
        }

        impl Write for ChunkedWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                panic!("send_msg should rely on vectored writes when available");
            }

            fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
                self.calls += 1;
                let mut remaining = self.max_chunk;
                let mut written = 0;

                for buf in bufs {
                    if buf.is_empty() || remaining == 0 {
                        continue;
                    }

                    let take = remaining.min(buf.len());
                    self.data.extend_from_slice(&buf[..take]);
                    remaining -= take;
                    written += take;

                    if take < buf.len() {
                        break;
                    }
                }

                Ok(written)
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = ChunkedWriter::new(2);
        let payload = b"abcdefgh";
        send_msg(&mut writer, MessageCode::Client, payload).expect("send succeeds");

        let header = MessageHeader::new(MessageCode::Client, payload.len() as u32).unwrap();
        let mut expected = Vec::from(header.encode());
        expected.extend_from_slice(payload);
        assert_eq!(writer.data, expected);
        assert!(
            writer.calls > 1,
            "partial writes should require multiple calls"
        );
    }

    #[test]
    fn send_msg_vectored_detects_write_zero() {
        struct ZeroWriter;

        impl Write for ZeroWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                panic!("unexpected write fallback");
            }

            fn write_vectored(&mut self, _bufs: &[IoSlice<'_>]) -> io::Result<usize> {
                Ok(0)
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = ZeroWriter;
        let err = send_msg(&mut writer, MessageCode::Info, b"payload").unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::WriteZero);
        assert_eq!(err.to_string(), "failed to write multiplexed message");
    }

    #[test]
    fn round_trip_zero_length_payload() {
        let mut buffer = Vec::new();
        send_msg(&mut buffer, MessageCode::Warning, b"").expect("send succeeds");

        let mut cursor = io::Cursor::new(buffer);
        let frame = recv_msg(&mut cursor).expect("receive succeeds");
        assert_eq!(frame.code(), MessageCode::Warning);
        assert!(frame.payload().is_empty());
        assert_eq!(frame.payload_len(), 0);
    }

    #[test]
    fn recv_msg_reports_truncated_payload() {
        let header = MessageHeader::new(MessageCode::Warning, 4)
            .expect("header")
            .encode();
        let mut buffer = header.to_vec();
        buffer.extend_from_slice(&[1, 2]);

        let err = recv_msg(&mut io::Cursor::new(buffer)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn recv_msg_reports_truncated_header() {
        let mut cursor = io::Cursor::new([0u8; HEADER_LEN - 1]);
        let err = recv_msg(&mut cursor).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn recv_msg_rejects_unknown_message_codes() {
        let unknown_code = 11u8;
        let tag = u32::from(MPLEX_BASE) + u32::from(unknown_code); // MPLEX_BASE + unknown code
        let raw = (tag << 24).to_le_bytes();
        let err = recv_msg(&mut io::Cursor::new(raw)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn recv_msg_rejects_headers_without_mplex_base() {
        let tag_without_base = u32::from(MPLEX_BASE - 1) << 24;
        let err = recv_msg(&mut io::Cursor::new(tag_without_base.to_le_bytes())).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string()
                .contains("multiplexed header contained invalid tag byte")
        );
    }

    #[test]
    fn recv_msg_into_populates_existing_buffer() {
        let mut stream = Vec::new();
        send_msg(&mut stream, MessageCode::Client, b"payload").expect("send succeeds");

        let mut cursor = io::Cursor::new(stream);
        let mut buffer = Vec::new();
        let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");

        assert_eq!(code, MessageCode::Client);
        assert_eq!(buffer.as_slice(), b"payload");
    }

    #[test]
    fn message_frame_into_parts_returns_code_and_payload_without_clone() {
        let frame = MessageFrame::new(MessageCode::Warning, b"payload".to_vec()).expect("frame");
        let (code, payload) = frame.into_parts();

        assert_eq!(code, MessageCode::Warning);
        assert_eq!(payload, b"payload");
    }

    #[test]
    fn message_frame_into_payload_returns_owned_payload() {
        let payload = b"payload".to_vec();
        let frame = MessageFrame::new(MessageCode::Warning, payload.clone()).expect("frame");

        let owned = frame.into_payload();

        assert_eq!(owned, payload);
    }

    #[test]
    fn recv_msg_into_reuses_buffer_capacity_for_smaller_payloads() {
        let mut stream = Vec::new();
        send_msg(&mut stream, MessageCode::Warning, b"hi").expect("send succeeds");

        let mut cursor = io::Cursor::new(stream);
        let mut buffer = Vec::with_capacity(64);
        buffer.extend_from_slice(&[0u8; 16]);
        let capacity_before = buffer.capacity();

        let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");

        assert_eq!(code, MessageCode::Warning);
        assert_eq!(buffer.as_slice(), b"hi");
        assert_eq!(buffer.capacity(), capacity_before);
    }

    #[test]
    fn recv_msg_into_handles_back_to_back_frames_without_reallocation() {
        let mut stream = Vec::new();
        let first_payload = b"primary payload";
        let second_payload = b"ok";

        send_msg(&mut stream, MessageCode::Info, first_payload).expect("first send");
        send_msg(&mut stream, MessageCode::Warning, second_payload).expect("second send");

        let mut cursor = io::Cursor::new(stream);
        let mut buffer = Vec::new();

        let first_code = recv_msg_into(&mut cursor, &mut buffer).expect("first receive");
        assert_eq!(first_code, MessageCode::Info);
        assert_eq!(buffer.as_slice(), first_payload);

        let capacity_after_first = buffer.capacity();
        let ptr_after_first = buffer.as_ptr();

        let second_code = recv_msg_into(&mut cursor, &mut buffer).expect("second receive");
        assert_eq!(second_code, MessageCode::Warning);
        assert_eq!(buffer.as_slice(), second_payload);
        assert_eq!(buffer.capacity(), capacity_after_first);
        assert_eq!(buffer.as_ptr(), ptr_after_first);
    }

    #[test]
    fn message_frame_payload_mut_allows_in_place_updates() {
        let mut frame = MessageFrame::new(MessageCode::Data, b"payload".to_vec()).expect("frame");

        {
            let payload = frame.payload_mut();
            payload[..4].copy_from_slice(b"data");
        }

        assert_eq!(frame.payload(), b"dataoad");
        assert_eq!(frame.payload_len(), 7);
    }

    #[test]
    fn message_frame_as_ref_exposes_payload_slice() {
        let frame = MessageFrame::new(MessageCode::Warning, b"slice".to_vec()).expect("frame");
        assert_eq!(AsRef::<[u8]>::as_ref(&frame), b"slice");
        let deref: &[u8] = &frame;
        assert_eq!(deref, b"slice");
    }

    #[test]
    fn message_frame_as_mut_allows_mutating_payload_slice() {
        let mut frame = MessageFrame::new(MessageCode::Warning, b"slice".to_vec()).expect("frame");
        let payload = AsMut::<[u8]>::as_mut(&mut frame);
        payload.copy_from_slice(b"PATCH");
        assert_eq!(frame.payload(), b"PATCH");
        {
            let deref: &mut [u8] = &mut frame;
            deref.copy_from_slice(b"lower");
        }
        assert_eq!(frame.payload(), b"lower");
    }

    #[test]
    fn reserve_payload_rejects_capacity_overflow() {
        let mut buffer = Vec::new();
        let err = super::reserve_payload(&mut buffer, usize::MAX).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
    }

    #[test]
    fn recv_msg_into_clears_buffer_for_empty_payloads() {
        let mut stream = Vec::new();
        send_msg(&mut stream, MessageCode::Log, b"").expect("send succeeds");

        let mut cursor = io::Cursor::new(stream);
        let mut buffer = Vec::with_capacity(8);
        buffer.extend_from_slice(b"junk");
        let capacity_before = buffer.capacity();
        let ptr_before = buffer.as_ptr();

        let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");

        assert_eq!(code, MessageCode::Log);
        assert!(buffer.is_empty());
        assert_eq!(buffer.capacity(), capacity_before);
        assert_eq!(buffer.as_ptr(), ptr_before);
    }

    #[test]
    fn send_msg_rejects_oversized_payload() {
        let payload = vec![0u8; (MAX_PAYLOAD_LENGTH + 1) as usize];
        let err = send_msg(&mut io::sink(), MessageCode::Error, &payload).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            err.to_string(),
            format!(
                "multiplexed payload length {} exceeds maximum {}",
                u128::from(MAX_PAYLOAD_LENGTH) + 1,
                u128::from(MAX_PAYLOAD_LENGTH)
            )
        );
    }

    #[test]
    fn message_frame_new_validates_payload_length() {
        let frame = MessageFrame::new(MessageCode::Stats, b"stats".to_vec()).expect("frame");
        assert_eq!(frame.code(), MessageCode::Stats);
        assert_eq!(frame.payload(), b"stats");
    }

    #[test]
    fn message_frame_new_rejects_oversized_payload() {
        let payload = vec![0u8; (MAX_PAYLOAD_LENGTH + 1) as usize];
        let err = MessageFrame::new(MessageCode::Info, payload).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            err.to_string(),
            format!(
                "multiplexed payload length {} exceeds maximum {}",
                u128::from(MAX_PAYLOAD_LENGTH) + 1,
                u128::from(MAX_PAYLOAD_LENGTH)
            )
        );
    }

    #[test]
    fn message_frame_try_from_tuple_constructs_frame() {
        let payload = b"payload".to_vec();
        let frame = MessageFrame::try_from((MessageCode::Warning, payload.clone()))
            .expect("tuple conversion succeeds");
        assert_eq!(frame.code(), MessageCode::Warning);
        assert_eq!(frame.payload(), payload);
    }

    #[test]
    fn message_frame_try_from_tuple_rejects_oversized_payload() {
        let payload = vec![0u8; (MAX_PAYLOAD_LENGTH + 1) as usize];
        let err = MessageFrame::try_from((MessageCode::Info, payload)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains(&format!(
            "multiplexed payload length {}",
            u128::from(MAX_PAYLOAD_LENGTH) + 1
        )));
    }

    #[test]
    fn message_frame_into_tuple_returns_owned_parts() {
        let frame = MessageFrame::new(MessageCode::Deleted, b"done".to_vec()).expect("frame");
        let (code, payload): (MessageCode, Vec<u8>) = frame.into();
        assert_eq!(code, MessageCode::Deleted);
        assert_eq!(payload, b"done");
    }

    #[test]
    fn recv_msg_into_populates_caller_buffer() {
        let mut serialized = Vec::new();
        send_msg(&mut serialized, MessageCode::Warning, b"payload").expect("send succeeds");

        let mut cursor = io::Cursor::new(serialized);
        let mut buffer = Vec::new();
        let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");

        assert_eq!(code, MessageCode::Warning);
        assert_eq!(buffer, b"payload");
    }

    #[test]
    fn recv_msg_into_reuses_existing_capacity() {
        let mut serialized = Vec::new();
        send_msg(&mut serialized, MessageCode::Info, b"hello").expect("send succeeds");

        let mut cursor = io::Cursor::new(serialized);
        let mut buffer = vec![0u8; 8];
        let original_capacity = buffer.capacity();
        let original_ptr = buffer.as_ptr();
        buffer.clear();

        let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");

        assert_eq!(code, MessageCode::Info);
        assert_eq!(buffer, b"hello");
        assert_eq!(buffer.capacity(), original_capacity);
        assert_eq!(buffer.as_ptr(), original_ptr);
    }

    #[test]
    fn recv_msg_into_handles_empty_payload_without_reading() {
        let mut serialized = Vec::new();
        send_msg(&mut serialized, MessageCode::Log, b"").expect("send succeeds");

        let mut cursor = io::Cursor::new(serialized);
        let mut buffer = vec![1u8; 4];
        let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");

        assert_eq!(code, MessageCode::Log);
        assert!(buffer.is_empty());
    }

    #[test]
    fn send_msg_propagates_write_errors() {
        struct FailingWriter;

        impl Write for FailingWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("injected failure"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = FailingWriter;
        let err = send_msg(&mut writer, MessageCode::Info, b"payload").unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert!(err.to_string().contains("injected failure"));
    }

    #[test]
    fn reserve_payload_maps_overflow_to_out_of_memory() {
        let mut buffer = Vec::new();
        let err = super::reserve_payload(&mut buffer, usize::MAX).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
    }

    #[test]
    fn ensure_payload_length_accepts_maximum_payload() {
        let len = MAX_PAYLOAD_LENGTH as usize;
        let validated = super::ensure_payload_length(len).expect("maximum allowed");

        assert_eq!(validated, MAX_PAYLOAD_LENGTH);
    }

    #[test]
    fn ensure_payload_length_rejects_values_above_limit() {
        let len = MAX_PAYLOAD_LENGTH as usize + 1;
        let err = super::ensure_payload_length(len).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            err.to_string(),
            format!(
                "multiplexed payload length {} exceeds maximum {}",
                u128::from(MAX_PAYLOAD_LENGTH) + 1,
                u128::from(MAX_PAYLOAD_LENGTH)
            )
        );
    }
}
