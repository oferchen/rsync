//! Keepalive mechanism tests.
//!
//! Upstream rsync's lull keepalive is an **empty `MSG_DATA`** frame (a
//! zero-length data message on the multiplexed stream), deliberately *not*
//! `MSG_NOOP`. `MSG_NOOP` (code 42) is a legacy protocol-30 construct that had
//! to be forwarded through the sender; the modern keepalive avoids forwarding
//! and works with every rsync version because a zero-length data frame adds no
//! bytes to the raw data stream and is silently absorbed by the peer.
//!
//! upstream: `io.c:maybe_send_keepalive()` (io.c:1453-1481) sends
//! `send_msg(MSG_DATA, "", 0, 0)`; the rationale is documented at io.c:1446-1452.
//!
//! These tests verify:
//!
//! - The keepalive wire format is an empty `MSG_DATA` frame (tag = MPLEX_BASE, len 0)
//! - A received empty `MSG_DATA` keepalive is absorbed silently, never surfaced
//!   as data or as an out-of-band message, and never read as end-of-stream
//! - Keepalives do not interfere with data transfer
//! - Legacy `MSG_NOOP` is still recognized as a keepalive for proto-30 peers

use std::io::{self, Cursor, Read};
use std::sync::{Arc, Mutex};

use protocol::{
    MPLEX_BASE, MessageCode, MessageFrame, MessageHeader, MplexReader, MplexWriter, recv_msg,
    recv_msg_into, send_keepalive, send_msg,
};

/// Keepalive produces a 4-byte header with zero-length payload.
#[test]
fn keepalive_wire_format_is_4_byte_header_only() {
    let mut buf = Vec::new();
    send_keepalive(&mut buf).expect("send_keepalive must succeed");

    // Empty MSG_DATA = header only.
    assert_eq!(
        buf.len(),
        4,
        "keepalive must be exactly 4 bytes (header only)"
    );
}

/// The keepalive header encodes MSG_DATA (tag = MPLEX_BASE + 0 = 7) with length 0.
#[test]
fn keepalive_header_encodes_data_tag() {
    let mut buf = Vec::new();
    send_keepalive(&mut buf).expect("send_keepalive must succeed");

    let raw_header = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let tag = (raw_header >> 24) as u8;
    let payload_len = raw_header & 0x00FF_FFFF;

    // upstream: io.c:1473 send_msg(MSG_DATA, "", 0, 0). MSG_DATA is code 0, so
    // the multiplex tag is MPLEX_BASE (7), NOT the legacy MSG_NOOP tag (49).
    assert_eq!(
        tag,
        MPLEX_BASE + MessageCode::Data.as_u8(),
        "tag must be MPLEX_BASE + MSG_DATA"
    );
    assert_eq!(tag, 7, "tag must be 7 (MPLEX_BASE + 0)");
    assert_ne!(
        tag,
        MPLEX_BASE + MessageCode::NoOp.as_u8(),
        "keepalive must NOT be MSG_NOOP"
    );
    assert_eq!(payload_len, 0, "keepalive payload length must be 0");
}

/// send_keepalive produces the same bytes as send_msg(Data, &[]).
#[test]
fn keepalive_matches_explicit_empty_data_send() {
    let mut keepalive_buf = Vec::new();
    send_keepalive(&mut keepalive_buf).expect("send_keepalive must succeed");

    let mut data_buf = Vec::new();
    send_msg(&mut data_buf, MessageCode::Data, &[]).expect("send_msg must succeed");

    assert_eq!(
        keepalive_buf, data_buf,
        "send_keepalive must produce identical bytes to send_msg(Data, &[])"
    );
}

/// Decoding a keepalive message yields an empty DATA frame.
#[test]
fn keepalive_roundtrip_decodes_to_empty_data() {
    let mut buf = Vec::new();
    send_keepalive(&mut buf).expect("send_keepalive must succeed");

    let mut cursor = Cursor::new(&buf);
    let frame = recv_msg(&mut cursor).expect("recv_msg must succeed");

    assert_eq!(frame.code(), MessageCode::Data, "code must be Data");
    assert!(frame.payload().is_empty(), "payload must be empty");
}

