// Comprehensive tests for MessageCode and LogCode handling
//
// These tests verify:
// - Wire format compatibility with upstream rsync 3.4.1
// - Complete coverage of all message code variants
// - Edge cases in parsing, formatting, and conversion
// - Semantic correctness of message classification
// - Cross-component integration between MessageCode and LogCode

use super::*;
use std::collections::HashMap;

// ============================================================================
// Wire Format Compatibility Tests
// These tests verify that our message codes match upstream rsync's exact values
// ============================================================================

/// Verifies that all message codes have the exact numeric values expected by
/// upstream rsync 3.4.1. These values are defined in rsync.h and must match
/// exactly for protocol compatibility.
#[test]
fn message_code_wire_values_match_upstream_rsync_3_4_1() {
    // Values from upstream rsync.h enum msgcode
    let upstream_values: [(MessageCode, u8, &str); 18] = [
        (MessageCode::Data, 0, "MSG_DATA"),
        (MessageCode::ErrorXfer, 1, "MSG_ERROR_XFER"),
        (MessageCode::Info, 2, "MSG_INFO"),
        (MessageCode::Error, 3, "MSG_ERROR"),
        (MessageCode::Warning, 4, "MSG_WARNING"),
        (MessageCode::ErrorSocket, 5, "MSG_ERROR_SOCKET"),
        (MessageCode::Log, 6, "MSG_LOG"),
        (MessageCode::Client, 7, "MSG_CLIENT"),
        (MessageCode::ErrorUtf8, 8, "MSG_ERROR_UTF8"),
        (MessageCode::Redo, 9, "MSG_REDO"),
        (MessageCode::Stats, 10, "MSG_STATS"),
        (MessageCode::IoError, 22, "MSG_IO_ERROR"),
        (MessageCode::IoTimeout, 33, "MSG_IO_TIMEOUT"),
        (MessageCode::NoOp, 42, "MSG_NOOP"),
        (MessageCode::ErrorExit, 86, "MSG_ERROR_EXIT"),
        (MessageCode::Success, 100, "MSG_SUCCESS"),
        (MessageCode::Deleted, 101, "MSG_DELETED"),
        (MessageCode::NoSend, 102, "MSG_NO_SEND"),
    ];

    for (code, expected_value, expected_name) in upstream_values {
        assert_eq!(
            code.as_u8(),
            expected_value,
            "MessageCode::{:?} should have wire value {} but has {}",
            code,
            expected_value,
            code.as_u8()
        );
        assert_eq!(
            code.name(),
            expected_name,
            "MessageCode::{:?} should have name {} but has {}",
            code,
            expected_name,
            code.name()
        );
    }
}

/// Verifies that all log codes have the exact numeric values expected by
/// upstream rsync 3.4.1. These values are defined in rsync.h enum logcode.
#[test]
fn log_code_wire_values_match_upstream_rsync_3_4_1() {
    // Values from upstream rsync.h enum logcode
    let upstream_values: [(LogCode, u8, &str); 9] = [
        (LogCode::None, 0, "FNONE"),
        (LogCode::ErrorXfer, 1, "FERROR_XFER"),
        (LogCode::Info, 2, "FINFO"),
        (LogCode::Error, 3, "FERROR"),
        (LogCode::Warning, 4, "FWARNING"),
        (LogCode::ErrorSocket, 5, "FERROR_SOCKET"),
        (LogCode::Log, 6, "FLOG"),
        (LogCode::Client, 7, "FCLIENT"),
        (LogCode::ErrorUtf8, 8, "FERROR_UTF8"),
    ];

    for (code, expected_value, expected_name) in upstream_values {
        assert_eq!(
            code.as_u8(),
            expected_value,
            "LogCode::{:?} should have wire value {} but has {}",
            code,
            expected_value,
            code.as_u8()
        );
        assert_eq!(
            code.name(),
            expected_name,
            "LogCode::{:?} should have name {} but has {}",
            code,
            expected_name,
            code.name()
        );
    }
}

// ============================================================================
// Message Code Categorization Tests
// ============================================================================

