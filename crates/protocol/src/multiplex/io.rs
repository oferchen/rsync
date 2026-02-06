use std::io::{self, IoSlice, Read, Write};
use std::slice;

use logging::debug_log;

use crate::envelope::{HEADER_LEN, MessageCode, MessageHeader};

use super::frame::MessageFrame;
use super::helpers::{
    ensure_payload_length, map_envelope_error, map_envelope_error_for_input, read_payload,
    read_payload_into,
};

/// Sends a multiplexed message to `writer` using the upstream rsync envelope format.
///
/// The payload length is validated against [`crate::MAX_PAYLOAD_LENGTH`], mirroring the
/// 24-bit limit imposed by the C implementation. Violations result in
/// [`io::ErrorKind::InvalidInput`].
pub fn send_msg<W: Write>(writer: &mut W, code: MessageCode, payload: &[u8]) -> io::Result<()> {
    debug_log!(Io, 3, "mux send: code={:?} len={}", code, payload.len());
    let payload_len = ensure_payload_length(payload.len())?;
    let header = MessageHeader::new(code, payload_len).map_err(map_envelope_error_for_input)?;

    write_validated_message(writer, header, payload)
}

/// Sends an already constructed [`MessageFrame`] over `writer`.
///
/// The helper mirrors [`crate::send_msg`] but allows callers that already decoded or constructed a
/// [`MessageFrame`] to transmit it without manually splitting the frame into its tag and payload.
/// The payload length is recomputed through [`MessageFrame::header`] to catch mutations performed via
/// [`::core::ops::DerefMut`], and the upstream-compatible encoding is reused through the same vectored write
/// path. [`MessageFrame::encode_into_writer`] forwards to this helper for ergonomic access from an
/// owned frame.
pub fn send_frame<W: Write>(writer: &mut W, frame: &MessageFrame) -> io::Result<()> {
    let header = frame.header()?;
    write_validated_message(writer, header, frame.payload())
}

/// Sends multiple multiplexed messages in a single vectored write operation.
///
/// This function batches multiple messages into a single `writev` syscall to reduce
/// syscall overhead when sending multiple small messages. Each message is specified
/// as a `(MessageCode, &[u8])` tuple. The payload length for each message is validated
/// against [`crate::MAX_PAYLOAD_LENGTH`].
///
/// # Performance
///
/// This function is significantly more efficient than calling [`send_msg`] repeatedly
/// when sending multiple messages, as it reduces the number of syscalls from N to 1.
///
/// # Errors
///
/// Returns an error if:
/// - Any payload exceeds [`crate::MAX_PAYLOAD_LENGTH`]
/// - The underlying write operation fails
/// - The writer reports writing more bytes than provided
///
/// # Example
///
/// ```no_run
/// use protocol::{send_msgs_vectored, MessageCode};
///
/// let mut buffer = Vec::new();
/// let messages = [
///     (MessageCode::Info, b"first message".as_slice()),
///     (MessageCode::Warning, b"second message".as_slice()),
/// ];
/// send_msgs_vectored(&mut buffer, &messages)?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn send_msgs_vectored<W: Write>(
    writer: &mut W,
    messages: &[(MessageCode, &[u8])],
) -> io::Result<()> {
    if messages.is_empty() {
        return Ok(());
    }

    // Pre-validate all messages and encode headers
    let mut headers = Vec::with_capacity(messages.len());
    for (code, payload) in messages {
        let payload_len = ensure_payload_length(payload.len())?;
        let header =
            MessageHeader::new(*code, payload_len).map_err(map_envelope_error_for_input)?;
        headers.push(header);
    }

    // Encode all headers first to avoid borrow conflicts
    let encoded_headers: Vec<[u8; HEADER_LEN]> = headers.iter().map(|h| h.encode()).collect();

    // Build IoSlice array: alternating headers and payloads
    let mut slices = Vec::with_capacity(messages.len() * 2);
    for (i, (_, payload)) in messages.iter().enumerate() {
        // Add header slice
        slices.push(IoSlice::new(&encoded_headers[i]));

        // Add payload slice only if non-empty
        if !payload.is_empty() {
            slices.push(IoSlice::new(payload));
        }
    }

    // Write all messages in a single vectored operation
    write_all_vectored_slices(writer, &slices)
}