/// The legacy is_keepalive() helper still identifies MSG_NOOP (proto-30 peers).
#[test]
fn is_keepalive_identifies_legacy_noop_only() {
    assert!(
        MessageCode::NoOp.is_keepalive(),
        "legacy NoOp must still be recognized as keepalive"
    );

    // All other codes, including Data, are not flagged by is_keepalive(): the
    // modern empty-DATA keepalive is recognized structurally (zero-length DATA),
    // not by code.
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

/// recv_msg decodes the keepalive as an empty DATA frame.
#[test]
fn recv_msg_decodes_keepalive() {
    let mut buf = Vec::new();
    send_keepalive(&mut buf).unwrap();

    let mut cursor = Cursor::new(&buf);
    let frame = recv_msg(&mut cursor).unwrap();

    assert_eq!(frame.code(), MessageCode::Data);
    assert!(frame.payload().is_empty());
}

/// recv_msg_into decodes the keepalive and leaves the buffer empty.
#[test]
fn recv_msg_into_decodes_keepalive_empty_buffer() {
    let mut buf = Vec::new();
    send_keepalive(&mut buf).unwrap();

    let mut cursor = Cursor::new(&buf);
    let mut payload_buf = vec![0xFF; 64]; // pre-filled
    let code = recv_msg_into(&mut cursor, &mut payload_buf).unwrap();

    assert_eq!(code, MessageCode::Data);
    assert!(payload_buf.is_empty(), "keepalive must clear the buffer");
}

/// Keepalive messages interleaved with data are silently absorbed by MplexReader.
#[test]
fn mplex_reader_absorbs_keepalives_between_data() {
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

    assert_eq!(
        result, b"hello world",
        "data must be reconstructed correctly"
    );
}

/// Leading keepalives do not cause a premature EOF in MplexReader.
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

/// Empty-DATA keepalives are absorbed silently and NOT reported to the handler.
///
/// upstream: an empty MSG_DATA contributes zero bytes to the data channel and is
/// consumed by the ordinary read path, never surfacing as an out-of-band message
/// (contrast the legacy MSG_NOOP, which was a distinct control frame).
#[test]
fn mplex_reader_does_not_report_empty_data_keepalive_to_handler() {
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
    assert!(
        captured.is_empty(),
        "empty-DATA keepalives must not be surfaced as out-of-band messages"
    );
}

/// A received legacy MSG_NOOP is still recognized and absorbed by the reader.
#[test]
fn mplex_reader_absorbs_legacy_noop_keepalive() {
    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::NoOp, &[]).unwrap();
    send_msg(&mut stream, MessageCode::Data, b"payload").unwrap();

    let saw_noop = Arc::new(Mutex::new(false));
    let saw_noop_clone = saw_noop.clone();

    let mut reader = MplexReader::new(Cursor::new(stream));
    reader.set_message_handler(move |code, _payload| {
        if code == MessageCode::NoOp {
            *saw_noop_clone.lock().unwrap() = true;
        }
    });

    let mut buf = [0u8; 64];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"payload");
    assert!(
        *saw_noop.lock().unwrap(),
        "legacy NoOp must reach the handler and be absorbed"
    );
}

/// Keepalives interleaved with other out-of-band messages and data.
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
        oob_clone.lock().unwrap().push((code, payload.to_vec()));
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
    // Only true out-of-band messages surface; empty-DATA keepalives are absorbed.
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0].0, MessageCode::Info);
    assert_eq!(captured[1].0, MessageCode::Warning);
}

/// A burst of keepalives can be sent and decoded without error.
#[test]
fn multiple_keepalives_in_sequence() {
    let count = 100;
    let mut buf = Vec::new();
    for _ in 0..count {
        send_keepalive(&mut buf).unwrap();
    }

    // All must decode as empty DATA frames.
    let mut cursor = Cursor::new(&buf);
    for i in 0..count {
        let frame = recv_msg(&mut cursor).unwrap_or_else(|_| panic!("keepalive {i} must decode"));
        assert_eq!(frame.code(), MessageCode::Data);
        assert!(frame.payload().is_empty());
    }
}

/// A stream of only keepalives reaches EOF (never a spurious Ok(0)) after
/// consuming all of them.
#[test]
fn mplex_reader_only_keepalives_reaches_eof() {
    let mut stream = Vec::new();
    for _ in 0..10 {
        send_keepalive(&mut stream).unwrap();
    }

    let mut reader = MplexReader::new(Cursor::new(stream));
    let mut buf = [0u8; 64];

    // Reading absorbs all keepalives, then hits real EOF as an error (not Ok(0),
    // which callers would treat as a legitimate end-of-data marker).
    let result = reader.read(&mut buf);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
}

/// Keepalives followed by data -- MplexReader returns data after absorbing them.
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

/// MplexWriter::write_keepalive sends a properly formatted empty MSG_DATA.
#[test]
fn mplex_writer_keepalive_produces_correct_frame() {
    let mut output = Vec::new();
    let mut writer = MplexWriter::new(&mut output);

    writer.write_keepalive().unwrap();

    // Verify it can be decoded as an empty DATA frame.
    let mut cursor = Cursor::new(&output);
    let frame = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame.code(), MessageCode::Data);
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

    // Verify: DATA frame "buffered", then the empty DATA keepalive frame.
    let mut cursor = Cursor::new(&output);

    let frame1 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame1.code(), MessageCode::Data);
    assert_eq!(frame1.payload(), b"buffered");

    let frame2 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame2.code(), MessageCode::Data);
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

    // Verify the sequence: data and empty-DATA keepalives, all MSG_DATA frames.
    let mut cursor = Cursor::new(&output);

    let f1 = recv_msg(&mut cursor).unwrap();
    assert_eq!(f1.code(), MessageCode::Data);
    assert_eq!(f1.payload(), b"chunk1");

    let f2 = recv_msg(&mut cursor).unwrap();
    assert_eq!(f2.code(), MessageCode::Data);
    assert!(f2.payload().is_empty());

    let f3 = recv_msg(&mut cursor).unwrap();
    assert_eq!(f3.code(), MessageCode::Data);
    assert_eq!(f3.payload(), b"chunk2");

    let f4 = recv_msg(&mut cursor).unwrap();
    assert_eq!(f4.code(), MessageCode::Data);
    assert!(f4.payload().is_empty());

    let f5 = recv_msg(&mut cursor).unwrap();
    assert_eq!(f5.code(), MessageCode::Data);
    assert!(f5.payload().is_empty());

    let f6 = recv_msg(&mut cursor).unwrap();
    assert_eq!(f6.code(), MessageCode::Data);
    assert_eq!(f6.payload(), b"chunk3");
}

