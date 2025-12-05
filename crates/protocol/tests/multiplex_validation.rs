//! Message multiplexing validation tests.
//!
//! These tests validate the wire-level multiplexing protocol used by rsync to
//! carry control messages alongside file data. The tests ensure frame encoding,
//! demultiplexing, vectored I/O, and all message tags work correctly according
//! to upstream rsync 3.4.1 semantics.

use protocol::{MessageCode, MessageFrame, recv_msg, recv_msg_into, send_frame, send_msg};
use std::io::Cursor;

// ============================================================================
// Message Frame Encoding Tests
// ============================================================================

#[test]
fn test_data_message_encoding() {
    let payload = b"file contents";
    let mut buf = Vec::new();

    send_msg(&mut buf, MessageCode::Data, payload).expect("send must succeed");

    // Verify header: 4 bytes little-endian representing (tag << 24) | payload_len
    // tag = MPLEX_BASE (7) + Data (0) = 7
    // payload_len = 13
    // raw u32 = (7 << 24) | 13 = 0x0700000D
    // little-endian bytes = [0x0D, 0x00, 0x00, 0x07]
    assert_eq!(buf.len(), 4 + 13, "total length must be header + payload");
    let raw_header = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let tag = (raw_header >> 24) as u8;
    let payload_len = raw_header & 0x00FFFFFF;

    assert_eq!(tag, 7, "MSG_DATA tag must be 7");
    assert_eq!(payload_len, 13, "payload length must be 13");

    // Verify payload
    assert_eq!(&buf[4..], payload, "payload must match");
}

#[test]
fn test_info_message_encoding() {
    let payload = b"transfer stats";
    let mut buf = Vec::new();

    send_msg(&mut buf, MessageCode::Info, payload).expect("send must succeed");

    let raw_header = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let tag = (raw_header >> 24) as u8;
    let payload_len = raw_header & 0x00FFFFFF;

    // MSG_INFO = 2, tag = 7 + 2 = 9
    assert_eq!(tag, 9, "MSG_INFO tag must be 9");
    assert_eq!(payload_len, 14, "payload length must be 14");
    assert_eq!(&buf[4..], payload, "payload must match");
}

#[test]
fn test_empty_payload_encoding() {
    let mut buf = Vec::new();

    send_msg(&mut buf, MessageCode::NoOp, b"").expect("send must succeed");

    // Header only, no payload bytes
    assert_eq!(buf.len(), 4, "empty payload produces 4-byte header only");

    let raw_header = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let tag = (raw_header >> 24) as u8;
    let payload_len = raw_header & 0x00FFFFFF;

    // MSG_NOOP = 42, tag = 7 + 42 = 49
    assert_eq!(tag, 49, "MSG_NOOP tag must be 49");
    assert_eq!(payload_len, 0, "payload length must be 0");
}

#[test]
fn test_large_payload_encoding() {
    // Test a payload near the maximum size (16MB - 1)
    let payload_size = 1024 * 1024; // 1MB
    let payload = vec![0x42u8; payload_size];
    let mut buf = Vec::new();

    send_msg(&mut buf, MessageCode::Data, &payload).expect("send must succeed");

    assert_eq!(
        buf.len(),
        4 + payload_size,
        "total size must be header + payload"
    );

    let raw_header = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let tag = (raw_header >> 24) as u8;
    let payload_len = raw_header & 0x00FFFFFF;

    assert_eq!(tag, 7, "MSG_DATA tag must be 7");
    assert_eq!(
        payload_len, payload_size as u32,
        "payload length must match"
    );
    assert_eq!(&buf[4..], &payload[..], "payload contents must match");
}

#[test]
fn test_all_message_codes_encoding() {
    let all_codes = MessageCode::all();

    for &code in all_codes {
        let mut buf = Vec::new();
        let payload = format!("test payload for {code}");

        send_msg(&mut buf, code, payload.as_bytes()).expect("send must succeed");

        // Verify we can decode it back
        let mut cursor = Cursor::new(&buf);
        let frame = recv_msg(&mut cursor).expect("recv must succeed");

        assert_eq!(frame.code(), code, "decoded code must match");
        assert_eq!(
            frame.payload(),
            payload.as_bytes(),
            "decoded payload must match"
        );
    }
}