/// Writes all IoSlices using vectored I/O with proper error handling.
///
/// This is similar to `write_all_vectored` but works with a slice of IoSlices
/// rather than just two buffers, allowing batching of multiple messages.
fn write_all_vectored_slices<W: Write + ?Sized>(
    writer: &mut W,
    slices: &[IoSlice<'_>],
) -> io::Result<()> {
    if slices.is_empty() {
        return Ok(());
    }

    let total_bytes: usize = slices.iter().map(|s| s.len()).sum();
    let mut written_total = 0usize;
    let mut use_vectored = true;

    while written_total < total_bytes {
        let remaining = total_bytes - written_total;

        let written = if use_vectored {
            // Calculate which slices still need to be written
            let mut accumulated = 0;
            let mut start_idx = 0;
            let mut offset_in_first = 0;

            for (i, slice) in slices.iter().enumerate() {
                if accumulated + slice.len() > written_total {
                    start_idx = i;
                    offset_in_first = written_total - accumulated;
                    break;
                }
                accumulated += slice.len();
            }

            loop {
                // Build temporary slice view for remaining data
                let mut remaining_slices = Vec::with_capacity(slices.len() - start_idx);

                for (i, slice) in slices[start_idx..].iter().enumerate() {
                    if i == 0 && offset_in_first > 0 {
                        // First slice may be partially written
                        let slice_data = &slice[offset_in_first..];
                        if !slice_data.is_empty() {
                            remaining_slices.push(IoSlice::new(slice_data));
                        }
                    } else {
                        remaining_slices.push(IoSlice::new(slice));
                    }
                }

                match writer.write_vectored(&remaining_slices) {
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "failed to write multiplexed messages",
                        ));
                    }
                    Ok(n) => break n,
                    Err(ref err) if err.kind() == io::ErrorKind::Interrupted => continue,
                    Err(ref err)
                        if err.kind() == io::ErrorKind::Unsupported
                            || err.kind() == io::ErrorKind::InvalidInput =>
                    {
                        use_vectored = false;
                        break 0; // Signal fallback
                    }
                    Err(err) => return Err(err),
                }
            }
        } else {
            // Fallback to sequential writes
            let mut accumulated = 0;
            let mut current_idx = 0;
            let mut offset = 0;

            for (i, slice) in slices.iter().enumerate() {
                if accumulated + slice.len() > written_total {
                    current_idx = i;
                    offset = written_total - accumulated;
                    break;
                }
                accumulated += slice.len();
            }

            loop {
                let current_slice = &slices[current_idx];
                let slice_data = &current_slice[offset..];

                match writer.write(slice_data) {
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "failed to write multiplexed messages",
                        ));
                    }
                    Ok(n) => break n,
                    Err(ref err) if err.kind() == io::ErrorKind::Interrupted => {}
                    Err(err) => return Err(err),
                }
            }
        };

        if !use_vectored && written == 0 {
            // Switch to fallback mode
            continue;
        }

        if written > remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "writer reported writing {written} bytes but only {remaining} bytes remained"
                ),
            ));
        }

        written_total += written;
    }

    Ok(())
}

fn write_validated_message<W: Write + ?Sized>(
    writer: &mut W,
    header: MessageHeader,
    payload: &[u8],
) -> io::Result<()> {
    let header_bytes = header.encode();

    if payload.is_empty() {
        writer.write_all(&header_bytes)?;
        return Ok(());
    }

    write_all_vectored(writer, header_bytes.as_slice(), payload)
}

/// Receives the next multiplexed message from `reader`.
///
/// The function blocks until the full header and payload are read or an I/O
/// error occurs. Invalid headers surface as [`io::ErrorKind::InvalidData`].
pub fn recv_msg<R: Read>(reader: &mut R) -> io::Result<MessageFrame> {
    let header = read_header(reader)?;
    let len = header.payload_len_usize();
    debug_log!(Io, 3, "mux recv: code={:?} len={}", header.code(), len);

    let payload = read_payload(reader, len)?;

    MessageFrame::new(header.code(), payload)
}

/// Receives the next multiplexed message into a caller-provided buffer.
///
/// The helper mirrors [`crate::recv_msg`] but avoids allocating a new vector for every
/// frame. The buffer is cleared and then resized to the exact payload length,
/// reusing any existing capacity to satisfy the workspace's buffer reuse
/// guidance. The decoded message code is returned so the caller can dispatch on
/// the frame type while reading the payload from `buffer`.
pub fn recv_msg_into<R: Read>(reader: &mut R, buffer: &mut Vec<u8>) -> io::Result<MessageCode> {
    let header = read_header(reader)?;
    let len = header.payload_len_usize();

    read_payload_into(reader, buffer, len)?;

    Ok(header.code())
}

fn read_header<R: Read>(reader: &mut R) -> io::Result<MessageHeader> {
    let mut header_bytes = [0u8; HEADER_LEN];
    reader.read_exact(&mut header_bytes)?;
    MessageHeader::decode(&header_bytes).map_err(map_envelope_error)
}

