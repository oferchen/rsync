use super::*;
use std::io::{self, Cursor};

/// Tests for multiplexing correctness covering message framing, type distinction,
/// large messages, and error message parsing.

// =============================================================================
// Message Framing Correctness Tests
// =============================================================================

#[test]
fn message_framing_single_message_roundtrip() {
    let mut buffer = Vec::new();
    send_msg(&mut buffer, MessageCode::Info, b"test message").expect("send succeeds");

    let mut cursor = Cursor::new(buffer);
    let frame = recv_msg(&mut cursor).expect("receive succeeds");

    assert_eq!(frame.code(), MessageCode::Info);
    assert_eq!(frame.payload(), b"test message");
}

#[test]
fn message_framing_multiple_messages_in_sequence() {
    let messages = [
        (MessageCode::Info, b"first" as &[u8]),
        (MessageCode::Warning, b"second"),
        (MessageCode::Error, b"third"),
        (MessageCode::Data, b"fourth"),
    ];

    let mut buffer = Vec::new();
    for (code, payload) in &messages {
        send_msg(&mut buffer, *code, payload).expect("send succeeds");
    }

    let mut cursor = Cursor::new(buffer);
    for (expected_code, expected_payload) in &messages {
        let frame = recv_msg(&mut cursor).expect("receive succeeds");
        assert_eq!(frame.code(), *expected_code);
        assert_eq!(frame.payload(), *expected_payload);
    }
}

#[test]
fn message_framing_preserves_exact_payload_boundaries() {
    // Test that payload boundaries are preserved even with varying lengths
    let payloads = [
        b"" as &[u8],
        b"a",
        b"ab",
        b"abc",
        b"abcd",
        b"abcdefghij",
        &[0u8; 100],
        &[0xFFu8; 255],
    ];

    let mut buffer = Vec::new();
    for payload in &payloads {
        send_msg(&mut buffer, MessageCode::Data, payload).expect("send succeeds");
    }

    let mut cursor = Cursor::new(buffer);
    for expected_payload in &payloads {
        let frame = recv_msg(&mut cursor).expect("receive succeeds");
        assert_eq!(frame.payload(), *expected_payload);
        assert_eq!(frame.payload_len(), expected_payload.len());
    }
}

#[test]
fn message_framing_empty_payload_has_correct_header() {
    let mut buffer = Vec::new();
    send_msg(&mut buffer, MessageCode::NoOp, b"").expect("send succeeds");

    assert_eq!(buffer.len(), HEADER_LEN);

    let tag = u32::from(MPLEX_BASE) + u32::from(MessageCode::NoOp.as_u8());
    let expected_header = (tag << 24).to_le_bytes();
    assert_eq!(&buffer[..HEADER_LEN], &expected_header);
}

#[test]
fn message_framing_header_encodes_payload_length_correctly() {
    let test_cases = [
        (0usize, b"" as &[u8]),
        (1, b"x"),
        (255, &[0u8; 255]),
        (256, &[0u8; 256]),
        (1024, &[0u8; 1024]),
        (65535, &[0u8; 65535]),
    ];

    for (expected_len, payload) in test_cases {
        let mut buffer = Vec::new();
        send_msg(&mut buffer, MessageCode::Data, payload).expect("send succeeds");

        // Extract length from header (lower 3 bytes of little-endian u32)
        let header_word = u32::from_le_bytes([
            buffer[0], buffer[1], buffer[2], buffer[3]
        ]);
        let encoded_len = (header_word & 0x00FFFFFF) as usize;

        assert_eq!(encoded_len, expected_len, "payload length {expected_len} not encoded correctly");
    }
}

#[test]
fn message_framing_decode_from_slice_handles_exact_frame() {
    let frame = MessageFrame::new(MessageCode::Stats, b"stats data".to_vec()).expect("frame");
    let mut encoded = Vec::new();
    frame.encode_into_vec(&mut encoded).expect("encode succeeds");

    let (decoded, remainder) = MessageFrame::decode_from_slice(&encoded).expect("decode succeeds");

    assert_eq!(decoded.code(), MessageCode::Stats);
    assert_eq!(decoded.payload(), b"stats data");
    assert!(remainder.is_empty());
}

