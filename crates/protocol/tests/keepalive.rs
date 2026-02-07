//! Keepalive mechanism tests.
//!
//! Upstream rsync sends MSG_NOOP (code 42) messages as keepalive heartbeats to
//! prevent connection timeouts during long operations such as large file
//! checksumming. These tests verify:
//!
//! - The keepalive wire format matches upstream rsync (MSG_NOOP with empty payload)
//! - Receiving keepalives is handled silently
//! - Keepalives do not interfere with data transfer
//! - Multiple keepalives in sequence are handled correctly
//! - The MplexReader transparently skips keepalives
//! - The MplexWriter can send keepalives interleaved with data

use std::io::{self, Cursor, Read};
use std::sync::{Arc, Mutex};

use protocol::{
    MPLEX_BASE, MessageCode, MessageFrame, MessageHeader, MplexReader, MplexWriter, recv_msg,
    recv_msg_into, send_keepalive, send_msg,
};

// ============================================================================
// Wire Format Tests
// ============================================================================

/// Keepalive produces correct 4-byte header with zero-length payload.
#[test]
fn keepalive_wire_format_is_4_byte_header_only() {
    let mut buf = Vec::new();
    send_keepalive(&mut buf).expect("send_keepalive must succeed");

    // MSG_NOOP with empty payload = header only
    assert_eq!(buf.len(), 4, "keepalive must be exactly 4 bytes (header only)");
}

/// The keepalive header encodes MSG_NOOP (tag = MPLEX_BASE + 42 = 49) with length 0.
#[test]
fn keepalive_header_encodes_noop_tag_49() {
    let mut buf = Vec::new();
    send_keepalive(&mut buf).expect("send_keepalive must succeed");

    let raw_header = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let tag = (raw_header >> 24) as u8;
    let payload_len = raw_header & 0x00FF_FFFF;

    assert_eq!(
        tag,
        u8::try_from(MPLEX_BASE).unwrap() + MessageCode::NoOp.as_u8(),
        "tag must be MPLEX_BASE + MSG_NOOP"
    );
    assert_eq!(tag, 49, "tag must be 49 (7 + 42)");
    assert_eq!(payload_len, 0, "keepalive payload length must be 0");
}

/// send_keepalive produces the same bytes as send_msg(NoOp, &[]).
#[test]
fn keepalive_matches_explicit_noop_send() {
    let mut keepalive_buf = Vec::new();
    send_keepalive(&mut keepalive_buf).expect("send_keepalive must succeed");

    let mut noop_buf = Vec::new();
    send_msg(&mut noop_buf, MessageCode::NoOp, &[]).expect("send_msg must succeed");

    assert_eq!(
        keepalive_buf, noop_buf,
        "send_keepalive must produce identical bytes to send_msg(NoOp, &[])"
    );
}

/// Decoding a keepalive message yields NoOp code with empty payload.
#[test]
fn keepalive_roundtrip_decodes_to_noop_empty() {
    let mut buf = Vec::new();
    send_keepalive(&mut buf).expect("send_keepalive must succeed");

    let mut cursor = Cursor::new(&buf);
    let frame = recv_msg(&mut cursor).expect("recv_msg must succeed");

    assert_eq!(frame.code(), MessageCode::NoOp, "code must be NoOp");
    assert!(frame.payload().is_empty(), "payload must be empty");
}

/// The is_keepalive() helper correctly identifies keepalive messages.
#[test]
fn is_keepalive_identifies_noop_only() {
    assert!(MessageCode::NoOp.is_keepalive(), "NoOp must be keepalive");

    // All other codes must not be keepalive
    for code in MessageCode::ALL {
        if code == MessageCode::NoOp {
            continue;
        }
        assert!(
            !code.is_keepalive(),
            "{} must not be identified as keepalive",
            code.name()
        );
    }
}

// ============================================================================
// Receiving Keepalive Messages
// ============================================================================

/// recv_msg decodes keepalive as NoOp with empty payload.
#[test]
fn recv_msg_decodes_keepalive() {
    let mut buf = Vec::new();
    send_keepalive(&mut buf).unwrap();

    let mut cursor = Cursor::new(&buf);
    let frame = recv_msg(&mut cursor).unwrap();

    assert_eq!(frame.code(), MessageCode::NoOp);
    assert!(frame.payload().is_empty());
    assert!(frame.code().is_keepalive());
}