/// An empty-DATA keepalive can be constructed and encoded via MessageFrame.
#[test]
fn message_frame_empty_data_keepalive_roundtrip() {
    let frame = MessageFrame::new(MessageCode::Data, vec![]).unwrap();
    assert!(frame.payload().is_empty());

    let mut encoded = Vec::new();
    frame.encode_into_vec(&mut encoded).unwrap();

    let (decoded, remainder) = MessageFrame::decode_from_slice(&encoded).unwrap();
    assert!(remainder.is_empty());
    assert_eq!(decoded.code(), MessageCode::Data);
    assert!(decoded.payload().is_empty());
}

/// An empty-DATA MessageFrame encodes to the same bytes as send_keepalive.
#[test]
fn message_frame_keepalive_matches_send_keepalive() {
    let frame = MessageFrame::new(MessageCode::Data, vec![]).unwrap();
    let mut frame_bytes = Vec::new();
    frame.encode_into_vec(&mut frame_bytes).unwrap();

    let mut keepalive_bytes = Vec::new();
    send_keepalive(&mut keepalive_bytes).unwrap();

    assert_eq!(frame_bytes, keepalive_bytes);
}

/// Simulates a transfer pipeline where keepalives are sent during a long
/// operation. The writer emits empty-DATA keepalives between data chunks; the
/// reader absorbs them transparently and never surfaces them to the handler.
#[test]
fn end_to_end_keepalive_during_transfer() {
    let mut wire = Vec::new();

    // Simulate the sender writing chunks with lull keepalives.
    {
        let mut writer = MplexWriter::new(&mut wire);

        writer.write_data(b"file header data").unwrap();

        // Simulate a long operation -- emit keepalives.
        writer.write_keepalive().unwrap();
        writer.write_keepalive().unwrap();
        writer.write_keepalive().unwrap();

        // Send an info message (like progress).
        writer
            .write_message(MessageCode::Info, b"checksumming...")
            .unwrap();

        writer.write_keepalive().unwrap();
        writer.write_data(b"file body data").unwrap();
        writer.write_keepalive().unwrap();
        writer.write_data(b"file trailer").unwrap();
        writer.flush().unwrap();
    }

    // Simulate the receiver reading the stream.
    let oob_count = Arc::new(Mutex::new(0u32));
    let info_messages = Arc::new(Mutex::new(Vec::new()));
    let oob_count_clone = oob_count.clone();
    let info_messages_clone = info_messages.clone();

    let mut reader = MplexReader::new(Cursor::new(wire));
    reader.set_message_handler(move |code, payload| {
        *oob_count_clone.lock().unwrap() += 1;
        if code == MessageCode::Info {
            info_messages_clone
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(payload).to_string());
        }
    });

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

    assert_eq!(
        data, b"file header datafile body datafile trailer",
        "data must be reconstructed without keepalive interference"
    );

    // Only the single Info message is surfaced out-of-band; the empty-DATA
    // keepalives are absorbed into the data path, matching upstream.
    assert_eq!(
        *oob_count.lock().unwrap(),
        1,
        "only the Info message must reach the handler"
    );

    let infos = info_messages.lock().unwrap();
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0], "checksumming...");
}

/// The keepalive MessageHeader (empty MSG_DATA) encodes and decodes correctly.
#[test]
fn keepalive_header_roundtrip() {
    let header = MessageHeader::new(MessageCode::Data, 0).unwrap();
    assert_eq!(header.code(), MessageCode::Data);
    assert_eq!(header.payload_len(), 0);

    let encoded = header.encode();
    let decoded = MessageHeader::decode(&encoded).unwrap();
    assert_eq!(decoded.code(), MessageCode::Data);
    assert_eq!(decoded.payload_len(), 0);
}

use std::io::Write;

/// Legacy MSG_NOOP remains distinct from MSG_IO_TIMEOUT in code and semantics.
#[test]
fn legacy_noop_distinct_from_io_timeout() {
    assert_ne!(MessageCode::NoOp, MessageCode::IoTimeout);
    assert_ne!(MessageCode::NoOp.as_u8(), MessageCode::IoTimeout.as_u8());

    assert!(MessageCode::NoOp.is_keepalive());
    assert!(!MessageCode::IoTimeout.is_keepalive());
}

/// Legacy MSG_NOOP is not a logging message.
#[test]
fn legacy_noop_is_not_logging() {
    assert!(!MessageCode::NoOp.is_logging());
}
