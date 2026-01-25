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
fn write_all_vectored<W: Write + ?Sized>(
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
}
