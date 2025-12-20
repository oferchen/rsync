use super::*;
use std::io::{self, Cursor, Read, Write};

#[test]
fn all_message_codes_round_trip_successfully() {
    for &code in MessageCode::all() {
        let payload = format!("test payload for {:?}", code);
        let mut buffer = Vec::new();
        send_msg(&mut buffer, code, payload.as_bytes()).expect("send succeeds");

        let mut cursor = Cursor::new(buffer);
        let frame = recv_msg(&mut cursor).expect("receive succeeds");
        assert_eq!(frame.code(), code);
        assert_eq!(frame.payload(), payload.as_bytes());
    }
}

#[test]
fn recv_msg_handles_all_valid_message_codes() {
    for &code in MessageCode::all() {
        let header = MessageHeader::new(code, 4).expect("valid header");
        let mut data = Vec::from(header.encode());
        data.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);

        let mut cursor = Cursor::new(data);
        let frame = recv_msg(&mut cursor).expect("receive succeeds");
        assert_eq!(frame.code(), code);
        assert_eq!(frame.payload(), &[0xAA, 0xBB, 0xCC, 0xDD]);
    }
}

#[test]
fn send_msg_handles_maximum_payload_length() {
    let payload = vec![0x42u8; MAX_PAYLOAD_LENGTH as usize];
    let mut buffer = Vec::new();
    send_msg(&mut buffer, MessageCode::Data, &payload).expect("send succeeds");

    let mut cursor = Cursor::new(buffer);
    let frame = recv_msg(&mut cursor).expect("receive succeeds");
    assert_eq!(frame.code(), MessageCode::Data);
    assert_eq!(frame.payload().len(), MAX_PAYLOAD_LENGTH as usize);
    assert!(frame.payload().iter().all(|&b| b == 0x42));
}

#[test]
fn recv_msg_into_handles_maximum_payload_length() {
    let payload = vec![0x55u8; MAX_PAYLOAD_LENGTH as usize];
    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::Stats, &payload).expect("send succeeds");

    let mut cursor = Cursor::new(stream);
    let mut buffer = Vec::new();
    let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");

    assert_eq!(code, MessageCode::Stats);
    assert_eq!(buffer.len(), MAX_PAYLOAD_LENGTH as usize);
    assert!(buffer.iter().all(|&b| b == 0x55));
}

#[test]
fn recv_msg_rejects_all_invalid_message_codes() {
    let invalid_codes = [11u8, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 23, 24, 25];

    for &invalid_code in &invalid_codes {
        let tag = u32::from(MPLEX_BASE) + u32::from(invalid_code);
        let raw = (tag << 24).to_le_bytes();
        let err = recv_msg(&mut Cursor::new(raw)).unwrap_err();
        assert_eq!(
            err.kind(),
            io::ErrorKind::InvalidData,
            "code {} should be rejected",
            invalid_code
        );
    }
}

#[test]
fn send_frame_propagates_write_errors() {
    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("write failed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let frame = MessageFrame::new(MessageCode::Error, b"payload".to_vec()).expect("valid frame");
    let mut writer = FailingWriter;
    let err = send_frame(&mut writer, &frame).unwrap_err();

    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert!(err.to_string().contains("write failed"));
}

#[test]
fn recv_msg_into_multiple_sequential_messages_different_sizes() {
    let mut stream = Vec::new();
    let payloads = [
        (MessageCode::Info, b"short".as_slice()),
        (MessageCode::Warning, b"medium payload here".as_slice()),
        (MessageCode::Error, b"x".as_slice()),
        (
            MessageCode::Data,
            b"this is a longer payload that spans more bytes".as_slice(),
        ),
        (MessageCode::Stats, b"".as_slice()),
    ];

    for (code, payload) in &payloads {
        send_msg(&mut stream, *code, payload).expect("send succeeds");
    }

    let mut cursor = Cursor::new(stream);
    let mut buffer = Vec::new();

    for (expected_code, expected_payload) in &payloads {
        let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");
        assert_eq!(code, *expected_code);
        assert_eq!(buffer.as_slice(), *expected_payload);
    }
}

#[test]
fn recv_msg_handles_single_byte_header_read() {
    struct SingleByteReader {
        data: Vec<u8>,
        offset: usize,
    }

    impl Read for SingleByteReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.offset >= self.data.len() {
                return Ok(0);
            }
            let chunk_size = buf.len().min(1);
            let available = (self.data.len() - self.offset).min(chunk_size);
            buf[..available].copy_from_slice(&self.data[self.offset..self.offset + available]);
            self.offset += available;
            Ok(available)
        }
    }

    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::Client, b"payload").expect("send succeeds");

    let mut reader = SingleByteReader {
        data: stream,
        offset: 0,
    };
    let frame = recv_msg(&mut reader).expect("receive succeeds");
    assert_eq!(frame.code(), MessageCode::Client);
    assert_eq!(frame.payload(), b"payload");
}