/// Verifies that the is_logging() method correctly identifies message codes
/// that carry human-readable log output.
#[test]
fn message_code_is_logging_correctly_categorizes_all_variants() {
    // These codes carry human-readable log messages
    let logging_codes = [
        MessageCode::ErrorXfer,
        MessageCode::Info,
        MessageCode::Error,
        MessageCode::Warning,
        MessageCode::ErrorSocket,
        MessageCode::Log,
        MessageCode::Client,
        MessageCode::ErrorUtf8,
    ];

    // These codes carry binary/control data
    let non_logging_codes = [
        MessageCode::Data,
        MessageCode::Redo,
        MessageCode::Stats,
        MessageCode::IoError,
        MessageCode::IoTimeout,
        MessageCode::NoOp,
        MessageCode::ErrorExit,
        MessageCode::Success,
        MessageCode::Deleted,
        MessageCode::NoSend,
    ];

    for code in logging_codes {
        assert!(
            code.is_logging(),
            "MessageCode::{:?} should be classified as logging",
            code
        );
    }

    for code in non_logging_codes {
        assert!(
            !code.is_logging(),
            "MessageCode::{:?} should NOT be classified as logging",
            code
        );
    }

    // Verify we covered all codes
    assert_eq!(
        logging_codes.len() + non_logging_codes.len(),
        MessageCode::ALL.len(),
        "Test should cover all message codes"
    );
}

/// Verifies that error-related message codes are properly distinguished.
#[test]
fn message_code_error_variants_are_correctly_identified() {
    let error_codes = [
        MessageCode::ErrorXfer,
        MessageCode::Error,
        MessageCode::ErrorSocket,
        MessageCode::ErrorUtf8,
        MessageCode::ErrorExit,
        MessageCode::IoError,
    ];

    for code in error_codes {
        assert!(
            code.name().contains("ERROR") || code.name().contains("IO_ERROR"),
            "Error code {:?} should have ERROR in name: {}",
            code,
            code.name()
        );
    }
}

/// Verifies that the MSG_FLUSH alias is correctly mapped to MSG_INFO.
#[test]
fn message_code_flush_alias_is_info() {
    assert_eq!(MessageCode::FLUSH, MessageCode::Info);
    assert_eq!(MessageCode::FLUSH.as_u8(), 2);

    // Parsing MSG_FLUSH should yield Info
    let parsed: MessageCode = "MSG_FLUSH".parse().expect("MSG_FLUSH should parse");
    assert_eq!(parsed, MessageCode::Info);

    // But name() returns MSG_INFO, not MSG_FLUSH
    assert_eq!(MessageCode::FLUSH.name(), "MSG_INFO");
}

// ============================================================================
// MessageCode/LogCode Conversion Tests
// ============================================================================

/// Verifies bidirectional conversion between MessageCode and LogCode for
/// all logging variants.
#[test]
fn message_code_log_code_bidirectional_conversion() {
    // All LogCodes except None should have a MessageCode equivalent
    for &log in LogCode::all() {
        if log == LogCode::None {
            assert!(
                MessageCode::from_log_code(log).is_none(),
                "LogCode::None should not convert to MessageCode"
            );
            continue;
        }

        let msg = MessageCode::from_log_code(log)
            .unwrap_or_else(|| panic!("LogCode::{log:?} should convert to MessageCode"));

        let back = msg
            .log_code()
            .unwrap_or_else(|| panic!("MessageCode::{msg:?} should convert back to LogCode"));

        assert_eq!(
            back, log,
            "Round-trip conversion failed for LogCode::{log:?}"
        );
    }
}

/// Verifies that message codes without log equivalents correctly fail conversion.
#[test]
fn message_code_without_log_equivalent_fails_conversion() {
    let codes_without_log = [
        MessageCode::Data,
        MessageCode::Redo,
        MessageCode::Stats,
        MessageCode::IoError,
        MessageCode::IoTimeout,
        MessageCode::NoOp,
        MessageCode::ErrorExit,
        MessageCode::Success,
        MessageCode::Deleted,
        MessageCode::NoSend,
    ];

    for code in codes_without_log {
        assert!(
            code.log_code().is_none(),
            "MessageCode::{:?} should have no log code equivalent",
            code
        );

        let result = LogCode::try_from(code);
        assert!(
            result.is_err(),
            "LogCode::try_from(MessageCode::{:?}) should fail",
            code
        );
    }
}

/// Verifies that numeric values align between corresponding MessageCode and
/// LogCode variants.
#[test]
fn message_code_and_log_code_numeric_alignment() {
    // These pairs should have the same numeric value
    let aligned_pairs = [
        (MessageCode::ErrorXfer, LogCode::ErrorXfer),
        (MessageCode::Info, LogCode::Info),
        (MessageCode::Error, LogCode::Error),
        (MessageCode::Warning, LogCode::Warning),
        (MessageCode::ErrorSocket, LogCode::ErrorSocket),
        (MessageCode::Log, LogCode::Log),
        (MessageCode::Client, LogCode::Client),
        (MessageCode::ErrorUtf8, LogCode::ErrorUtf8),
    ];

    for (msg, log) in aligned_pairs {
        assert_eq!(
            msg.as_u8(),
            log.as_u8(),
            "MessageCode::{msg:?} and LogCode::{log:?} should have same numeric value"
        );
    }
}

