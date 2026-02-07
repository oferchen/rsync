// Comprehensive tests for timeout handling in the rsync protocol.
//
// These tests verify:
// 1. MSG_IO_TIMEOUT message code semantics
// 2. Timeout value encoding and decoding
// 3. Timeout interaction with multiplexed streams
// 4. Protocol-level timeout communication between peers
// 5. Edge cases in timeout value ranges
// 6. Timeout message parsing and generation

use protocol::{MessageCode, MessageHeader, MPLEX_BASE};

// =============================================================================
// MSG_IO_TIMEOUT Wire Format Tests
// =============================================================================

/// MSG_IO_TIMEOUT has wire value 33, used by daemon to communicate timeout.
#[test]
fn io_timeout_message_code_has_correct_wire_value() {
    assert_eq!(MessageCode::IoTimeout.as_u8(), 33);
}

/// MSG_IO_TIMEOUT should parse correctly from its wire value.
#[test]
fn io_timeout_message_code_from_u8() {
    let parsed = MessageCode::from_u8(33);
    assert_eq!(parsed, Some(MessageCode::IoTimeout));
}

/// MSG_IO_TIMEOUT should parse from its name.
#[test]
fn io_timeout_message_code_from_name() {
    let parsed: MessageCode = "MSG_IO_TIMEOUT".parse().expect("should parse");
    assert_eq!(parsed, MessageCode::IoTimeout);
}

/// MSG_IO_TIMEOUT has the expected canonical name.
#[test]
fn io_timeout_message_code_name() {
    assert_eq!(MessageCode::IoTimeout.name(), "MSG_IO_TIMEOUT");
}

/// MSG_IO_TIMEOUT is not a logging message (carries control data).
#[test]
fn io_timeout_is_not_logging_message() {
    assert!(!MessageCode::IoTimeout.is_logging());
}

/// MSG_IO_TIMEOUT has no log code equivalent.
#[test]
fn io_timeout_has_no_log_code() {
    assert!(MessageCode::IoTimeout.log_code().is_none());
}

// =============================================================================
// Timeout Value Encoding Tests
// =============================================================================

/// Timeout value of 0 should be encodable (means disable timeout).
#[test]
fn timeout_zero_is_valid_encoding() {
    // Zero timeout is a valid wire value meaning "disable timeout"
    let timeout_bytes = 0u32.to_le_bytes();
    let decoded = u32::from_le_bytes(timeout_bytes);
    assert_eq!(decoded, 0);
}

/// Typical timeout values should round-trip through encoding.
#[test]
fn typical_timeout_values_round_trip() {
    let typical_values = [30, 60, 120, 300, 600, 3600, 86400];

    for value in typical_values {
        let encoded = (value as u32).to_le_bytes();
        let decoded = u32::from_le_bytes(encoded);
        assert_eq!(decoded, value as u32, "value {value} should round-trip");
    }
}

/// Very short timeout values (1 second) should be valid.
#[test]
fn very_short_timeout_one_second_is_valid() {
    let timeout: u32 = 1;
    let encoded = timeout.to_le_bytes();
    let decoded = u32::from_le_bytes(encoded);
    assert_eq!(decoded, 1);
}

/// Maximum u32 timeout value should be encodable.
#[test]
fn max_u32_timeout_is_valid() {
    let timeout = u32::MAX;
    let encoded = timeout.to_le_bytes();
    let decoded = u32::from_le_bytes(encoded);
    assert_eq!(decoded, u32::MAX);
}

/// Timeout values near boundaries should work correctly.
#[test]
fn timeout_boundary_values() {
    let boundaries = [
        0u32,
        1,
        255,
        256,
        65535,
        65536,
        16777215,
        16777216,
        u32::MAX - 1,
        u32::MAX,
    ];

    for value in boundaries {
        let encoded = value.to_le_bytes();
        let decoded = u32::from_le_bytes(encoded);
        assert_eq!(decoded, value, "boundary value {value} should round-trip");
    }
}

// =============================================================================
// Timeout Message Frame Tests
// =============================================================================