/// recv_msg_into decodes keepalive and leaves the buffer empty.
#[test]
fn recv_msg_into_decodes_keepalive_empty_buffer() {
    let mut buf = Vec::new();
    send_keepalive(&mut buf).unwrap();

    let mut cursor = Cursor::new(&buf);
    let mut payload_buf = vec![0xFF; 64]; // pre-filled
    let code = recv_msg_into(&mut cursor, &mut payload_buf).unwrap();

    assert_eq!(code, MessageCode::NoOp);
    assert!(payload_buf.is_empty(), "keepalive must clear the buffer");
}

// ============================================================================
// Keepalive Does Not Interfere With Data Transfer
// ============================================================================

/// Keepalive messages interleaved with data messages are silently consumed by MplexReader.
#[test]
fn mplex_reader_skips_keepalives_between_data() {
    let mut stream = Vec::new();
    send_keepalive(&mut stream).unwrap();
    send_msg(&mut stream, MessageCode::Data, b"hello").unwrap();
    send_keepalive(&mut stream).unwrap();
    send_keepalive(&mut stream).unwrap();
    send_msg(&mut stream, MessageCode::Data, b" world").unwrap();
    send_keepalive(&mut stream).unwrap();

    let mut reader = MplexReader::new(Cursor::new(stream));
    let mut result = Vec::new();
    let mut buf = [0u8; 64];

    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => result.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    assert_eq!(result, b"hello world", "data must be reconstructed correctly");
}

/// Keepalive before any data does not cause errors in MplexReader.
#[test]
fn mplex_reader_handles_leading_keepalives() {
    let mut stream = Vec::new();
    send_keepalive(&mut stream).unwrap();
    send_keepalive(&mut stream).unwrap();
    send_keepalive(&mut stream).unwrap();
    send_msg(&mut stream, MessageCode::Data, b"after keepalives").unwrap();

    let mut reader = MplexReader::new(Cursor::new(stream));
    let mut buf = [0u8; 64];
    let n = reader.read(&mut buf).unwrap();

    assert_eq!(&buf[..n], b"after keepalives");
}

/// Keepalive messages are reported to the message handler.
#[test]
fn mplex_reader_reports_keepalive_to_handler() {
    let mut stream = Vec::new();
    send_keepalive(&mut stream).unwrap();
    send_msg(&mut stream, MessageCode::Data, b"data").unwrap();

    let messages = Arc::new(Mutex::new(Vec::new()));
    let messages_clone = messages.clone();

    let mut reader = MplexReader::new(Cursor::new(stream));
    reader.set_message_handler(move |code, payload| {
        messages_clone
            .lock()
            .unwrap()
            .push((code, payload.to_vec()));
    });

    let mut buf = [0u8; 64];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"data");

    let captured = messages.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].0, MessageCode::NoOp);
    assert!(captured[0].1.is_empty());
}

/// Keepalive interleaved with other out-of-band messages and data.
#[test]
fn keepalive_interleaved_with_oob_and_data() {
    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::Info, b"info").unwrap();
    send_keepalive(&mut stream).unwrap();
    send_msg(&mut stream, MessageCode::Data, b"chunk1").unwrap();
    send_keepalive(&mut stream).unwrap();
    send_msg(&mut stream, MessageCode::Warning, b"warn").unwrap();
    send_msg(&mut stream, MessageCode::Data, b"chunk2").unwrap();
    send_keepalive(&mut stream).unwrap();

    let oob = Arc::new(Mutex::new(Vec::new()));
    let oob_clone = oob.clone();

    let mut reader = MplexReader::new(Cursor::new(stream));
    reader.set_message_handler(move |code, payload| {
        oob_clone
            .lock()
            .unwrap()
            .push((code, payload.to_vec()));
    });

    let mut data = Vec::new();
    let mut buf = [0u8; 64];

    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => data.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    assert_eq!(data, b"chunk1chunk2");

    let captured = oob.lock().unwrap();
    // Should have: Info, NoOp, NoOp, Warning, NoOp (in order)
    assert_eq!(captured.len(), 5);
    assert_eq!(captured[0].0, MessageCode::Info);
    assert_eq!(captured[1].0, MessageCode::NoOp);
    assert_eq!(captured[2].0, MessageCode::NoOp);
    assert_eq!(captured[3].0, MessageCode::Warning);
    assert_eq!(captured[4].0, MessageCode::NoOp);
}

// ============================================================================
// Multiple Keepalives in Sequence
// ============================================================================