// ============================================================================
// Parsing Edge Cases
// ============================================================================

/// Tests that parsing is case-sensitive and rejects incorrect casing.
#[test]
fn message_code_parsing_is_case_sensitive() {
    let invalid_cases = [
        "msg_data",
        "Msg_Data",
        "MSG_data",
        "MSG_DATA ",
        " MSG_DATA",
        "MSG DATA",
        "MSGDATA",
    ];

    for invalid in invalid_cases {
        let result: Result<MessageCode, _> = invalid.parse();
        assert!(
            result.is_err(),
            "'{invalid}' should not parse as MessageCode"
        );
    }
}

/// Tests that LogCode parsing is case-sensitive and rejects incorrect casing.
#[test]
fn log_code_parsing_is_case_sensitive() {
    let invalid_cases = [
        "finfo", "Finfo", "FINFO ", " FINFO", "F INFO", "FLOG ", " ferror",
    ];

    for invalid in invalid_cases {
        let result: Result<LogCode, _> = invalid.parse();
        assert!(result.is_err(), "'{invalid}' should not parse as LogCode");
    }
}

/// Tests that empty strings are rejected during parsing.
#[test]
fn message_code_rejects_empty_string() {
    let result: Result<MessageCode, _> = "".parse();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.invalid_name(), "");
}

#[test]
fn log_code_rejects_empty_string() {
    let result: Result<LogCode, _> = "".parse();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.invalid_name(), Some(""));
}

// ============================================================================
// Boundary Value Tests for Numeric Parsing
// ============================================================================

/// Tests from_u8 at boundaries around valid code ranges.
#[test]
fn message_code_from_u8_boundary_values() {
    // Valid boundary values
    assert!(MessageCode::from_u8(0).is_some()); // Data
    assert!(MessageCode::from_u8(10).is_some()); // Stats
    assert!(MessageCode::from_u8(22).is_some()); // IoError
    assert!(MessageCode::from_u8(33).is_some()); // IoTimeout
    assert!(MessageCode::from_u8(42).is_some()); // NoOp
    assert!(MessageCode::from_u8(86).is_some()); // ErrorExit
    assert!(MessageCode::from_u8(100).is_some()); // Success
    assert!(MessageCode::from_u8(102).is_some()); // NoSend

    // Invalid boundary values (just outside valid ranges)
    assert!(MessageCode::from_u8(11).is_none()); // After Stats
    assert!(MessageCode::from_u8(21).is_none()); // Before IoError
    assert!(MessageCode::from_u8(23).is_none()); // After IoError
    assert!(MessageCode::from_u8(32).is_none()); // Before IoTimeout
    assert!(MessageCode::from_u8(34).is_none()); // After IoTimeout
    assert!(MessageCode::from_u8(41).is_none()); // Before NoOp
    assert!(MessageCode::from_u8(43).is_none()); // After NoOp
    assert!(MessageCode::from_u8(85).is_none()); // Before ErrorExit
    assert!(MessageCode::from_u8(87).is_none()); // After ErrorExit
    assert!(MessageCode::from_u8(99).is_none()); // Before Success
    assert!(MessageCode::from_u8(103).is_none()); // After NoSend

    // Extreme values
    assert!(MessageCode::from_u8(u8::MAX).is_none());
}

/// Tests LogCode from_u8 at boundary values.
#[test]
fn log_code_from_u8_boundary_values() {
    // Valid range is 0-8
    for i in 0..=8 {
        assert!(
            LogCode::from_u8(i).is_some(),
            "LogCode::from_u8({i}) should succeed"
        );
    }

    // Invalid values
    for i in 9..=u8::MAX {
        assert!(
            LogCode::from_u8(i).is_none(),
            "LogCode::from_u8({i}) should fail"
        );
    }
}

// ============================================================================
// ALL Array Consistency Tests
// ============================================================================