/// MSG_IO_TIMEOUT should be distinct from MSG_IO_ERROR.
#[test]
fn io_timeout_distinct_from_io_error() {
    assert_ne!(MessageCode::IoTimeout, MessageCode::IoError);
    assert_ne!(MessageCode::IoTimeout.as_u8(), MessageCode::IoError.as_u8());

    // IoError is 22, IoTimeout is 33
    assert_eq!(MessageCode::IoError.as_u8(), 22);
    assert_eq!(MessageCode::IoTimeout.as_u8(), 33);
}

/// MSG_IO_TIMEOUT is in the sparse range between 23-32.
#[test]
fn io_timeout_in_sparse_range() {
    // Values 23-32 are invalid
    for i in 23u8..33 {
        assert!(
            MessageCode::from_u8(i).is_none(),
            "value {i} should be invalid"
        );
    }

    // 33 (IoTimeout) is valid
    assert!(MessageCode::from_u8(33).is_some());

    // Values 34-41 are invalid
    for i in 34u8..42 {
        assert!(
            MessageCode::from_u8(i).is_none(),
            "value {i} should be invalid"
        );
    }
}

// =============================================================================
// Timeout Message Payload Format Tests
// =============================================================================

/// Timeout payload should be exactly 4 bytes for u32 timeout value.
#[test]
fn timeout_payload_is_4_bytes() {
    let timeout: u32 = 30;
    let payload = timeout.to_le_bytes();
    assert_eq!(payload.len(), 4);
}

/// Timeout payload should use little-endian encoding.
#[test]
fn timeout_payload_is_little_endian() {
    let timeout: u32 = 0x01020304;
    let payload = timeout.to_le_bytes();

    // Little-endian: least significant byte first
    assert_eq!(payload[0], 0x04);
    assert_eq!(payload[1], 0x03);
    assert_eq!(payload[2], 0x02);
    assert_eq!(payload[3], 0x01);
}

/// Zero timeout encodes as all zero bytes.
#[test]
fn zero_timeout_encodes_as_zeros() {
    let timeout: u32 = 0;
    let payload = timeout.to_le_bytes();
    assert_eq!(payload, [0, 0, 0, 0]);
}

// =============================================================================
// Connection Timeout vs I/O Timeout Distinction
// =============================================================================

/// Connection timeout and I/O timeout are conceptually distinct.
/// Connection timeout (--contimeout): affects initial connection establishment
/// I/O timeout (--timeout): affects ongoing data transfer operations
#[test]
fn connection_timeout_vs_io_timeout_semantic_distinction() {
    // MSG_IO_TIMEOUT is for I/O timeout communication, not connection timeout.
    // Connection timeout errors would result in connection failure before
    // the multiplexed protocol stream is established.

    let io_timeout_code = MessageCode::IoTimeout;
    assert_eq!(io_timeout_code.name(), "MSG_IO_TIMEOUT");

    // The name includes "IO" to indicate it's for I/O operations,
    // not initial connection establishment.
    assert!(io_timeout_code.name().contains("IO"));
}

// =============================================================================
// Timeout Range Validity Tests
// =============================================================================

/// Timeout value 0 means disabled (infinite timeout).
#[test]
fn timeout_zero_means_disabled() {
    // In rsync, a timeout of 0 means "no timeout" or disabled
    let disabled_timeout: u32 = 0;
    let is_disabled = disabled_timeout == 0;
    assert!(is_disabled);
}

/// Small positive timeout values (1-10 seconds) are valid but unusual.
#[test]
fn small_positive_timeouts_are_valid() {
    for secs in 1u32..=10 {
        // All small positive values should be representable
        assert!(secs > 0);
        let _encoded = secs.to_le_bytes();
    }
}

/// Common timeout values in practice.
#[test]
fn common_timeout_values() {
    // Common timeout values users might set
    let common = [
        30,    // 30 seconds (default in some configurations)
        60,    // 1 minute
        120,   // 2 minutes
        300,   // 5 minutes
        600,   // 10 minutes
        900,   // 15 minutes
        1800,  // 30 minutes
        3600,  // 1 hour
        7200,  // 2 hours
        86400, // 24 hours
    ];

    for timeout in common {
        let encoded = (timeout as u32).to_le_bytes();
        let decoded = u32::from_le_bytes(encoded);
        assert_eq!(decoded, timeout as u32);
    }
}