/// A burst of keepalives can be sent and received without error.
#[test]
fn multiple_keepalives_in_sequence() {
    let count = 100;
    let mut buf = Vec::new();
    for _ in 0..count {
        send_keepalive(&mut buf).unwrap();
    }

    // All must decode as NoOp with empty payload
    let mut cursor = Cursor::new(&buf);
    for i in 0..count {
        let frame = recv_msg(&mut cursor).expect(&format!("keepalive {i} must decode"));
        assert_eq!(frame.code(), MessageCode::NoOp);
        assert!(frame.payload().is_empty());
    }
}

/// MplexReader handles a stream of only keepalives (reaches EOF after consuming all).
#[test]
fn mplex_reader_only_keepalives_reaches_eof() {
    let mut stream = Vec::new();
    for _ in 0..10 {
        send_keepalive(&mut stream).unwrap();
    }

    let mut reader = MplexReader::new(Cursor::new(stream));
    let mut buf = [0u8; 64];

    // Attempting to read should eventually hit EOF after consuming all keepalives
    let result = reader.read(&mut buf);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
}

/// Keepalives followed by data -- MplexReader correctly returns data after skipping keepalives.
#[test]
fn keepalive_burst_then_data() {
    let mut stream = Vec::new();
    for _ in 0..50 {
        send_keepalive(&mut stream).unwrap();
    }
    send_msg(&mut stream, MessageCode::Data, b"finally!").unwrap();

    let mut reader = MplexReader::new(Cursor::new(stream));
    let mut buf = [0u8; 64];
    let n = reader.read(&mut buf).unwrap();

    assert_eq!(&buf[..n], b"finally!");
}

// ============================================================================
// MplexWriter Keepalive Integration
// ============================================================================

/// MplexWriter::write_keepalive sends a properly formatted MSG_NOOP.
#[test]
fn mplex_writer_keepalive_produces_correct_frame() {
    let mut output = Vec::new();
    let mut writer = MplexWriter::new(&mut output);

    writer.write_keepalive().unwrap();

    // Verify it can be decoded
    let mut cursor = Cursor::new(&output);
    let frame = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame.code(), MessageCode::NoOp);
    assert!(frame.payload().is_empty());
}

/// MplexWriter::write_keepalive flushes buffered data first.
#[test]
fn mplex_writer_keepalive_flushes_buffered_data() {
    let mut output = Vec::new();
    let mut writer = MplexWriter::new(&mut output);

    // Buffer some data
    writer.write_all(b"buffered").unwrap();
    assert_eq!(writer.buffered(), 8);

    // Keepalive should flush the buffer
    writer.write_keepalive().unwrap();
    assert_eq!(writer.buffered(), 0);

    // Verify: DATA frame, then NOOP frame
    let mut cursor = Cursor::new(&output);

    let frame1 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame1.code(), MessageCode::Data);
    assert_eq!(frame1.payload(), b"buffered");

    let frame2 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame2.code(), MessageCode::NoOp);
    assert!(frame2.payload().is_empty());
}

/// Keepalive interleaved with MplexWriter data writes.
#[test]
fn mplex_writer_keepalive_interleaved_with_data() {
    let mut output = Vec::new();
    let mut writer = MplexWriter::new(&mut output);

    writer.write_data(b"chunk1").unwrap();
    writer.write_keepalive().unwrap();
    writer.write_data(b"chunk2").unwrap();
    writer.write_keepalive().unwrap();
    writer.write_keepalive().unwrap();
    writer.write_data(b"chunk3").unwrap();
    writer.flush().unwrap();

    // Verify the sequence
    let mut cursor = Cursor::new(&output);

    let f1 = recv_msg(&mut cursor).unwrap();
    assert_eq!(f1.code(), MessageCode::Data);
    assert_eq!(f1.payload(), b"chunk1");

    let f2 = recv_msg(&mut cursor).unwrap();
    assert_eq!(f2.code(), MessageCode::NoOp);
    assert!(f2.payload().is_empty());

    let f3 = recv_msg(&mut cursor).unwrap();
    assert_eq!(f3.code(), MessageCode::Data);
    assert_eq!(f3.payload(), b"chunk2");

    let f4 = recv_msg(&mut cursor).unwrap();
    assert_eq!(f4.code(), MessageCode::NoOp);

    let f5 = recv_msg(&mut cursor).unwrap();
    assert_eq!(f5.code(), MessageCode::NoOp);

    let f6 = recv_msg(&mut cursor).unwrap();
    assert_eq!(f6.code(), MessageCode::Data);
    assert_eq!(f6.payload(), b"chunk3");
}

// ============================================================================
// MessageFrame Integration
// ============================================================================