/// Verifies that MessageCode::ALL contains exactly all variants in ascending
/// numeric order.
#[test]
fn message_code_all_is_complete_and_ordered() {
    let all = MessageCode::all();

    // Check completeness by verifying round-trip for all valid u8 values
    let mut count = 0;
    for value in 0..=u8::MAX {
        if let Some(code) = MessageCode::from_u8(value) {
            assert!(
                all.contains(&code),
                "MessageCode::ALL missing variant for value {value}"
            );
            count += 1;
        }
    }
    assert_eq!(count, all.len(), "ALL should contain all valid codes");

    // Check ordering
    for i in 1..all.len() {
        assert!(
            all[i - 1].as_u8() < all[i].as_u8(),
            "MessageCode::ALL is not sorted: {:?} >= {:?}",
            all[i - 1],
            all[i]
        );
    }
}

/// Verifies that LogCode::ALL contains exactly all variants in ascending order.
#[test]
fn log_code_all_is_complete_and_ordered() {
    let all = LogCode::all();

    // Check completeness
    let mut count = 0;
    for value in 0..=u8::MAX {
        if let Some(code) = LogCode::from_u8(value) {
            assert!(
                all.contains(&code),
                "LogCode::ALL missing variant for value {value}"
            );
            count += 1;
        }
    }
    assert_eq!(count, all.len(), "ALL should contain all valid codes");

    // Check ordering
    for i in 1..all.len() {
        assert!(
            all[i - 1].as_u8() < all[i].as_u8(),
            "LogCode::ALL is not sorted"
        );
    }
}

// ============================================================================
// Trait Implementation Tests
// ============================================================================

/// Tests that MessageCode implements Clone correctly.
#[test]
fn message_code_clone_preserves_value() {
    for &code in MessageCode::all() {
        let cloned = code;
        assert_eq!(code, cloned);
        assert_eq!(code.as_u8(), cloned.as_u8());
    }
}

/// Tests that MessageCode implements Copy (implicit test via assignment).
#[test]
fn message_code_is_copy() {
    let code = MessageCode::Info;
    let copy = code;
    let another = code;
    assert_eq!(copy, another);
    // If this compiles and runs, MessageCode is Copy
}

/// Tests that LogCode implements Hash correctly by using it in a HashMap.
#[test]
fn log_code_hash_works_in_hashmap() {
    let mut map: HashMap<LogCode, &str> = HashMap::new();

    for &code in LogCode::all() {
        map.insert(code, code.name());
    }

    assert_eq!(map.len(), LogCode::ALL.len());

    for &code in LogCode::all() {
        assert_eq!(map.get(&code), Some(&code.name()));
    }
}

/// Tests that MessageCode implements Hash correctly.
#[test]
fn message_code_hash_works_in_hashmap() {
    let mut map: HashMap<MessageCode, &str> = HashMap::new();

    for &code in MessageCode::all() {
        map.insert(code, code.name());
    }

    assert_eq!(map.len(), MessageCode::ALL.len());

    for &code in MessageCode::all() {
        assert_eq!(map.get(&code), Some(&code.name()));
    }
}

// ============================================================================
// Error Type Tests
// ============================================================================

/// Tests ParseMessageCodeError construction and accessors.
#[test]
fn parse_message_code_error_construction() {
    let err = ParseMessageCodeError::new("INVALID_CODE");

    assert_eq!(err.invalid_name(), "INVALID_CODE");

    let display = err.to_string();
    assert!(display.contains("INVALID_CODE"));
    assert!(display.contains("unknown"));
}

/// Tests ParseLogCodeError construction and accessors.
#[test]
fn parse_log_code_error_construction_from_value() {
    let err = ParseLogCodeError::new(99);

    assert_eq!(err.invalid_value(), Some(99));
    assert_eq!(err.invalid_name(), None);

    let display = err.to_string();
    assert!(display.contains("99"));
}

#[test]
fn parse_log_code_error_construction_from_name() {
    let err = ParseLogCodeError::new_name("INVALID");

    assert_eq!(err.invalid_value(), None);
    assert_eq!(err.invalid_name(), Some("INVALID"));

    let display = err.to_string();
    assert!(display.contains("INVALID"));
}

/// Tests LogCodeConversionError construction and accessors.
#[test]
fn log_code_conversion_error_accessors() {
    let no_msg_err = LogCodeConversionError::NoMessageEquivalent(LogCode::None);
    assert_eq!(no_msg_err.log_code(), Some(LogCode::None));
    assert_eq!(no_msg_err.message_code(), None);

    let no_log_err = LogCodeConversionError::NoLogEquivalent(MessageCode::Data);
    assert_eq!(no_log_err.log_code(), None);
    assert_eq!(no_log_err.message_code(), Some(MessageCode::Data));
}