// =============================================================================
// Timeout Error Condition Tests
// =============================================================================

/// Tests for recognizing timeout-related exit code value (30).
#[test]
fn timeout_exit_code_is_30() {
    // RERR_TIMEOUT = 30 in upstream rsync's errcode.h
    const RERR_TIMEOUT: i32 = 30;
    assert_eq!(RERR_TIMEOUT, 30);
}

/// Tests for recognizing connection timeout exit code value (35).
#[test]
fn connection_timeout_exit_code_is_35() {
    // RERR_CONTIMEOUT = 35 in upstream rsync's errcode.h
    const RERR_CONTIMEOUT: i32 = 35;
    assert_eq!(RERR_CONTIMEOUT, 35);
}

/// Timeout and connection timeout have different exit codes.
#[test]
fn timeout_exit_codes_are_distinct() {
    const RERR_TIMEOUT: i32 = 30;
    const RERR_CONTIMEOUT: i32 = 35;
    assert_ne!(RERR_TIMEOUT, RERR_CONTIMEOUT);
}

// =============================================================================
// MSG_IO_TIMEOUT in Message Code ALL Array
// =============================================================================

/// MSG_IO_TIMEOUT is included in MessageCode::ALL.
#[test]
fn io_timeout_in_all_array() {
    assert!(MessageCode::ALL.contains(&MessageCode::IoTimeout));
}

/// MSG_IO_TIMEOUT position in sorted array reflects its numeric value.
#[test]
fn io_timeout_array_position() {
    let all = MessageCode::all();
    let position = all.iter().position(|c| *c == MessageCode::IoTimeout);
    assert!(position.is_some());

    // IoTimeout (33) should come after IoError (22) and before NoOp (42)
    let io_error_pos = all.iter().position(|c| *c == MessageCode::IoError).unwrap();
    let noop_pos = all.iter().position(|c| *c == MessageCode::NoOp).unwrap();
    let timeout_pos = position.unwrap();

    assert!(timeout_pos > io_error_pos);
    assert!(timeout_pos < noop_pos);
}

// =============================================================================
// Timeout Message Display and Debug Tests
// =============================================================================

/// MSG_IO_TIMEOUT Display format.
#[test]
fn io_timeout_display_format() {
    let display = format!("{}", MessageCode::IoTimeout);
    assert_eq!(display, "MSG_IO_TIMEOUT");
}

/// MSG_IO_TIMEOUT Debug format.
#[test]
fn io_timeout_debug_format() {
    let debug = format!("{:?}", MessageCode::IoTimeout);
    assert_eq!(debug, "IoTimeout");
}

// =============================================================================
// Timeout with Multiplexed Stream Integration
// =============================================================================

/// MSG_IO_TIMEOUT can be used in multiplexed message headers.
#[test]
fn io_timeout_in_message_header() {
    let header = MessageHeader::new(MessageCode::IoTimeout, 4).expect("header should construct");
    assert_eq!(header.code(), MessageCode::IoTimeout);
    assert_eq!(header.payload_len(), 4);
}

/// MSG_IO_TIMEOUT header encodes correctly.
#[test]
fn io_timeout_header_encoding() {
    let header = MessageHeader::new(MessageCode::IoTimeout, 4).expect("header");
    let encoded = header.encode();

    // Header is 4 bytes: tag in upper byte, length in lower 3 bytes
    let decoded_word = u32::from_le_bytes(encoded);
    let tag = (decoded_word >> 24) as u8;
    let length = decoded_word & 0x00FF_FFFF;

    assert_eq!(tag, MPLEX_BASE + MessageCode::IoTimeout.as_u8());
    assert_eq!(length, 4);
}

/// MSG_IO_TIMEOUT header round-trips through encode/decode.
#[test]
fn io_timeout_header_round_trip() {
    let original = MessageHeader::new(MessageCode::IoTimeout, 4).expect("header");
    let encoded = original.encode();
    let decoded = MessageHeader::decode(&encoded).expect("decode should succeed");

    assert_eq!(decoded.code(), MessageCode::IoTimeout);
    assert_eq!(decoded.payload_len(), 4);
}