#[test]
fn recv_msg_into_handles_single_byte_payload_read() {
    struct SingleByteReader {
        data: Vec<u8>,
        offset: usize,
    }

    impl Read for SingleByteReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.offset >= self.data.len() {
                return Ok(0);
            }
            buf[0] = self.data[self.offset];
            self.offset += 1;
            Ok(1)
        }
    }

    let mut stream = Vec::new();
    let payload = b"byte-by-byte payload";
    send_msg(&mut stream, MessageCode::Warning, payload).expect("send succeeds");

    let mut reader = SingleByteReader {
        data: stream,
        offset: 0,
    };
    let mut buffer = Vec::new();
    let code = recv_msg_into(&mut reader, &mut buffer).expect("receive succeeds");
    assert_eq!(code, MessageCode::Warning);
    assert_eq!(buffer.as_slice(), payload);
}

#[test]
fn send_msg_with_empty_payload_writes_only_header() {
    let mut buffer = Vec::new();
    send_msg(&mut buffer, MessageCode::NoOp, b"").expect("send succeeds");

    assert_eq!(buffer.len(), HEADER_LEN);
    let header = MessageHeader::decode(&buffer).expect("valid header");
    assert_eq!(header.code(), MessageCode::NoOp);
    assert_eq!(header.payload_len(), 0);
}