// ============================================================================
// Display/Debug Consistency Tests
// ============================================================================

/// Verifies Display output uses MSG_* format for all MessageCodes.
#[test]
fn message_code_display_uses_msg_prefix() {
    for &code in MessageCode::all() {
        let display = format!("{}", code);
        assert!(
            display.starts_with("MSG_"),
            "MessageCode::{:?} display should start with MSG_: {}",
            code,
            display
        );
    }
}

/// Verifies Display output uses F* format for all LogCodes.
#[test]
fn log_code_display_uses_f_prefix() {
    for &code in LogCode::all() {
        let display = format!("{}", code);
        assert!(
            display.starts_with("F"),
            "LogCode::{:?} display should start with F: {}",
            code,
            display
        );
    }
}

/// Verifies Debug output differs from Display for MessageCode.
#[test]
fn message_code_debug_differs_from_display() {
    for &code in MessageCode::all() {
        let debug = format!("{:?}", code);
        let display = format!("{}", code);
        assert_ne!(
            debug, display,
            "Debug and Display should differ for MessageCode::{:?}",
            code
        );
    }
}

// ============================================================================
// Semantic Tests for Specific Message Codes
// ============================================================================

/// MSG_DATA (0) is used for raw file data in the multiplexed stream.
#[test]
fn msg_data_semantics() {
    let code = MessageCode::Data;
    assert_eq!(code.as_u8(), 0);
    assert!(!code.is_logging());
    assert!(code.log_code().is_none());
}

/// MSG_REDO (9) requests reprocessing of a file-list index.
#[test]
fn msg_redo_semantics() {
    let code = MessageCode::Redo;
    assert_eq!(code.as_u8(), 9);
    assert!(!code.is_logging());
}

/// MSG_STATS (10) carries transfer statistics.
#[test]
fn msg_stats_semantics() {
    let code = MessageCode::Stats;
    assert_eq!(code.as_u8(), 10);
    assert!(!code.is_logging());
}

/// MSG_IO_ERROR (22) indicates sender encountered an I/O error.
#[test]
fn msg_io_error_semantics() {
    let code = MessageCode::IoError;
    assert_eq!(code.as_u8(), 22);
    assert!(!code.is_logging());
}

/// MSG_IO_TIMEOUT (33) is used by daemons to communicate timeout.
#[test]
fn msg_io_timeout_semantics() {
    let code = MessageCode::IoTimeout;
    assert_eq!(code.as_u8(), 33);
    assert!(!code.is_logging());
}

/// MSG_NOOP (42) is a legacy compatibility message for protocol 30.
#[test]
fn msg_noop_semantics() {
    let code = MessageCode::NoOp;
    assert_eq!(code.as_u8(), 42);
    assert!(!code.is_logging());
}

/// MSG_ERROR_EXIT (86) synchronizes error exit across processes (protocol >= 31).
#[test]
fn msg_error_exit_semantics() {
    let code = MessageCode::ErrorExit;
    assert_eq!(code.as_u8(), 86);
    assert!(!code.is_logging());
}

/// MSG_SUCCESS (100) indicates receiver successfully updated a file.
#[test]
fn msg_success_semantics() {
    let code = MessageCode::Success;
    assert_eq!(code.as_u8(), 100);
    assert!(!code.is_logging());
}

/// MSG_DELETED (101) indicates receiver deleted a file.
#[test]
fn msg_deleted_semantics() {
    let code = MessageCode::Deleted;
    assert_eq!(code.as_u8(), 101);
    assert!(!code.is_logging());
}

/// MSG_NO_SEND (102) indicates sender failed to open a requested file.
#[test]
fn msg_no_send_semantics() {
    let code = MessageCode::NoSend;
    assert_eq!(code.as_u8(), 102);
    assert!(!code.is_logging());
}

// ============================================================================
// Integration with MessageHeader
// ============================================================================

/// Verifies that all message codes can be used in MessageHeader construction.
#[test]
fn all_message_codes_work_with_header() {
    for &code in MessageCode::all() {
        let header = MessageHeader::new(code, 0).expect("header construction should succeed");
        assert_eq!(header.code(), code);

        let header_max =
            MessageHeader::new(code, MAX_PAYLOAD_LENGTH).expect("max payload should work");
        assert_eq!(header_max.code(), code);
    }
}