// =============================================================================
// Timeout Protocol Version Compatibility
// =============================================================================

/// MSG_IO_TIMEOUT is available in protocol versions that support multiplexing.
#[test]
fn io_timeout_available_in_multiplexed_protocols() {
    // MSG_IO_TIMEOUT is part of the multiplexed message set
    // It should be available whenever multiplexing is active
    // (protocol versions 29+)

    let code = MessageCode::IoTimeout;
    // The code exists and has a valid wire value
    assert!(MessageCode::from_u8(code.as_u8()).is_some());
}

// =============================================================================
// Timeout Edge Cases
// =============================================================================

/// Adjacent wire values to MSG_IO_TIMEOUT are not valid message codes.
#[test]
fn io_timeout_adjacent_values_invalid() {
    // 32 is invalid
    assert!(MessageCode::from_u8(32).is_none());

    // 33 is IoTimeout
    assert_eq!(MessageCode::from_u8(33), Some(MessageCode::IoTimeout));

    // 34 is invalid
    assert!(MessageCode::from_u8(34).is_none());
}

/// MSG_IO_TIMEOUT parsing rejects similar but incorrect names.
#[test]
fn io_timeout_parsing_rejects_incorrect_names() {
    // Variations that should not parse
    let invalid_names = [
        "IO_TIMEOUT",
        "MSG_TIMEOUT",
        "MSG_IO_timeout",
        "msg_io_timeout",
        "MSG IO TIMEOUT",
        "MSG_IOTIMEOUT",
        "MSG_IO_TIMEOUT ",
        " MSG_IO_TIMEOUT",
    ];

    for name in invalid_names {
        let result: Result<MessageCode, _> = name.parse();
        assert!(result.is_err(), "'{name}' should not parse as MessageCode");
    }
}

/// MSG_IO_TIMEOUT is Copy and Clone.
#[test]
fn io_timeout_is_copy_and_clone() {
    let code = MessageCode::IoTimeout;
    let copy = code;
    let clone = code;

    assert_eq!(code, copy);
    assert_eq!(code, clone);
}

/// MSG_IO_TIMEOUT can be used as HashMap key.
#[test]
fn io_timeout_as_hashmap_key() {
    use std::collections::HashMap;

    let mut map = HashMap::new();
    map.insert(MessageCode::IoTimeout, "timeout");

    assert_eq!(map.get(&MessageCode::IoTimeout), Some(&"timeout"));
}

// =============================================================================
// Keepalive and Timeout Interaction Tests
// =============================================================================

/// Keepalive messages are distinct from timeout messages.
/// Keepalives maintain connection liveness; timeouts signal protocol-level timeouts.
#[test]
fn keepalive_distinct_from_timeout() {
    // MSG_NOOP (42) can serve as a keepalive/heartbeat
    // MSG_IO_TIMEOUT (33) communicates actual timeout values

    assert_ne!(MessageCode::NoOp, MessageCode::IoTimeout);
    assert_ne!(MessageCode::NoOp.as_u8(), MessageCode::IoTimeout.as_u8());

    // Both are non-logging control messages
    assert!(!MessageCode::NoOp.is_logging());
    assert!(!MessageCode::IoTimeout.is_logging());
}

/// Both NoOp and IoTimeout are valid message codes.
#[test]
fn noop_and_io_timeout_both_valid() {
    assert!(MessageCode::from_u8(42).is_some()); // NoOp
    assert!(MessageCode::from_u8(33).is_some()); // IoTimeout
}

// =============================================================================
// Partial Transfer Timeout Handling
// =============================================================================

/// Timeout during partial transfer should be distinguishable from other errors.
/// RERR_TIMEOUT (30) is different from RERR_PARTIAL (23).
#[test]
fn timeout_distinct_from_partial_transfer_error() {
    const RERR_TIMEOUT: i32 = 30;
    const RERR_PARTIAL: i32 = 23;

    assert_ne!(RERR_TIMEOUT, RERR_PARTIAL);
}