// ============================================================================
// Multiplexed Stream Reading Tests
// ============================================================================

#[test]
fn test_receive_single_message() {
    let mut buf = Vec::new();
    send_msg(&mut buf, MessageCode::Info, b"single message").expect("send must succeed");

    let mut cursor = Cursor::new(&buf);
    let frame = recv_msg(&mut cursor).expect("recv must succeed");

    assert_eq!(frame.code(), MessageCode::Info);
    assert_eq!(frame.payload(), b"single message");
}

#[test]
fn test_receive_multiple_messages_sequential() {
    let mut buf = Vec::new();

    // Send three messages
    send_msg(&mut buf, MessageCode::Data, b"data1").expect("send must succeed");
    send_msg(&mut buf, MessageCode::Info, b"info1").expect("send must succeed");
    send_msg(&mut buf, MessageCode::Data, b"data2").expect("send must succeed");

    // Read them back
    let mut cursor = Cursor::new(&buf);

    let frame1 = recv_msg(&mut cursor).expect("recv 1 must succeed");
    assert_eq!(frame1.code(), MessageCode::Data);
    assert_eq!(frame1.payload(), b"data1");

    let frame2 = recv_msg(&mut cursor).expect("recv 2 must succeed");
    assert_eq!(frame2.code(), MessageCode::Info);
    assert_eq!(frame2.payload(), b"info1");

    let frame3 = recv_msg(&mut cursor).expect("recv 3 must succeed");
    assert_eq!(frame3.code(), MessageCode::Data);
    assert_eq!(frame3.payload(), b"data2");
}

#[test]
fn test_receive_into_reuses_buffer() {
    let mut buf = Vec::new();
    send_msg(&mut buf, MessageCode::Data, b"test payload").expect("send must succeed");

    let mut cursor = Cursor::new(&buf);
    let mut payload_buf = Vec::with_capacity(1024);

    let code = recv_msg_into(&mut cursor, &mut payload_buf).expect("recv must succeed");

    assert_eq!(code, MessageCode::Data);
    assert_eq!(&payload_buf[..], b"test payload");
    assert_eq!(payload_buf.capacity(), 1024, "capacity should be preserved");
}

#[test]
fn test_receive_into_clears_previous_content() {
    let mut buf = Vec::new();
    send_msg(&mut buf, MessageCode::Data, b"new").expect("send must succeed");

    let mut cursor = Cursor::new(&buf);
    let mut payload_buf = vec![0xFF; 100]; // Pre-filled buffer

    let code = recv_msg_into(&mut cursor, &mut payload_buf).expect("recv must succeed");

    assert_eq!(code, MessageCode::Data);
    assert_eq!(payload_buf.len(), 3, "buffer length must match new payload");
    assert_eq!(&payload_buf[..], b"new");
}

#[test]
fn test_demultiplex_mixed_message_types() {
    let mut buf = Vec::new();

    // Simulate a transfer with mixed message types
    send_msg(&mut buf, MessageCode::Data, b"chunk1").expect("send must succeed");
    send_msg(&mut buf, MessageCode::Info, b"transferred 6 bytes").expect("send must succeed");
    send_msg(&mut buf, MessageCode::Data, b"chunk2").expect("send must succeed");
    send_msg(&mut buf, MessageCode::Success, b"file updated").expect("send must succeed");
    send_msg(&mut buf, MessageCode::Stats, b"total: 12 bytes").expect("send must succeed");

    let mut cursor = Cursor::new(&buf);
    let mut data_bytes = Vec::new();
    let mut log_messages = Vec::new();

    // Demultiplex the stream
    for _ in 0..5 {
        let frame = recv_msg(&mut cursor).expect("recv must succeed");
        match frame.code() {
            MessageCode::Data => data_bytes.extend_from_slice(frame.payload()),
            MessageCode::Info | MessageCode::Success | MessageCode::Stats => {
                log_messages.push(String::from_utf8(frame.payload().to_vec()).unwrap());
            }
            _ => {}
        }
    }

    assert_eq!(
        &data_bytes, b"chunk1chunk2",
        "data must be concatenated correctly"
    );
    assert_eq!(log_messages.len(), 3, "must receive 3 log messages");
}

// ============================================================================
// Vectored Write Tests
// ============================================================================