/// A keepalive can be constructed and encoded via MessageFrame.
#[test]
fn message_frame_keepalive_roundtrip() {
    let frame = MessageFrame::new(MessageCode::NoOp, vec![]).unwrap();
    assert!(frame.code().is_keepalive());
    assert!(frame.payload().is_empty());

    let mut encoded = Vec::new();
    frame.encode_into_vec(&mut encoded).unwrap();

    let (decoded, remainder) = MessageFrame::decode_from_slice(&encoded).unwrap();
    assert!(remainder.is_empty());
    assert_eq!(decoded.code(), MessageCode::NoOp);
    assert!(decoded.payload().is_empty());
    assert!(decoded.code().is_keepalive());
}

/// Keepalive MessageFrame encodes to the same bytes as send_keepalive.
#[test]
fn message_frame_keepalive_matches_send_keepalive() {
    let frame = MessageFrame::new(MessageCode::NoOp, vec![]).unwrap();
    let mut frame_bytes = Vec::new();
    frame.encode_into_vec(&mut frame_bytes).unwrap();

    let mut keepalive_bytes = Vec::new();
    send_keepalive(&mut keepalive_bytes).unwrap();

    assert_eq!(frame_bytes, keepalive_bytes);
}

// ============================================================================
// End-to-End Pipeline Test
// ============================================================================

/// Simulates a transfer pipeline where keepalives are sent during a long operation.
/// Writer sends keepalives between data chunks; reader extracts data transparently.
#[test]
fn end_to_end_keepalive_during_transfer() {
    let mut wire = Vec::new();

    // Simulate sender writing chunks with keepalives
    {
        let mut writer = MplexWriter::new(&mut wire);

        // Send first chunk
        writer.write_data(b"file header data").unwrap();

        // Simulate long operation -- send keepalives
        writer.write_keepalive().unwrap();
        writer.write_keepalive().unwrap();
        writer.write_keepalive().unwrap();

        // Send info message (like progress)
        writer
            .write_message(MessageCode::Info, b"checksumming...")
            .unwrap();

        // More keepalives
        writer.write_keepalive().unwrap();

        // Continue data
        writer.write_data(b"file body data").unwrap();

        // Final keepalive before completion
        writer.write_keepalive().unwrap();

        writer.write_data(b"file trailer").unwrap();
        writer.flush().unwrap();
    }

    // Simulate receiver reading the stream
    let keepalive_count = Arc::new(Mutex::new(0u32));
    let info_messages = Arc::new(Mutex::new(Vec::new()));
    let keepalive_count_clone = keepalive_count.clone();
    let info_messages_clone = info_messages.clone();

    let mut reader = MplexReader::new(Cursor::new(wire));
    reader.set_message_handler(move |code, payload| {
        if code.is_keepalive() {
            *keepalive_count_clone.lock().unwrap() += 1;
        } else if code == MessageCode::Info {
            info_messages_clone
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(payload).to_string());
        }
    });

    // Read all data
    let mut data = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => data.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    // Verify data integrity
    assert_eq!(
        data,
        b"file header datafile body datafile trailer",
        "data must be reconstructed without keepalive interference"
    );

    // Verify keepalives were counted
    assert_eq!(
        *keepalive_count.lock().unwrap(),
        5,
        "all 5 keepalives must have been handled"
    );

    // Verify info message was received
    let infos = info_messages.lock().unwrap();
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0], "checksumming...");
}

// ============================================================================
// MessageHeader Keepalive Encoding
// ============================================================================

/// Keepalive MessageHeader encodes and decodes correctly.
#[test]
fn keepalive_header_roundtrip() {
    let header = MessageHeader::new(MessageCode::NoOp, 0).unwrap();
    assert_eq!(header.code(), MessageCode::NoOp);
    assert_eq!(header.payload_len(), 0);

    let encoded = header.encode();
    let decoded = MessageHeader::decode(&encoded).unwrap();
    assert_eq!(decoded.code(), MessageCode::NoOp);
    assert_eq!(decoded.payload_len(), 0);
}

use std::io::Write;

/// Keepalive is distinct from IoTimeout in both code and semantics.
#[test]
fn keepalive_distinct_from_io_timeout() {
    assert_ne!(MessageCode::NoOp, MessageCode::IoTimeout);
    assert_ne!(MessageCode::NoOp.as_u8(), MessageCode::IoTimeout.as_u8());

    assert!(MessageCode::NoOp.is_keepalive());
    assert!(!MessageCode::IoTimeout.is_keepalive());
}

/// Keepalive is not a logging message.
#[test]
fn keepalive_is_not_logging() {
    assert!(!MessageCode::NoOp.is_logging());
}