/// File list exchange and file transfer can both experience timeouts.
/// The MSG_IO_TIMEOUT code is used for both scenarios.
#[test]
fn same_timeout_code_for_flist_and_transfer() {
    // There's only one MSG_IO_TIMEOUT code
    // It's used regardless of whether timeout occurs during:
    // - File list exchange
    // - File data transfer
    // - Delta computation

    let code = MessageCode::IoTimeout;
    assert_eq!(code.as_u8(), 33);
}

// =============================================================================
// Timeout Message Sequence Tests
// =============================================================================

/// MSG_IO_TIMEOUT can appear in a sequence with other messages.
#[test]
fn io_timeout_in_message_sequence() {
    // Simulate a sequence: Data, Info, IoTimeout, Data
    let codes = [
        MessageCode::Data,
        MessageCode::Info,
        MessageCode::IoTimeout,
        MessageCode::Data,
    ];

    for code in codes {
        let header = MessageHeader::new(code, 0).expect("header should construct");
        assert_eq!(header.code(), code);
    }
}

/// MSG_IO_TIMEOUT can have varying payload lengths.
#[test]
fn io_timeout_varying_payload_lengths() {
    // Common payload lengths for timeout messages
    let lengths = [0, 4, 8];

    for len in lengths {
        let header = MessageHeader::new(MessageCode::IoTimeout, len).expect("header");
        assert_eq!(header.payload_len(), len);
    }
}

// =============================================================================
// Multiplexed Stream send_msg / recv_msg Roundtrip Tests
// =============================================================================

use std::io::Cursor;
use protocol::{recv_msg, send_msg, MplexReader, MplexWriter, MessageFrame};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

/// MSG_IO_TIMEOUT with a 4-byte timeout payload roundtrips through send_msg/recv_msg.
#[test]
fn io_timeout_send_recv_roundtrip_with_timeout_payload() {
    let timeout_secs: u32 = 300;
    let payload = timeout_secs.to_le_bytes();

    let mut buf = Vec::new();
    send_msg(&mut buf, MessageCode::IoTimeout, &payload).expect("send io_timeout");

    let mut cursor = Cursor::new(&buf);
    let frame = recv_msg(&mut cursor).expect("recv io_timeout");

    assert_eq!(frame.code(), MessageCode::IoTimeout);
    assert_eq!(frame.payload().len(), 4);
    let decoded = u32::from_le_bytes(frame.payload().try_into().unwrap());
    assert_eq!(decoded, 300);
}

/// MSG_IO_TIMEOUT with zero timeout (disabled) roundtrips correctly.
#[test]
fn io_timeout_send_recv_zero_timeout_roundtrip() {
    let timeout_secs: u32 = 0;
    let payload = timeout_secs.to_le_bytes();

    let mut buf = Vec::new();
    send_msg(&mut buf, MessageCode::IoTimeout, &payload).expect("send");

    let mut cursor = Cursor::new(&buf);
    let frame = recv_msg(&mut cursor).expect("recv");

    assert_eq!(frame.code(), MessageCode::IoTimeout);
    let decoded = u32::from_le_bytes(frame.payload().try_into().unwrap());
    assert_eq!(decoded, 0);
}

/// MSG_IO_TIMEOUT with empty payload (no timeout value) roundtrips.
#[test]
fn io_timeout_send_recv_empty_payload_roundtrip() {
    let mut buf = Vec::new();
    send_msg(&mut buf, MessageCode::IoTimeout, &[]).expect("send");

    let mut cursor = Cursor::new(&buf);
    let frame = recv_msg(&mut cursor).expect("recv");

    assert_eq!(frame.code(), MessageCode::IoTimeout);
    assert!(frame.payload().is_empty());
}

/// MSG_IO_TIMEOUT encodes to the correct wire bytes.
#[test]
fn io_timeout_wire_bytes_correct() {
    let timeout_secs: u32 = 30;
    let payload = timeout_secs.to_le_bytes();

    let mut buf = Vec::new();
    send_msg(&mut buf, MessageCode::IoTimeout, &payload).expect("send");

    // Total: 4 byte header + 4 byte payload = 8 bytes
    assert_eq!(buf.len(), 8);

    // Verify header
    let raw_header = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let tag = (raw_header >> 24) as u8;
    let payload_len = raw_header & 0x00FF_FFFF;

    // IoTimeout code = 33, tag = MPLEX_BASE(7) + 33 = 40
    assert_eq!(tag, MPLEX_BASE + 33);
    assert_eq!(payload_len, 4);

    // Verify payload is the LE-encoded 30
    assert_eq!(&buf[4..], &30u32.to_le_bytes());
}