#[test]
fn message_framing_decode_from_slice_splits_multiple_frames() {
    let frame1 = MessageFrame::new(MessageCode::Info, b"first".to_vec()).expect("frame1");
    let frame2 = MessageFrame::new(MessageCode::Warning, b"second".to_vec()).expect("frame2");

    let mut buffer = Vec::new();
    frame1.encode_into_vec(&mut buffer).expect("encode1");
    frame2.encode_into_vec(&mut buffer).expect("encode2");

    let (decoded1, remainder1) = MessageFrame::decode_from_slice(&buffer).expect("decode1");
    assert_eq!(decoded1.code(), MessageCode::Info);
    assert_eq!(decoded1.payload(), b"first");

    let (decoded2, remainder2) = MessageFrame::decode_from_slice(remainder1).expect("decode2");
    assert_eq!(decoded2.code(), MessageCode::Warning);
    assert_eq!(decoded2.payload(), b"second");
    assert!(remainder2.is_empty());
}

#[test]
fn message_framing_vectored_send_maintains_frame_boundaries() {
    let messages = [
        (MessageCode::Info, b"msg1" as &[u8]),
        (MessageCode::Warning, b"msg2"),
        (MessageCode::Error, b"msg3"),
    ];

    let mut buffer = Vec::new();
    send_msgs_vectored(&mut buffer, &messages).expect("vectored send succeeds");

    let mut cursor = Cursor::new(buffer);
    for (expected_code, expected_payload) in messages {
        let frame = recv_msg(&mut cursor).expect("receive succeeds");
        assert_eq!(frame.code(), expected_code);
        assert_eq!(frame.payload(), expected_payload);
    }
}

// =============================================================================
// Message Type Distinction Tests
// =============================================================================

#[test]
fn message_types_all_codes_roundtrip_correctly() {
    for code in MessageCode::ALL {
        let payload = format!("payload for {}", code.name());

        let mut buffer = Vec::new();
        send_msg(&mut buffer, code, payload.as_bytes()).expect("send succeeds");

        let mut cursor = Cursor::new(buffer);
        let frame = recv_msg(&mut cursor).expect("receive succeeds");

        assert_eq!(frame.code(), code, "code mismatch for {}", code.name());
        assert_eq!(frame.payload(), payload.as_bytes());
    }
}

#[test]
fn message_types_data_vs_log_messages_distinguished() {
    let test_cases = [
        (MessageCode::Data, false, "data"),
        (MessageCode::Info, true, "info"),
        (MessageCode::Error, true, "error"),
        (MessageCode::Warning, true, "warning"),
        (MessageCode::Stats, false, "stats"),
        (MessageCode::Success, false, "success"),
    ];

    for (code, is_log, name) in test_cases {
        assert_eq!(
            code.is_logging(),
            is_log,
            "{name} should {}be a logging message",
            if is_log { "" } else { "not " }
        );
    }
}

#[test]
fn message_types_error_variants_are_distinct() {
    let error_codes = [
        MessageCode::Error,
        MessageCode::ErrorXfer,
        MessageCode::ErrorSocket,
        MessageCode::ErrorUtf8,
        MessageCode::ErrorExit,
    ];

    // Verify each error code is distinct
    for (i, code1) in error_codes.iter().enumerate() {
        for (j, code2) in error_codes.iter().enumerate() {
            if i != j {
                assert_ne!(
                    code1.as_u8(),
                    code2.as_u8(),
                    "{} and {} must have different values",
                    code1.name(),
                    code2.name()
                );
            }
        }
    }

    // Verify they roundtrip correctly
    for code in error_codes {
        let mut buffer = Vec::new();
        send_msg(&mut buffer, code, b"error message").expect("send succeeds");

        let mut cursor = Cursor::new(buffer);
        let frame = recv_msg(&mut cursor).expect("receive succeeds");

        assert_eq!(frame.code(), code);
    }
}