#[test]
fn recv_msg_validates_header_before_reading_payload() {
    let invalid_tag = (u32::from(MPLEX_BASE - 1) << 24).to_le_bytes();
    let mut data = invalid_tag.to_vec();
    data.extend_from_slice(&[0xFF; 100]);

    let err = recv_msg(&mut Cursor::new(data)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn recv_msg_into_validates_header_before_reading_payload() {
    let unknown_code = 99u8;
    let tag = u32::from(MPLEX_BASE) + u32::from(unknown_code);
    let invalid_header = ((tag << 24) | 10).to_le_bytes();
    let mut data = invalid_header.to_vec();
    data.extend_from_slice(&[0xFF; 10]);

    let mut buffer = Vec::new();
    let err = recv_msg_into(&mut Cursor::new(data), &mut buffer).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(buffer.is_empty(), "buffer should remain empty on error");
}

#[test]
fn send_msg_rejects_payload_exceeding_maximum_by_one() {
    let oversized = vec![0u8; (MAX_PAYLOAD_LENGTH + 1) as usize];
    let err = send_msg(&mut io::sink(), MessageCode::Data, &oversized).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn send_frame_validates_payload_length_via_header() {
    let oversized = vec![0u8; (MAX_PAYLOAD_LENGTH + 1) as usize];
    let err = MessageFrame::new(MessageCode::Data, oversized).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn recv_msg_handles_boundary_payload_lengths() {
    let boundary_lengths = [0, 1, 255, 256, 65535, 65536, MAX_PAYLOAD_LENGTH];

    for &len in &boundary_lengths {
        let payload = vec![0xABu8; len as usize];
        let mut stream = Vec::new();
        send_msg(&mut stream, MessageCode::Data, &payload).expect("send succeeds");

        let mut cursor = Cursor::new(stream);
        let frame = recv_msg(&mut cursor).expect("receive succeeds");
        assert_eq!(frame.payload().len(), len as usize);
        assert!(frame.payload().iter().all(|&b| b == 0xAB));
    }
}

#[test]
fn recv_msg_into_handles_boundary_payload_lengths() {
    let boundary_lengths = [0, 1, 255, 256, 65535, 65536];

    for &len in &boundary_lengths {
        let payload = vec![0xCDu8; len as usize];
        let mut stream = Vec::new();
        send_msg(&mut stream, MessageCode::Info, &payload).expect("send succeeds");

        let mut cursor = Cursor::new(stream);
        let mut buffer = Vec::new();
        let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");
        assert_eq!(code, MessageCode::Info);
        assert_eq!(buffer.len(), len as usize);
        if len > 0 {
            assert!(buffer.iter().all(|&b| b == 0xCD));
        }
    }
}

#[test]
fn recv_msg_reports_partial_header_read_as_unexpected_eof() {
    for truncate_at in 0..HEADER_LEN {
        let header = MessageHeader::new(MessageCode::Warning, 0)
            .expect("valid header")
            .encode();
        let truncated = &header[..truncate_at];

        let err = recv_msg(&mut Cursor::new(truncated)).unwrap_err();
        assert_eq!(
            err.kind(),
            io::ErrorKind::UnexpectedEof,
            "truncation at byte {} should report UnexpectedEof",
            truncate_at
        );
    }
}

#[test]
fn recv_msg_into_preserves_buffer_on_header_read_error() {
    let incomplete_header = [0u8; HEADER_LEN - 1];
    let mut buffer = vec![0xAA, 0xBB, 0xCC];
    let original_capacity = buffer.capacity();
    let original_ptr = buffer.as_ptr();

    let err = recv_msg_into(&mut Cursor::new(incomplete_header), &mut buffer).unwrap_err();

    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(buffer, vec![0xAA, 0xBB, 0xCC]);
    assert_eq!(buffer.capacity(), original_capacity);
    assert_eq!(buffer.as_ptr(), original_ptr);
}

#[test]
fn send_msg_handles_interleaved_message_codes() {
    let mut stream = Vec::new();
    let messages = [
        (MessageCode::Info, b"first".as_slice()),
        (MessageCode::Warning, b"second".as_slice()),
        (MessageCode::Error, b"third".as_slice()),
        (MessageCode::Data, b"fourth".as_slice()),
    ];

    for (code, payload) in &messages {
        send_msg(&mut stream, *code, payload).expect("send succeeds");
    }

    let mut cursor = Cursor::new(stream);
    for (expected_code, expected_payload) in &messages {
        let frame = recv_msg(&mut cursor).expect("receive succeeds");
        assert_eq!(frame.code(), *expected_code);
        assert_eq!(frame.payload(), *expected_payload);
    }
}

#[test]
fn recv_msg_detects_payload_shorter_than_header_claims() {
    for claimed_len in 1..=10 {
        for actual_len in 0..claimed_len {
            let header = MessageHeader::new(MessageCode::Data, claimed_len)
                .expect("valid header")
                .encode();
            let mut data = header.to_vec();
            data.extend_from_slice(&vec![0xFFu8; actual_len as usize]);

            let err = recv_msg(&mut Cursor::new(data)).unwrap_err();
            assert_eq!(
                err.kind(),
                io::ErrorKind::UnexpectedEof,
                "claimed={} actual={} should report UnexpectedEof",
                claimed_len,
                actual_len
            );
        }
    }
}

#[test]
fn recv_msg_into_detects_payload_shorter_than_header_claims() {
    let header = MessageHeader::new(MessageCode::Stats, 8)
        .expect("valid header")
        .encode();
    let mut data = header.to_vec();
    data.extend_from_slice(&[0x01, 0x02, 0x03]);

    let mut buffer = Vec::new();
    let err = recv_msg_into(&mut Cursor::new(data), &mut buffer).unwrap_err();

    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(
        err.to_string(),
        "multiplexed payload truncated: expected 8 bytes but received 3"
    );
    assert_eq!(buffer, vec![0x01, 0x02, 0x03]);
}

#[test]
fn send_frame_matches_send_msg_for_all_message_codes() {
    for &code in MessageCode::all() {
        let payload = format!("test-{:?}", code).into_bytes();
        let frame = MessageFrame::new(code, payload.clone()).expect("valid frame");

        let mut via_send_msg = Vec::new();
        send_msg(&mut via_send_msg, code, &payload).expect("send_msg succeeds");

        let mut via_send_frame = Vec::new();
        send_frame(&mut via_send_frame, &frame).expect("send_frame succeeds");

        assert_eq!(
            via_send_frame, via_send_msg,
            "send_frame and send_msg should produce identical output for {:?}",
            code
        );
    }
}

#[test]
fn recv_msg_into_clears_and_populates_for_varying_sizes() {
    let sizes = [0, 1, 10, 100, 1000];
    let mut buffer = Vec::new();

    for &size in &sizes {
        let payload = vec![0x88u8; size];
        let mut stream = Vec::new();
        send_msg(&mut stream, MessageCode::Client, &payload).expect("send succeeds");

        let mut cursor = Cursor::new(stream);
        let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");

        assert_eq!(code, MessageCode::Client);
        assert_eq!(buffer.len(), size);
        if size > 0 {
            assert!(buffer.iter().all(|&b| b == 0x88));
        }
    }
}

#[test]
fn send_msg_handles_flush_alias() {
    let mut buffer = Vec::new();
    send_msg(&mut buffer, MessageCode::FLUSH, b"flushing").expect("send succeeds");

    let mut cursor = Cursor::new(buffer);
    let frame = recv_msg(&mut cursor).expect("receive succeeds");
    assert_eq!(frame.code(), MessageCode::Info);
    assert_eq!(frame.payload(), b"flushing");
}