/// Verifies header encode/decode round-trip preserves message codes.
#[test]
fn header_round_trip_preserves_all_message_codes() {
    for &code in MessageCode::all() {
        let header = MessageHeader::new(code, 12345).expect("header construction");
        let encoded = header.encode();
        let decoded = MessageHeader::decode(&encoded).expect("decode should succeed");

        assert_eq!(decoded.code(), code, "Code mismatch for {:?}", code);
    }
}

// ============================================================================
// Const Context Tests
// ============================================================================

/// Verifies that MessageCode methods work in const contexts.
#[test]
fn message_code_methods_are_const() {
    const DATA_VALUE: u8 = MessageCode::Data.as_u8();
    const INFO_NAME: &str = MessageCode::Info.name();
    const PARSED: Option<MessageCode> = MessageCode::from_u8(2);
    const IS_LOGGING: bool = MessageCode::Info.is_logging();
    const LOG_CODE: Option<LogCode> = MessageCode::Info.log_code();
    const FROM_LOG: Option<MessageCode> = MessageCode::from_log_code(LogCode::Info);

    assert_eq!(DATA_VALUE, 0);
    assert_eq!(INFO_NAME, "MSG_INFO");
    assert_eq!(PARSED, Some(MessageCode::Info));
    assert!(IS_LOGGING);
    assert_eq!(LOG_CODE, Some(LogCode::Info));
    assert_eq!(FROM_LOG, Some(MessageCode::Info));
}

/// Verifies that LogCode methods work in const contexts.
#[test]
fn log_code_methods_are_const() {
    const INFO_VALUE: u8 = LogCode::Info.as_u8();
    const INFO_NAME: &str = LogCode::Info.name();
    const PARSED: Option<LogCode> = LogCode::from_u8(2);

    assert_eq!(INFO_VALUE, 2);
    assert_eq!(INFO_NAME, "FINFO");
    assert_eq!(PARSED, Some(LogCode::Info));
}

// ============================================================================
// Edge Case: Sparse Code Value Range
// ============================================================================

/// Documents and tests the sparse nature of message code values.
/// The rsync protocol uses non-contiguous values for historical reasons.
#[test]
fn message_code_sparse_value_ranges() {
    // Contiguous range: 0-10
    for i in 0u8..=10 {
        assert!(
            MessageCode::from_u8(i).is_some(),
            "Value {i} should be valid"
        );
    }

    // Gap: 11-21
    for i in 11u8..22 {
        assert!(
            MessageCode::from_u8(i).is_none(),
            "Value {i} should be invalid (gap 11-21)"
        );
    }

    // Single value: 22
    assert!(MessageCode::from_u8(22).is_some());

    // Gap: 23-32
    for i in 23u8..33 {
        assert!(
            MessageCode::from_u8(i).is_none(),
            "Value {i} should be invalid (gap 23-32)"
        );
    }

    // Single value: 33
    assert!(MessageCode::from_u8(33).is_some());

    // Gap: 34-41
    for i in 34u8..42 {
        assert!(
            MessageCode::from_u8(i).is_none(),
            "Value {i} should be invalid (gap 34-41)"
        );
    }

    // Single value: 42
    assert!(MessageCode::from_u8(42).is_some());

    // Gap: 43-85
    for i in 43u8..86 {
        assert!(
            MessageCode::from_u8(i).is_none(),
            "Value {i} should be invalid (gap 43-85)"
        );
    }

    // Single value: 86
    assert!(MessageCode::from_u8(86).is_some());

    // Gap: 87-99
    for i in 87u8..100 {
        assert!(
            MessageCode::from_u8(i).is_none(),
            "Value {i} should be invalid (gap 87-99)"
        );
    }

    // Contiguous range: 100-102
    for i in 100u8..=102 {
        assert!(
            MessageCode::from_u8(i).is_some(),
            "Value {i} should be valid"
        );
    }

    // Gap: 103-255
    for i in 103u8..=255 {
        assert!(
            MessageCode::from_u8(i).is_none(),
            "Value {i} should be invalid (gap 103-255)"
        );
    }
}

/// Documents that log codes are contiguous (0-8) unlike message codes.
#[test]
fn log_code_contiguous_value_range() {
    // Log codes are contiguous 0-8
    for i in 0u8..=8 {
        assert!(LogCode::from_u8(i).is_some(), "LogCode {i} should exist");
    }

    // All other values are invalid
    for i in 9u8..=255 {
        assert!(
            LogCode::from_u8(i).is_none(),
            "LogCode {i} should not exist"
        );
    }
}