/// Internal helper for vectored writes; exposed for testing.
pub(super) fn write_all_vectored<W: Write + ?Sized>(
    writer: &mut W,
    mut header: &[u8],
    mut payload: &[u8],
) -> io::Result<()> {
    let mut use_vectored = true;

    'outer: while !header.is_empty() || !payload.is_empty() {
        let header_len = header.len();
        let payload_len = payload.len();
        let available = if use_vectored {
            header_len + payload_len
        } else if header_len != 0 {
            header_len
        } else {
            payload_len
        };

        let written = if use_vectored {
            loop {
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
                    Err(ref err)
                        if err.kind() == io::ErrorKind::Unsupported
                            || err.kind() == io::ErrorKind::InvalidInput =>
                    {
                        use_vectored = false;
                        continue 'outer;
                    }
                    Err(err) => return Err(err),
                }
            }
        } else {
            loop {
                let buffer = if !header.is_empty() { header } else { payload };
                match writer.write(buffer) {
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "failed to write multiplexed message",
                        ));
                    }
                    Ok(written) => break written,
                    Err(ref err) if err.kind() == io::ErrorKind::Interrupted => {}
                    Err(err) => return Err(err),
                }
            }
        };

        if written > available {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "writer reported writing {written} bytes but only {available} bytes were provided for multiplexed frame"
                ),
            ));
        }

        let mut remaining = written;
        if header_len != 0 {
            if remaining >= header_len {
                remaining -= header_len;
                header = &[];
            } else {
                header = &header[remaining..];
                continue;
            }
        }

        if remaining > 0 && payload_len != 0 {
            if remaining == payload_len {
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
    use std::io::Cursor;

    // ==================== send_msg tests ====================

    #[test]
    fn send_msg_empty_payload() {
        let mut buffer = Vec::new();
        send_msg(&mut buffer, MessageCode::Data, &[]).unwrap();
        // Header is 4 bytes (code + length)
        assert_eq!(buffer.len(), HEADER_LEN);
    }

    #[test]
    fn send_msg_with_payload() {
        let mut buffer = Vec::new();
        let payload = b"hello world";
        send_msg(&mut buffer, MessageCode::Data, payload).unwrap();
        // Header (4 bytes) + payload (11 bytes)
        assert_eq!(buffer.len(), HEADER_LEN + payload.len());
    }

    #[test]
    fn send_msg_preserves_payload_content() {
        let mut buffer = Vec::new();
        let payload = b"test payload data";
        send_msg(&mut buffer, MessageCode::Data, payload).unwrap();
        // Payload should be at the end after header
        assert_eq!(&buffer[HEADER_LEN..], payload.as_slice());
    }

    #[test]
    fn send_msg_encodes_message_code() {
        let mut buffer = Vec::new();
        send_msg(&mut buffer, MessageCode::ErrorXfer, b"error").unwrap();
        // Decode the header to verify code
        let header = MessageHeader::decode(&buffer[..HEADER_LEN]).unwrap();
        assert_eq!(header.code(), MessageCode::ErrorXfer);
    }

    #[test]
    fn send_msg_encodes_payload_length() {
        let mut buffer = Vec::new();
        let payload = vec![0u8; 1000];
        send_msg(&mut buffer, MessageCode::Data, &payload).unwrap();
        let header = MessageHeader::decode(&buffer[..HEADER_LEN]).unwrap();
        assert_eq!(header.payload_len_usize(), 1000);
    }

    // ==================== recv_msg tests ====================

    #[test]
    fn recv_msg_empty_payload() {
        // Create message with empty payload
        let mut buffer = Vec::new();
        send_msg(&mut buffer, MessageCode::Data, &[]).unwrap();

        let mut reader = Cursor::new(buffer);
        let frame = recv_msg(&mut reader).unwrap();
        assert_eq!(frame.code(), MessageCode::Data);
        assert!(frame.payload().is_empty());
    }

    #[test]
    fn recv_msg_with_payload() {
        let payload = b"test message content";
        let mut buffer = Vec::new();
        send_msg(&mut buffer, MessageCode::Data, payload).unwrap();

        let mut reader = Cursor::new(buffer);
        let frame = recv_msg(&mut reader).unwrap();
        assert_eq!(frame.code(), MessageCode::Data);
        assert_eq!(frame.payload(), payload.as_slice());
    }

    #[test]
    fn recv_msg_different_codes() {
        for code in [
            MessageCode::Data,
            MessageCode::ErrorXfer,
            MessageCode::Info,
            MessageCode::Error,
            MessageCode::Warning,
            MessageCode::Log,
        ] {
            let mut buffer = Vec::new();
            send_msg(&mut buffer, code, b"test").unwrap();

            let mut reader = Cursor::new(buffer);
            let frame = recv_msg(&mut reader).unwrap();
            assert_eq!(frame.code(), code);
        }
    }

    #[test]
    fn recv_msg_eof_returns_error() {
        let mut reader = Cursor::new(Vec::new());
        let result = recv_msg(&mut reader);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn recv_msg_truncated_header_returns_error() {
        // Only 2 bytes of a 4-byte header
        let mut reader = Cursor::new(vec![0u8, 1u8]);
        let result = recv_msg(&mut reader);
        assert!(result.is_err());
    }

    // ==================== recv_msg_into tests ====================

    #[test]
    fn recv_msg_into_reuses_buffer() {
        let payload = b"buffer reuse test";
        let mut send_buffer = Vec::new();
        send_msg(&mut send_buffer, MessageCode::Data, payload).unwrap();

        let mut reader = Cursor::new(send_buffer);
        let mut recv_buffer = Vec::with_capacity(100);
        let code = recv_msg_into(&mut reader, &mut recv_buffer).unwrap();

        assert_eq!(code, MessageCode::Data);
        assert_eq!(recv_buffer, payload.as_slice());
    }

    #[test]
    fn recv_msg_into_clears_buffer() {
        let payload = b"new content";
        let mut send_buffer = Vec::new();
        send_msg(&mut send_buffer, MessageCode::Data, payload).unwrap();

        let mut reader = Cursor::new(send_buffer);
        // Pre-fill buffer with garbage
        let mut recv_buffer = vec![0xFFu8; 50];
        let _code = recv_msg_into(&mut reader, &mut recv_buffer).unwrap();

        // Buffer should only contain the payload
        assert_eq!(recv_buffer.len(), payload.len());
        assert_eq!(recv_buffer, payload.as_slice());
    }

    #[test]
    fn recv_msg_into_empty_payload() {
        let mut send_buffer = Vec::new();
        send_msg(&mut send_buffer, MessageCode::Info, &[]).unwrap();

        let mut reader = Cursor::new(send_buffer);
        let mut recv_buffer = vec![1, 2, 3];
        let code = recv_msg_into(&mut reader, &mut recv_buffer).unwrap();

        assert_eq!(code, MessageCode::Info);
        assert!(recv_buffer.is_empty());
    }

    // ==================== send_frame tests ====================

    #[test]
    fn send_frame_matches_send_msg() {
        let payload = b"frame test";

        // Send via send_msg
        let mut buffer1 = Vec::new();
        send_msg(&mut buffer1, MessageCode::Data, payload).unwrap();

        // Send via send_frame
        let frame = MessageFrame::new(MessageCode::Data, payload.to_vec()).unwrap();
        let mut buffer2 = Vec::new();
        send_frame(&mut buffer2, &frame).unwrap();

        assert_eq!(buffer1, buffer2);
    }

    #[test]
    fn send_frame_empty_payload() {
        let frame = MessageFrame::new(MessageCode::Warning, Vec::new()).unwrap();
        let mut buffer = Vec::new();
        send_frame(&mut buffer, &frame).unwrap();
        assert_eq!(buffer.len(), HEADER_LEN);
    }

    // ==================== roundtrip tests ====================

    #[test]
    fn roundtrip_preserves_message() {
        let original_payload = b"roundtrip test with special chars: \x00\x01\xff";

        let mut buffer = Vec::new();
        send_msg(&mut buffer, MessageCode::Data, original_payload).unwrap();

        let mut reader = Cursor::new(buffer);
        let frame = recv_msg(&mut reader).unwrap();

        assert_eq!(frame.code(), MessageCode::Data);
        assert_eq!(frame.payload(), original_payload.as_slice());
    }

    #[test]
    fn roundtrip_multiple_messages() {
        let messages = [
            (MessageCode::Data, b"first message".as_slice()),
            (MessageCode::Info, b"second message".as_slice()),
            (MessageCode::Warning, b"third message".as_slice()),
        ];

        let mut buffer = Vec::new();
        for (code, payload) in &messages {
            send_msg(&mut buffer, *code, payload).unwrap();
        }

        let mut reader = Cursor::new(buffer);
        for (expected_code, expected_payload) in &messages {
            let frame = recv_msg(&mut reader).unwrap();
            assert_eq!(frame.code(), *expected_code);
            assert_eq!(frame.payload(), *expected_payload);
        }
    }

    #[test]
    fn roundtrip_large_payload() {
        let large_payload = vec![0xABu8; 65535];

        let mut buffer = Vec::new();
        send_msg(&mut buffer, MessageCode::Data, &large_payload).unwrap();

        let mut reader = Cursor::new(buffer);
        let frame = recv_msg(&mut reader).unwrap();

        assert_eq!(frame.payload(), large_payload.as_slice());
    }

    // ==================== write_all_vectored tests ====================

    #[test]
    fn write_all_vectored_empty_slices() {
        let mut buffer = Vec::new();
        write_all_vectored(&mut buffer, &[], &[]).unwrap();
        assert!(buffer.is_empty());
    }

    #[test]
    fn write_all_vectored_header_only() {
        let mut buffer = Vec::new();
        let header = b"HEAD";
        write_all_vectored(&mut buffer, header, &[]).unwrap();
        assert_eq!(buffer, header.as_slice());
    }

    #[test]
    fn write_all_vectored_payload_only() {
        let mut buffer = Vec::new();
        let payload = b"PAYLOAD";
        write_all_vectored(&mut buffer, &[], payload).unwrap();
        assert_eq!(buffer, payload.as_slice());
    }

    #[test]
    fn write_all_vectored_both_slices() {
        let mut buffer = Vec::new();
        let header = b"HEADER";
        let payload = b"PAYLOAD";
        write_all_vectored(&mut buffer, header, payload).unwrap();
        assert_eq!(buffer, b"HEADERPAYLOAD".as_slice());
    }

    // ==================== send_msgs_vectored tests ====================

    #[test]
    fn send_msgs_vectored_empty() {
        let mut buffer = Vec::new();
        send_msgs_vectored(&mut buffer, &[]).unwrap();
        assert!(buffer.is_empty());
    }

    #[test]
    fn send_msgs_vectored_single_message() {
        let mut buffer = Vec::new();
        let messages = [(MessageCode::Info, b"test".as_slice())];
        send_msgs_vectored(&mut buffer, &messages).unwrap();

        // Verify it matches send_msg output
        let mut expected = Vec::new();
        send_msg(&mut expected, MessageCode::Info, b"test").unwrap();
        assert_eq!(buffer, expected);
    }

    #[test]
    fn send_msgs_vectored_multiple_messages() {
        let mut buffer = Vec::new();
        let messages = [
            (MessageCode::Info, b"first".as_slice()),
            (MessageCode::Warning, b"second".as_slice()),
            (MessageCode::Error, b"third".as_slice()),
        ];
        send_msgs_vectored(&mut buffer, &messages).unwrap();

        // Verify by parsing back
        let mut cursor = Cursor::new(&buffer);
        let frame1 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame1.code(), MessageCode::Info);
        assert_eq!(frame1.payload(), b"first");

        let frame2 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame2.code(), MessageCode::Warning);
        assert_eq!(frame2.payload(), b"second");

        let frame3 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame3.code(), MessageCode::Error);
        assert_eq!(frame3.payload(), b"third");
    }

    #[test]
    fn send_msgs_vectored_with_empty_payloads() {
        let mut buffer = Vec::new();
        let messages = [
            (MessageCode::Info, b"".as_slice()),
            (MessageCode::Data, b"content".as_slice()),
            (MessageCode::Warning, b"".as_slice()),
        ];
        send_msgs_vectored(&mut buffer, &messages).unwrap();

        // Verify by parsing back
        let mut cursor = Cursor::new(&buffer);
        let frame1 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame1.code(), MessageCode::Info);
        assert!(frame1.payload().is_empty());

        let frame2 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame2.code(), MessageCode::Data);
        assert_eq!(frame2.payload(), b"content");

        let frame3 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame3.code(), MessageCode::Warning);
        assert!(frame3.payload().is_empty());
    }

    #[test]
    fn send_msgs_vectored_matches_sequential_sends() {
        let messages = [
            (MessageCode::Info, b"first message".as_slice()),
            (MessageCode::Data, b"some data content".as_slice()),
            (MessageCode::Warning, b"warning text".as_slice()),
        ];

        // Send using vectored API
        let mut vectored_buffer = Vec::new();
        send_msgs_vectored(&mut vectored_buffer, &messages).unwrap();

        // Send using sequential API
        let mut sequential_buffer = Vec::new();
        for (code, payload) in &messages {
            send_msg(&mut sequential_buffer, *code, payload).unwrap();
        }

        assert_eq!(vectored_buffer, sequential_buffer);
    }

    #[test]
    fn send_msgs_vectored_large_batch() {
        let mut buffer = Vec::new();

        // Create 100 messages
        let payload = b"test payload data";
        let messages: Vec<_> = (0..100)
            .map(|_| (MessageCode::Data, payload.as_slice()))
            .collect();

        send_msgs_vectored(&mut buffer, &messages).unwrap();

        // Verify all messages can be read back
        let mut cursor = Cursor::new(&buffer);
        for _ in 0..100 {
            let frame = recv_msg(&mut cursor).unwrap();
            assert_eq!(frame.code(), MessageCode::Data);
            assert_eq!(frame.payload(), payload);
        }
    }

    #[test]
    fn send_msgs_vectored_validates_payload_size() {
        use crate::MAX_PAYLOAD_LENGTH;

        let mut buffer = Vec::new();
        let oversized = vec![0u8; (MAX_PAYLOAD_LENGTH + 1) as usize];
        let messages = [(MessageCode::Data, oversized.as_slice())];

        let result = send_msgs_vectored(&mut buffer, &messages);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn send_msgs_vectored_mixed_sizes() {
        let mut buffer = Vec::new();
        let small = b"x";
        let medium = vec![0u8; 1000];
        let large = vec![0u8; 10000];

        let messages = [
            (MessageCode::Info, small.as_slice()),
            (MessageCode::Data, medium.as_slice()),
            (MessageCode::Warning, large.as_slice()),
        ];

        send_msgs_vectored(&mut buffer, &messages).unwrap();

        // Verify by parsing back
        let mut cursor = Cursor::new(&buffer);
        let frame1 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame1.payload().len(), 1);

        let frame2 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame2.payload().len(), 1000);

        let frame3 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame3.payload().len(), 10000);
    }

    // Test with a writer that only supports non-vectored I/O
    #[test]
    fn send_msgs_vectored_fallback_to_sequential() {
        struct NonVectoredWriter {
            buffer: Vec<u8>,
        }

        impl Write for NonVectoredWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.buffer.extend_from_slice(buf);
                Ok(buf.len())
            }

            fn write_vectored(&mut self, _bufs: &[IoSlice<'_>]) -> io::Result<usize> {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "vectored I/O not supported",
                ))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = NonVectoredWriter { buffer: Vec::new() };
        let messages = [
            (MessageCode::Info, b"first".as_slice()),
            (MessageCode::Data, b"second".as_slice()),
        ];

        send_msgs_vectored(&mut writer, &messages).unwrap();

        // Verify output
        let mut cursor = Cursor::new(&writer.buffer);
        let frame1 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame1.code(), MessageCode::Info);
        assert_eq!(frame1.payload(), b"first");

        let frame2 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame2.code(), MessageCode::Data);
        assert_eq!(frame2.payload(), b"second");
    }

    // Test with a writer that performs partial writes
    #[test]
    fn send_msgs_vectored_handles_partial_writes() {
        struct PartialWriter {
            buffer: Vec<u8>,
            chunk_size: usize,
        }

        impl Write for PartialWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                let to_write = buf.len().min(self.chunk_size);
                self.buffer.extend_from_slice(&buf[..to_write]);
                Ok(to_write)
            }

            fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
                let mut remaining = self.chunk_size;
                for buf in bufs {
                    if remaining == 0 {
                        break;
                    }
                    let to_write = buf.len().min(remaining);
                    self.buffer.extend_from_slice(&buf[..to_write]);
                    remaining -= to_write;
                }
                Ok(self.chunk_size - remaining)
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = PartialWriter {
            buffer: Vec::new(),
            chunk_size: 10, // Write only 10 bytes at a time
        };

        let messages = [
            (MessageCode::Info, b"message one".as_slice()),
            (MessageCode::Data, b"message two".as_slice()),
        ];

        send_msgs_vectored(&mut writer, &messages).unwrap();

        // Verify all data was written
        let mut cursor = Cursor::new(&writer.buffer);
        let frame1 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame1.payload(), b"message one");

        let frame2 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame2.payload(), b"message two");
    }

    #[test]
    fn send_msgs_vectored_detects_write_zero() {
        struct ZeroWriter;

        impl Write for ZeroWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Ok(0)
            }

            fn write_vectored(&mut self, _bufs: &[IoSlice<'_>]) -> io::Result<usize> {
                Ok(0)
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = ZeroWriter;
        let messages = [(MessageCode::Info, b"test".as_slice())];

        let result = send_msgs_vectored(&mut writer, &messages);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WriteZero);
    }

    #[test]
    fn send_msgs_vectored_all_message_codes() {
        let mut buffer = Vec::new();
        let payload = b"test";

        for code in MessageCode::ALL {
            buffer.clear();
            let messages = [(code, payload.as_slice())];
            send_msgs_vectored(&mut buffer, &messages).unwrap();

            let mut cursor = Cursor::new(&buffer);
            let frame = recv_msg(&mut cursor).unwrap();
            assert_eq!(frame.code(), code);
            assert_eq!(frame.payload(), payload);
        }
    }

    // ==================== Additional edge case tests ====================

    #[test]
    fn send_msgs_vectored_single_empty_message() {
        let mut buffer = Vec::new();
        let messages = [(MessageCode::Info, b"".as_slice())];
        send_msgs_vectored(&mut buffer, &messages).unwrap();

        // Should only contain header
        assert_eq!(buffer.len(), HEADER_LEN);

        let mut cursor = Cursor::new(&buffer);
        let frame = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame.code(), MessageCode::Info);
        assert!(frame.payload().is_empty());
    }

    #[test]
    fn send_msgs_vectored_all_empty_messages() {
        let mut buffer = Vec::new();
        let messages = [
            (MessageCode::Info, b"".as_slice()),
            (MessageCode::Data, b"".as_slice()),
            (MessageCode::Warning, b"".as_slice()),
        ];
        send_msgs_vectored(&mut buffer, &messages).unwrap();

        // Should only contain 3 headers
        assert_eq!(buffer.len(), HEADER_LEN * 3);

        let mut cursor = Cursor::new(&buffer);
        for (expected_code, _) in &messages {
            let frame = recv_msg(&mut cursor).unwrap();
            assert_eq!(frame.code(), *expected_code);
            assert!(frame.payload().is_empty());
        }
    }

    #[test]
    fn send_msgs_vectored_very_large_payload() {
        let mut buffer = Vec::new();
        // Maximum allowed payload
        let max_size = crate::MAX_PAYLOAD_LENGTH as usize;
        let large_payload = vec![0x42u8; max_size];
        let messages = [(MessageCode::Data, large_payload.as_slice())];

        send_msgs_vectored(&mut buffer, &messages).unwrap();

        let mut cursor = Cursor::new(&buffer);
        let frame = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame.code(), MessageCode::Data);
        assert_eq!(frame.payload().len(), max_size);
        assert!(frame.payload().iter().all(|&b| b == 0x42));
    }

    #[test]
    fn send_msgs_vectored_multiple_large_payloads() {
        let mut buffer = Vec::new();
        // Test multiple large messages
        let large1 = vec![0xAAu8; 50000];
        let large2 = vec![0xBBu8; 60000];
        let large3 = vec![0xCCu8; 70000];

        let messages = [
            (MessageCode::Data, large1.as_slice()),
            (MessageCode::Info, large2.as_slice()),
            (MessageCode::Warning, large3.as_slice()),
        ];

        send_msgs_vectored(&mut buffer, &messages).unwrap();

        let mut cursor = Cursor::new(&buffer);
        let frame1 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame1.payload().len(), 50000);
        assert!(frame1.payload().iter().all(|&b| b == 0xAA));

        let frame2 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame2.payload().len(), 60000);
        assert!(frame2.payload().iter().all(|&b| b == 0xBB));

        let frame3 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame3.payload().len(), 70000);
        assert!(frame3.payload().iter().all(|&b| b == 0xCC));
    }

    #[test]
    fn send_msgs_vectored_validates_all_messages_before_writing() {
        use crate::MAX_PAYLOAD_LENGTH;

        let mut buffer = Vec::new();
        let valid_payload = b"valid";
        let oversized = vec![0u8; (MAX_PAYLOAD_LENGTH + 1) as usize];

        // Second message is invalid, but first is valid
        let messages = [
            (MessageCode::Info, valid_payload.as_slice()),
            (MessageCode::Data, oversized.as_slice()),
        ];

        let result = send_msgs_vectored(&mut buffer, &messages);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);

        // No data should have been written since validation happens first
        assert!(buffer.is_empty());
    }

    #[test]
    fn send_msgs_vectored_binary_payloads() {
        let mut buffer = Vec::new();
        // Test with binary data including null bytes and all byte values
        let binary1 = vec![0u8, 1, 2, 255, 254, 253];
        let binary2: Vec<u8> = (0..=255).collect();
        let binary3 = vec![0xFFu8; 100];

        let messages = [
            (MessageCode::Data, binary1.as_slice()),
            (MessageCode::Info, binary2.as_slice()),
            (MessageCode::Warning, binary3.as_slice()),
        ];

        send_msgs_vectored(&mut buffer, &messages).unwrap();

        let mut cursor = Cursor::new(&buffer);
        let frame1 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame1.payload(), binary1);

        let frame2 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame2.payload(), binary2);

        let frame3 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame3.payload(), binary3);
    }

    #[test]
    fn send_msgs_vectored_handles_interrupted_write() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct InterruptingWriter {
            buffer: Vec<u8>,
            interrupt_count: Arc<AtomicUsize>,
        }

        impl Write for InterruptingWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                if self.interrupt_count.fetch_sub(1, Ordering::SeqCst) > 0 {
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "interrupted"));
                }
                self.buffer.extend_from_slice(buf);
                Ok(buf.len())
            }

            fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
                if self.interrupt_count.fetch_sub(1, Ordering::SeqCst) > 0 {
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "interrupted"));
                }
                let mut total = 0;
                for buf in bufs {
                    self.buffer.extend_from_slice(buf);
                    total += buf.len();
                }
                Ok(total)
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let interrupt_count = Arc::new(AtomicUsize::new(3));
        let mut writer = InterruptingWriter {
            buffer: Vec::new(),
            interrupt_count: interrupt_count.clone(),
        };

        let messages = [
            (MessageCode::Info, b"first".as_slice()),
            (MessageCode::Data, b"second".as_slice()),
        ];

        send_msgs_vectored(&mut writer, &messages).unwrap();

        // Verify all data was written despite interruptions
        let mut cursor = Cursor::new(&writer.buffer);
        let frame1 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame1.payload(), b"first");

        let frame2 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame2.payload(), b"second");
    }

    #[test]
    fn send_msgs_vectored_very_many_messages() {
        let mut buffer = Vec::new();
        // Test with a very large number of messages
        let count = 1000;
        let payload = b"msg";
        let messages: Vec<_> = (0..count)
            .map(|_| (MessageCode::Data, payload.as_slice()))
            .collect();

        send_msgs_vectored(&mut buffer, &messages).unwrap();

        // Verify all messages can be read back
        let mut cursor = Cursor::new(&buffer);
        for _ in 0..count {
            let frame = recv_msg(&mut cursor).unwrap();
            assert_eq!(frame.code(), MessageCode::Data);
            assert_eq!(frame.payload(), payload);
        }
    }

    #[test]
    fn send_msgs_vectored_detects_writer_overreporting() {
        struct OverreportingWriter {
            buffer: Vec<u8>,
        }

        impl Write for OverreportingWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.buffer.extend_from_slice(buf);
                // Report more bytes written than actually provided
                Ok(buf.len() + 100)
            }

            fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
                let mut total = 0;
                for buf in bufs {
                    self.buffer.extend_from_slice(buf);
                    total += buf.len();
                }
                // Report more bytes written than actually provided
                Ok(total + 100)
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = OverreportingWriter { buffer: Vec::new() };
        let messages = [(MessageCode::Info, b"test".as_slice())];

        let result = send_msgs_vectored(&mut writer, &messages);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn send_msgs_vectored_propagates_write_errors() {
        struct FailingWriter;

        impl Write for FailingWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "write failed"))
            }

            fn write_vectored(&mut self, _bufs: &[IoSlice<'_>]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "write failed"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = FailingWriter;
        let messages = [(MessageCode::Info, b"test".as_slice())];

        let result = send_msgs_vectored(&mut writer, &messages);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn send_msgs_vectored_single_byte_payloads() {
        let mut buffer = Vec::new();
        let messages = [
            (MessageCode::Info, b"a".as_slice()),
            (MessageCode::Data, b"b".as_slice()),
            (MessageCode::Warning, b"c".as_slice()),
            (MessageCode::Error, b"d".as_slice()),
        ];

        send_msgs_vectored(&mut buffer, &messages).unwrap();

        let mut cursor = Cursor::new(&buffer);
        let expected = [b"a", b"b", b"c", b"d"];
        for payload in &expected {
            let frame = recv_msg(&mut cursor).unwrap();
            assert_eq!(frame.payload(), payload.as_slice());
        }
    }

    #[test]
    fn send_msgs_vectored_partial_write_across_message_boundary() {
        struct BoundaryWriter {
            buffer: Vec<u8>,
            write_count: usize,
        }

        impl Write for BoundaryWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                // First write: take only part of the data
                self.write_count += 1;
                let to_write = if self.write_count <= 3 {
                    buf.len().min(7) // Write only 7 bytes at a time
                } else {
                    buf.len()
                };
                self.buffer.extend_from_slice(&buf[..to_write]);
                Ok(to_write)
            }

            fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
                // First write: take only part of the data
                self.write_count += 1;
                let limit = if self.write_count <= 3 { 7 } else { usize::MAX };
                let mut written = 0;
                for buf in bufs {
                    if written >= limit {
                        break;
                    }
                    let to_write = (limit - written).min(buf.len());
                    self.buffer.extend_from_slice(&buf[..to_write]);
                    written += to_write;
                    if written >= limit {
                        break;
                    }
                }
                Ok(written)
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = BoundaryWriter {
            buffer: Vec::new(),
            write_count: 0,
        };

        let messages = [
            (MessageCode::Info, b"first message".as_slice()),
            (MessageCode::Data, b"second message".as_slice()),
        ];

        send_msgs_vectored(&mut writer, &messages).unwrap();

        // Verify all data was written correctly
        let mut cursor = Cursor::new(&writer.buffer);
        let frame1 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame1.payload(), b"first message");

        let frame2 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame2.payload(), b"second message");
    }
}