#[test]
fn message_types_control_messages_have_unique_codes() {
    let control_codes = [
        MessageCode::Redo,
        MessageCode::Stats,
        MessageCode::IoError,
        MessageCode::IoTimeout,
        MessageCode::NoOp,
        MessageCode::Success,
        MessageCode::Deleted,
        MessageCode::NoSend,
    ];

    // Verify all control codes are unique
    for (i, code1) in control_codes.iter().enumerate() {
        for (j, code2) in control_codes.iter().enumerate() {
            if i != j {
                assert_ne!(code1, code2, "control codes must be unique");
            }
        }
    }
}

#[test]
fn message_types_mixed_sequence_preserves_types() {
    let sequence = [
        (MessageCode::Data, b"file data" as &[u8]),
        (MessageCode::Info, b"progress info"),
        (MessageCode::Data, b"more file data"),
        (MessageCode::Warning, b"a warning"),
        (MessageCode::Data, b"final data"),
        (MessageCode::Success, b"done"),
    ];

    let mut buffer = Vec::new();
    for (code, payload) in &sequence {
        send_msg(&mut buffer, *code, payload).expect("send succeeds");
    }

    let mut cursor = Cursor::new(buffer);
    for (expected_code, expected_payload) in &sequence {
        let frame = recv_msg(&mut cursor).expect("receive succeeds");
        assert_eq!(frame.code(), *expected_code);
        assert_eq!(frame.payload(), *expected_payload);
    }
}

#[test]
fn message_types_log_codes_convert_to_message_codes() {
    let conversions = [
        (LogCode::ErrorXfer, MessageCode::ErrorXfer),
        (LogCode::Info, MessageCode::Info),
        (LogCode::Error, MessageCode::Error),
        (LogCode::Warning, MessageCode::Warning),
        (LogCode::ErrorSocket, MessageCode::ErrorSocket),
        (LogCode::Log, MessageCode::Log),
        (LogCode::Client, MessageCode::Client),
        (LogCode::ErrorUtf8, MessageCode::ErrorUtf8),
    ];

    for (log_code, expected_msg_code) in conversions {
        let msg_code = MessageCode::from_log_code(log_code);
        assert_eq!(
            msg_code,
            Some(expected_msg_code),
            "LogCode::{:?} should convert to MessageCode::{}",
            log_code,
            expected_msg_code.name()
        );
    }
}

#[test]
fn message_types_flush_alias_equals_info() {
    assert_eq!(MessageCode::FLUSH, MessageCode::Info);
    assert_eq!(MessageCode::FLUSH.as_u8(), MessageCode::Info.as_u8());

    // Verify they serialize identically
    let mut buffer1 = Vec::new();
    send_msg(&mut buffer1, MessageCode::FLUSH, b"test").expect("send flush");

    let mut buffer2 = Vec::new();
    send_msg(&mut buffer2, MessageCode::Info, b"test").expect("send info");

    assert_eq!(buffer1, buffer2);
}

// =============================================================================
// Large Message Handling Tests
// =============================================================================

#[test]
fn large_messages_32kb_payload_roundtrip() {
    let payload = vec![0xAAu8; 32 * 1024];

    let mut buffer = Vec::new();
    send_msg(&mut buffer, MessageCode::Data, &payload).expect("send succeeds");

    let mut cursor = Cursor::new(buffer);
    let frame = recv_msg(&mut cursor).expect("receive succeeds");

    assert_eq!(frame.payload_len(), 32 * 1024);
    assert_eq!(frame.payload(), payload.as_slice());
}

#[test]
fn large_messages_1mb_payload_roundtrip() {
    let payload = vec![0xBBu8; 1024 * 1024];

    let mut buffer = Vec::new();
    send_msg(&mut buffer, MessageCode::Data, &payload).expect("send succeeds");

    let mut cursor = Cursor::new(buffer);
    let frame = recv_msg(&mut cursor).expect("receive succeeds");

    assert_eq!(frame.payload_len(), 1024 * 1024);
    assert!(frame.payload().iter().all(|&b| b == 0xBB));
}