// =============================================================================
// MplexReader Timeout Message Handling Tests
// =============================================================================

/// MplexReader dispatches MSG_IO_TIMEOUT to the message handler as an OOB message.
#[test]
fn mplex_reader_dispatches_io_timeout_to_handler() {
    let mut stream = Vec::new();
    // Send a timeout message followed by data
    send_msg(&mut stream, MessageCode::IoTimeout, &30u32.to_le_bytes()).unwrap();
    send_msg(&mut stream, MessageCode::Data, b"after timeout").unwrap();

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
    assert_eq!(&buf[..n], b"after timeout");

    // Verify the timeout message was dispatched
    let captured = messages.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].0, MessageCode::IoTimeout);
    let timeout_val = u32::from_le_bytes(captured[0].1[..4].try_into().unwrap());
    assert_eq!(timeout_val, 30);
}

/// MplexReader skips IoTimeout messages (non-DATA) transparently when no handler is set.
#[test]
fn mplex_reader_skips_io_timeout_without_handler() {
    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::IoTimeout, &60u32.to_le_bytes()).unwrap();
    send_msg(&mut stream, MessageCode::Data, b"data after silent timeout").unwrap();

    let mut reader = MplexReader::new(Cursor::new(stream));
    // No handler set -- timeout is silently discarded

    let mut buf = [0u8; 64];
    let n = reader.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"data after silent timeout");
}

/// MplexReader handles multiple IoTimeout messages interleaved with data.
#[test]
fn mplex_reader_handles_multiple_io_timeouts_interleaved() {
    let mut stream = Vec::new();
    send_msg(&mut stream, MessageCode::Data, b"chunk1").unwrap();
    send_msg(&mut stream, MessageCode::IoTimeout, &30u32.to_le_bytes()).unwrap();
    send_msg(&mut stream, MessageCode::Data, b"chunk2").unwrap();
    send_msg(&mut stream, MessageCode::IoTimeout, &60u32.to_le_bytes()).unwrap();
    send_msg(&mut stream, MessageCode::IoTimeout, &90u32.to_le_bytes()).unwrap();
    send_msg(&mut stream, MessageCode::Data, b"chunk3").unwrap();

    let timeout_values = Arc::new(Mutex::new(Vec::new()));
    let timeout_values_clone = timeout_values.clone();

    let mut reader = MplexReader::new(Cursor::new(stream));
    reader.set_message_handler(move |code, payload| {
        if code == MessageCode::IoTimeout && payload.len() == 4 {
            let val = u32::from_le_bytes(payload.try_into().unwrap());
            timeout_values_clone.lock().unwrap().push(val);
        }
    });

    let mut data = Vec::new();
    let mut buf = [0u8; 64];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => data.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    assert_eq!(data, b"chunk1chunk2chunk3");

    let captured = timeout_values.lock().unwrap();
    assert_eq!(*captured, vec![30, 60, 90]);
}

// =============================================================================
// MplexWriter Timeout Message Tests
// =============================================================================

/// MplexWriter can send an IoTimeout message via write_message.
#[test]
fn mplex_writer_sends_io_timeout_message() {
    let mut output = Vec::new();
    let mut writer = MplexWriter::new(&mut output);

    let timeout_payload = 120u32.to_le_bytes();
    writer
        .write_message(MessageCode::IoTimeout, &timeout_payload)
        .unwrap();

    // Verify by decoding
    let mut cursor = Cursor::new(&output);
    let frame = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame.code(), MessageCode::IoTimeout);
    assert_eq!(frame.payload().len(), 4);
    let decoded = u32::from_le_bytes(frame.payload().try_into().unwrap());
    assert_eq!(decoded, 120);
}