#[test]
fn test_send_frame_uses_vectored_io() {
    // This test verifies that send_frame works correctly
    let payload = b"test data for vectored write";
    let frame = MessageFrame::new(MessageCode::Data, payload.to_vec())
        .expect("frame creation must succeed");

    let mut buf = Vec::new();
    send_frame(&mut buf, &frame).expect("send_frame must succeed");

    // Verify encoding matches send_msg
    let mut expected = Vec::new();
    send_msg(&mut expected, MessageCode::Data, payload).expect("send_msg must succeed");

    assert_eq!(
        buf, expected,
        "send_frame must produce same output as send_msg"
    );
}

#[test]
fn test_vectored_write_single_syscall_simulation() {
    // Verify header and payload are written together when possible
    struct CountingWriter {
        buf: Vec<u8>,
        write_count: usize,
    }

    impl std::io::Write for CountingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.write_count += 1;
            self.buf.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn write_vectored(&mut self, bufs: &[std::io::IoSlice<'_>]) -> std::io::Result<usize> {
            self.write_count += 1;
            let mut total = 0;
            for buf in bufs {
                self.buf.extend_from_slice(buf);
                total += buf.len();
            }
            Ok(total)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut writer = CountingWriter {
        buf: Vec::new(),
        write_count: 0,
    };

    send_msg(&mut writer, MessageCode::Data, b"test").expect("send must succeed");

    // Should use vectored write (1 call) instead of 2 separate writes
    assert_eq!(writer.write_count, 1, "should use single vectored write");
    assert_eq!(writer.buf.len(), 8, "4-byte header + 4-byte payload");
}

// ============================================================================
// All Message Tags Validation
// ============================================================================

#[test]
fn test_all_18_message_codes_round_trip() {
    let all_codes = MessageCode::all();
    assert_eq!(all_codes.len(), 18, "must have exactly 18 message codes");

    for &code in all_codes {
        let payload = format!("payload for {}", code.name());
        let mut buf = Vec::new();

        send_msg(&mut buf, code, payload.as_bytes()).expect("send must succeed");

        let mut cursor = Cursor::new(&buf);
        let frame = recv_msg(&mut cursor).expect("recv must succeed");

        assert_eq!(frame.code(), code, "code must round-trip");
        assert_eq!(
            frame.payload(),
            payload.as_bytes(),
            "payload must round-trip"
        );
    }
}

#[test]
fn test_message_code_names() {
    assert_eq!(MessageCode::Data.name(), "MSG_DATA");
    assert_eq!(MessageCode::ErrorXfer.name(), "MSG_ERROR_XFER");
    assert_eq!(MessageCode::Info.name(), "MSG_INFO");
    assert_eq!(MessageCode::Error.name(), "MSG_ERROR");
    assert_eq!(MessageCode::Warning.name(), "MSG_WARNING");
    assert_eq!(MessageCode::ErrorSocket.name(), "MSG_ERROR_SOCKET");
    assert_eq!(MessageCode::Log.name(), "MSG_LOG");
    assert_eq!(MessageCode::Client.name(), "MSG_CLIENT");
    assert_eq!(MessageCode::ErrorUtf8.name(), "MSG_ERROR_UTF8");
    assert_eq!(MessageCode::Redo.name(), "MSG_REDO");
    assert_eq!(MessageCode::Stats.name(), "MSG_STATS");
    assert_eq!(MessageCode::IoError.name(), "MSG_IO_ERROR");
    assert_eq!(MessageCode::IoTimeout.name(), "MSG_IO_TIMEOUT");
    assert_eq!(MessageCode::NoOp.name(), "MSG_NOOP");
    assert_eq!(MessageCode::ErrorExit.name(), "MSG_ERROR_EXIT");
    assert_eq!(MessageCode::Success.name(), "MSG_SUCCESS");
    assert_eq!(MessageCode::Deleted.name(), "MSG_DELETED");
    assert_eq!(MessageCode::NoSend.name(), "MSG_NO_SEND");
}

#[test]
fn test_message_code_numeric_values() {
    assert_eq!(MessageCode::Data.as_u8(), 0);
    assert_eq!(MessageCode::ErrorXfer.as_u8(), 1);
    assert_eq!(MessageCode::Info.as_u8(), 2);
    assert_eq!(MessageCode::Error.as_u8(), 3);
    assert_eq!(MessageCode::Warning.as_u8(), 4);
    assert_eq!(MessageCode::ErrorSocket.as_u8(), 5);
    assert_eq!(MessageCode::Log.as_u8(), 6);
    assert_eq!(MessageCode::Client.as_u8(), 7);
    assert_eq!(MessageCode::ErrorUtf8.as_u8(), 8);
    assert_eq!(MessageCode::Redo.as_u8(), 9);
    assert_eq!(MessageCode::Stats.as_u8(), 10);
    assert_eq!(MessageCode::IoError.as_u8(), 22);
    assert_eq!(MessageCode::IoTimeout.as_u8(), 33);
    assert_eq!(MessageCode::NoOp.as_u8(), 42);
    assert_eq!(MessageCode::ErrorExit.as_u8(), 86);
    assert_eq!(MessageCode::Success.as_u8(), 100);
    assert_eq!(MessageCode::Deleted.as_u8(), 101);
    assert_eq!(MessageCode::NoSend.as_u8(), 102);
}

#[test]
fn test_logging_message_detection() {
    // Logging messages
    assert!(MessageCode::ErrorXfer.is_logging());
    assert!(MessageCode::Info.is_logging());
    assert!(MessageCode::Error.is_logging());
    assert!(MessageCode::Warning.is_logging());
    assert!(MessageCode::ErrorSocket.is_logging());
    assert!(MessageCode::Log.is_logging());
    assert!(MessageCode::Client.is_logging());
    assert!(MessageCode::ErrorUtf8.is_logging());

    // Non-logging messages
    assert!(!MessageCode::Data.is_logging());
    assert!(!MessageCode::Redo.is_logging());
    assert!(!MessageCode::Stats.is_logging());
    assert!(!MessageCode::IoError.is_logging());
    assert!(!MessageCode::IoTimeout.is_logging());
    assert!(!MessageCode::NoOp.is_logging());
    assert!(!MessageCode::ErrorExit.is_logging());
    assert!(!MessageCode::Success.is_logging());
    assert!(!MessageCode::Deleted.is_logging());
    assert!(!MessageCode::NoSend.is_logging());
}

#[test]
fn test_flush_alias() {
    // MSG_FLUSH is an alias for MSG_INFO
    assert_eq!(MessageCode::FLUSH, MessageCode::Info);
    assert_eq!(MessageCode::FLUSH.as_u8(), 2);
}

// ============================================================================
// Edge Cases and Error Handling
// ============================================================================

#[test]
fn test_truncated_header_detection() {
    // Only 3 bytes instead of 4
    let buf = vec![7, 0, 0];
    let mut cursor = Cursor::new(&buf);

    let result = recv_msg(&mut cursor);
    assert!(result.is_err(), "truncated header must fail");
}

#[test]
fn test_truncated_payload_detection() {
    let mut buf = Vec::new();
    send_msg(&mut buf, MessageCode::Data, b"complete").expect("send must succeed");

    // Truncate the buffer (remove last 2 bytes of payload)
    buf.truncate(buf.len() - 2);

    let mut cursor = Cursor::new(&buf);
    let result = recv_msg(&mut cursor);

    assert!(result.is_err(), "truncated payload must fail");
}

#[test]
fn test_maximum_payload_length_accepted() {
    // Maximum payload is 0xFFFFFF (16MB - 1)
    let max_len = 0xFFFFFF;
    let payload = vec![0x42u8; max_len];

    let mut buf = Vec::new();
    let result = send_msg(&mut buf, MessageCode::Data, &payload);

    assert!(result.is_ok(), "maximum payload length must be accepted");
}

#[test]
fn test_oversized_payload_rejected() {
    // Payload larger than 0xFFFFFF must be rejected
    let oversized_len = 0x1000000; // 16MB
    let payload = vec![0x42u8; oversized_len];

    let mut buf = Vec::new();
    let result = send_msg(&mut buf, MessageCode::Data, &payload);

    assert!(result.is_err(), "oversized payload must be rejected");
}

#[test]
fn test_message_frame_display() {
    let frame = MessageFrame::new(MessageCode::Info, b"test".to_vec())
        .expect("frame creation must succeed");

    let display = format!("{frame:?}");
    assert!(display.contains("Info"), "display must show message code");
}