#[test]
fn large_messages_max_payload_length_accepted() {
    let max_size = MAX_PAYLOAD_LENGTH as usize;
    let payload = vec![0xCCu8; max_size];

    let mut buffer = Vec::new();
    send_msg(&mut buffer, MessageCode::Data, &payload).expect("send succeeds");

    let mut cursor = Cursor::new(buffer);
    let frame = recv_msg(&mut cursor).expect("receive succeeds");

    assert_eq!(frame.payload_len(), max_size);
}

#[test]
fn large_messages_just_over_max_rejected() {
    let oversized = vec![0u8; (MAX_PAYLOAD_LENGTH + 1) as usize];

    let err = send_msg(&mut Vec::new(), MessageCode::Data, &oversized).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("exceeds maximum"));
}

#[test]
fn large_messages_frame_construction_validates_size() {
    let oversized = vec![0u8; (MAX_PAYLOAD_LENGTH + 1) as usize];

    let err = MessageFrame::new(MessageCode::Data, oversized).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn large_messages_recv_into_reuses_buffer_capacity() {
    let large_payload = vec![0xDDu8; 64 * 1024];

    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::Data, &large_payload).expect("send succeeds");

    let mut cursor = Cursor::new(stream);
    let mut buffer = Vec::with_capacity(128 * 1024);
    let capacity_before = buffer.capacity();

    recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");

    assert_eq!(buffer.len(), 64 * 1024);
    assert_eq!(buffer.capacity(), capacity_before, "should reuse existing capacity");
}

#[test]
fn large_messages_multiple_large_messages_in_sequence() {
    let sizes = [16 * 1024, 32 * 1024, 64 * 1024, 128 * 1024];

    let mut buffer = Vec::new();
    for (i, size) in sizes.iter().enumerate() {
        let payload = vec![(i as u8); *size];
        send_msg(&mut buffer, MessageCode::Data, &payload).expect("send succeeds");
    }

    let mut cursor = Cursor::new(buffer);
    for (i, size) in sizes.iter().enumerate() {
        let frame = recv_msg(&mut cursor).expect("receive succeeds");
        assert_eq!(frame.payload_len(), *size);
        assert!(frame.payload().iter().all(|&b| b == i as u8));
    }
}

#[test]
fn large_messages_vectored_send_with_large_payloads() {
    let payloads = [
        vec![0x11u8; 32 * 1024],
        vec![0x22u8; 64 * 1024],
        vec![0x33u8; 16 * 1024],
    ];

    let messages: Vec<(MessageCode, &[u8])> = payloads
        .iter()
        .map(|p| (MessageCode::Data, p.as_slice()))
        .collect();

    let mut buffer = Vec::new();
    send_msgs_vectored(&mut buffer, &messages).expect("vectored send succeeds");

    let mut cursor = Cursor::new(buffer);
    for (i, expected_payload) in payloads.iter().enumerate() {
        let frame = recv_msg(&mut cursor).expect("receive succeeds");
        assert_eq!(frame.payload(), expected_payload.as_slice(), "payload {i} mismatch");
    }
}

// =============================================================================
// Error Message Parsing Tests
// =============================================================================

#[test]
fn error_parsing_all_error_types_parse_correctly() {
    let error_types = [
        (MessageCode::Error, b"standard error" as &[u8]),
        (MessageCode::ErrorXfer, b"transfer error"),
        (MessageCode::ErrorSocket, b"socket error"),
        (MessageCode::ErrorUtf8, b"utf8 error"),
        (MessageCode::ErrorExit, b"exit error"),
    ];

    for (code, payload) in error_types {
        let mut buffer = Vec::new();
        send_msg(&mut buffer, code, payload).expect("send succeeds");

        let mut cursor = Cursor::new(buffer);
        let frame = recv_msg(&mut cursor).expect("receive succeeds");

        assert_eq!(frame.code(), code);
        assert_eq!(frame.payload(), payload);
    }
}