/// MplexWriter flushes buffered data before sending IoTimeout message.
#[test]
fn mplex_writer_flushes_before_io_timeout() {
    let mut output = Vec::new();
    let mut writer = MplexWriter::new(&mut output);

    writer.write_all(b"buffered data").unwrap();
    assert_eq!(writer.buffered(), 13);

    writer
        .write_message(MessageCode::IoTimeout, &45u32.to_le_bytes())
        .unwrap();
    assert_eq!(writer.buffered(), 0);

    // Verify order: DATA then IoTimeout
    let mut cursor = Cursor::new(&output);
    let frame1 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame1.code(), MessageCode::Data);
    assert_eq!(frame1.payload(), b"buffered data");

    let frame2 = recv_msg(&mut cursor).unwrap();
    assert_eq!(frame2.code(), MessageCode::IoTimeout);
    let decoded = u32::from_le_bytes(frame2.payload().try_into().unwrap());
    assert_eq!(decoded, 45);
}

// =============================================================================
// MessageFrame IoTimeout Tests
// =============================================================================

/// MessageFrame can carry an IoTimeout with timeout payload.
#[test]
fn message_frame_io_timeout_roundtrip() {
    let payload = 600u32.to_le_bytes().to_vec();
    let frame = MessageFrame::new(MessageCode::IoTimeout, payload).unwrap();

    assert_eq!(frame.code(), MessageCode::IoTimeout);
    assert_eq!(frame.payload().len(), 4);

    let mut encoded = Vec::new();
    frame.encode_into_vec(&mut encoded).unwrap();

    let (decoded, remainder) = MessageFrame::decode_from_slice(&encoded).unwrap();
    assert!(remainder.is_empty());
    assert_eq!(decoded.code(), MessageCode::IoTimeout);
    let timeout_val = u32::from_le_bytes(decoded.payload().try_into().unwrap());
    assert_eq!(timeout_val, 600);
}

// =============================================================================
// End-to-End Writer-to-Reader IoTimeout Pipeline
// =============================================================================

/// Simulates a daemon sending its timeout configuration via MSG_IO_TIMEOUT
/// and the client receiving it through the MplexReader pipeline.
#[test]
fn end_to_end_io_timeout_writer_to_reader_pipeline() {
    let mut wire = Vec::new();

    // Simulate daemon sending timeout announcement followed by data
    {
        let mut writer = MplexWriter::new(&mut wire);

        // Daemon announces its 300-second timeout
        writer
            .write_message(MessageCode::IoTimeout, &300u32.to_le_bytes())
            .unwrap();

        // Then sends file data
        writer.write_data(b"file contents here").unwrap();

        // Sends a keepalive heartbeat
        writer.write_keepalive().unwrap();

        // Sends more data
        writer.write_data(b" and more data").unwrap();
        writer.flush().unwrap();
    }

    // Simulate client receiving the stream
    let received_timeout = Arc::new(Mutex::new(None));
    let keepalive_count = Arc::new(Mutex::new(0u32));
    let received_timeout_clone = received_timeout.clone();
    let keepalive_count_clone = keepalive_count.clone();

    let mut reader = MplexReader::new(Cursor::new(wire));
    reader.set_message_handler(move |code, payload| {
        match code {
            MessageCode::IoTimeout if payload.len() == 4 => {
                let val = u32::from_le_bytes(payload.try_into().unwrap());
                *received_timeout_clone.lock().unwrap() = Some(val);
            }
            MessageCode::NoOp => {
                *keepalive_count_clone.lock().unwrap() += 1;
            }
            _ => {}
        }
    });

    // Read all data
    let mut data = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => data.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    // Verify data integrity
    assert_eq!(data, b"file contents here and more data");

    // Verify timeout was received
    assert_eq!(*received_timeout.lock().unwrap(), Some(300));

    // Verify keepalive was counted
    assert_eq!(*keepalive_count.lock().unwrap(), 1);
}

/// MSG_IO_TIMEOUT is not a keepalive -- is_keepalive() returns false.
#[test]
fn io_timeout_is_not_keepalive() {
    assert!(!MessageCode::IoTimeout.is_keepalive());
    assert!(MessageCode::NoOp.is_keepalive());
}
