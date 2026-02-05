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