#[test]
fn error_parsing_error_messages_with_special_characters() {
    let special_payloads = [
        b"error: file not found\n" as &[u8],
        b"error\twith\ttabs",
        b"error with \0 null byte",
        b"error with UTF-8: \xC3\xA9",
        b"error with quotes: \"file.txt\"",
    ];

    for payload in special_payloads {
        let mut buffer = Vec::new();
        send_msg(&mut buffer, MessageCode::Error, payload).expect("send succeeds");

        let mut cursor = Cursor::new(buffer);
        let frame = recv_msg(&mut cursor).expect("receive succeeds");

        assert_eq!(frame.code(), MessageCode::Error);
        assert_eq!(frame.payload(), payload);
    }
}

#[test]
fn error_parsing_empty_error_message() {
    let mut buffer = Vec::new();
    send_msg(&mut buffer, MessageCode::Error, b"").expect("send succeeds");

    let mut cursor = Cursor::new(buffer);
    let frame = recv_msg(&mut cursor).expect("receive succeeds");

    assert_eq!(frame.code(), MessageCode::Error);
    assert!(frame.payload().is_empty());
}

#[test]
fn error_parsing_long_error_messages() {
    let long_error = vec![b'E'; 8192];

    let mut buffer = Vec::new();
    send_msg(&mut buffer, MessageCode::ErrorXfer, &long_error).expect("send succeeds");

    let mut cursor = Cursor::new(buffer);
    let frame = recv_msg(&mut cursor).expect("receive succeeds");

    assert_eq!(frame.code(), MessageCode::ErrorXfer);
    assert_eq!(frame.payload_len(), 8192);
}

#[test]
fn error_parsing_error_followed_by_other_messages() {
    let sequence = [
        (MessageCode::Info, b"starting" as &[u8]),
        (MessageCode::Error, b"something went wrong"),
        (MessageCode::Warning, b"recovering"),
        (MessageCode::Success, b"completed anyway"),
    ];

    let mut buffer = Vec::new();
    for (code, payload) in &sequence {
        send_msg(&mut buffer, *code, payload).expect("send succeeds");
    }

    let mut cursor = Cursor::new(buffer);
    for (expected_code, expected_payload) in &sequence {
        let frame = recv_msg(&mut cursor).expect("receive succeeds");
        assert_eq!(frame.code(), *expected_code);
        assert_eq!(frame.payload(), *expected_payload);
    }
}

#[test]
fn error_parsing_binary_error_payload() {
    let binary_payload = [
        0x00, 0xFF, 0x7F, 0x80, 0x01, 0xFE, 0xAA, 0x55,
        0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE, 0xBA, 0xBE,
    ];

    let mut buffer = Vec::new();
    send_msg(&mut buffer, MessageCode::ErrorSocket, &binary_payload).expect("send succeeds");

    let mut cursor = Cursor::new(buffer);
    let frame = recv_msg(&mut cursor).expect("receive succeeds");

    assert_eq!(frame.code(), MessageCode::ErrorSocket);
    assert_eq!(frame.payload(), &binary_payload);
}

#[test]
fn error_parsing_io_error_and_io_timeout_distinguished() {
    let mut buffer = Vec::new();
    send_msg(&mut buffer, MessageCode::IoError, b"read failed").expect("send io_error");
    send_msg(&mut buffer, MessageCode::IoTimeout, b"timed out").expect("send io_timeout");

    let mut cursor = Cursor::new(buffer);

    let frame1 = recv_msg(&mut cursor).expect("receive io_error");
    assert_eq!(frame1.code(), MessageCode::IoError);
    assert_eq!(frame1.payload(), b"read failed");

    let frame2 = recv_msg(&mut cursor).expect("receive io_timeout");
    assert_eq!(frame2.code(), MessageCode::IoTimeout);
    assert_eq!(frame2.payload(), b"timed out");
}

#[test]
fn error_parsing_unknown_code_rejected() {
    // Construct a header with an unknown message code
    let unknown_code = 11u8; // Not in MessageCode::ALL
    let tag = u32::from(MPLEX_BASE) + u32::from(unknown_code);
    let raw_header = (tag << 24).to_le_bytes();

    let err = recv_msg(&mut Cursor::new(raw_header)).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

// =============================================================================
// Cross-Cutting Correctness Tests
// =============================================================================

#[test]
fn correctness_interleaved_data_and_control_messages() {
    let sequence = [
        (MessageCode::Data, vec![0x01; 1024]),
        (MessageCode::Stats, vec![0x02; 16]),
        (MessageCode::Data, vec![0x03; 2048]),
        (MessageCode::Success, vec![0x04; 8]),
        (MessageCode::Data, vec![0x05; 512]),
    ];

    let mut buffer = Vec::new();
    for (code, payload) in &sequence {
        send_msg(&mut buffer, *code, payload).expect("send succeeds");
    }

    let mut cursor = Cursor::new(buffer);
    for (expected_code, expected_payload) in &sequence {
        let frame = recv_msg(&mut cursor).expect("receive succeeds");
        assert_eq!(frame.code(), *expected_code);
        assert_eq!(frame.payload(), expected_payload.as_slice());
    }
}

#[test]
fn correctness_frame_encode_decode_roundtrip_all_types() {
    for code in MessageCode::ALL {
        let payload = format!("test for {}", code.name()).into_bytes();
        let frame = MessageFrame::new(code, payload.clone()).expect("frame creation");

        let mut encoded = Vec::new();
        frame.encode_into_vec(&mut encoded).expect("encode succeeds");

        let (decoded, remainder) = MessageFrame::decode_from_slice(&encoded).expect("decode succeeds");

        assert_eq!(decoded.code(), code);
        assert_eq!(decoded.payload(), payload.as_slice());
        assert!(remainder.is_empty());
    }
}

#[test]
fn correctness_recv_into_preserves_message_boundaries() {
    let messages = [
        (MessageCode::Info, b"first message" as &[u8]),
        (MessageCode::Data, &[0xAAu8; 256]),
        (MessageCode::Warning, b"third message"),
    ];

    let mut stream = Vec::new();
    for (code, payload) in &messages {
        send_msg(&mut stream, *code, payload).expect("send succeeds");
    }

    let mut cursor = Cursor::new(stream);
    let mut buffer = Vec::new();

    for (expected_code, expected_payload) in &messages {
        let code = recv_msg_into(&mut cursor, &mut buffer).expect("receive succeeds");
        assert_eq!(code, *expected_code);
        assert_eq!(buffer.as_slice(), *expected_payload);
    }
}

#[test]
fn correctness_zero_length_messages_at_various_positions() {
    let sequence = [
        (MessageCode::Info, b"" as &[u8]),
        (MessageCode::Data, b"data"),
        (MessageCode::Warning, b""),
        (MessageCode::Error, b"error"),
        (MessageCode::NoOp, b""),
    ];

    let mut buffer = Vec::new();
    for (code, payload) in &sequence {
        send_msg(&mut buffer, *code, payload).expect("send succeeds");
    }

    let mut cursor = Cursor::new(buffer);
    for (expected_code, expected_payload) in &sequence {
        let frame = recv_msg(&mut cursor).expect("receive succeeds");
        assert_eq!(frame.code(), *expected_code);
        assert_eq!(frame.payload(), *expected_payload);
    }
}

#[test]
fn correctness_message_boundaries_with_identical_payloads() {
    // Verify that identical payloads don't cause messages to merge
    let identical_payload = b"same payload";
    let codes = [
        MessageCode::Info,
        MessageCode::Warning,
        MessageCode::Error,
    ];

    let mut buffer = Vec::new();
    for code in codes {
        send_msg(&mut buffer, code, identical_payload).expect("send succeeds");
    }

    let mut cursor = Cursor::new(buffer);
    for expected_code in codes {
        let frame = recv_msg(&mut cursor).expect("receive succeeds");
        assert_eq!(frame.code(), expected_code);
        assert_eq!(frame.payload(), identical_payload);
    }
}
