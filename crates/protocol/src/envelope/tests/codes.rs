use super::*;
use std::collections::HashSet;

// ============================================================================
// Phase 2.13: Test all 18 message codes parse correctly from u8 values
// ============================================================================

/// Tests that each of the 18 defined message codes parses correctly from its
/// corresponding u8 value using `MessageCode::from_u8`.
///
/// This test explicitly verifies the mapping for every variant to catch any
/// accidental changes to numeric values that would break wire compatibility.
#[test]
fn message_code_from_u8_parses_all_18_codes() {
    // Codes 0-10: contiguous range
    assert_eq!(MessageCode::from_u8(0), Some(MessageCode::Data));
    assert_eq!(MessageCode::from_u8(1), Some(MessageCode::ErrorXfer));
    assert_eq!(MessageCode::from_u8(2), Some(MessageCode::Info));
    assert_eq!(MessageCode::from_u8(3), Some(MessageCode::Error));
    assert_eq!(MessageCode::from_u8(4), Some(MessageCode::Warning));
    assert_eq!(MessageCode::from_u8(5), Some(MessageCode::ErrorSocket));
    assert_eq!(MessageCode::from_u8(6), Some(MessageCode::Log));
    assert_eq!(MessageCode::from_u8(7), Some(MessageCode::Client));
    assert_eq!(MessageCode::from_u8(8), Some(MessageCode::ErrorUtf8));
    assert_eq!(MessageCode::from_u8(9), Some(MessageCode::Redo));
    assert_eq!(MessageCode::from_u8(10), Some(MessageCode::Stats));

    // Sparse codes: non-contiguous values used by rsync protocol
    assert_eq!(MessageCode::from_u8(22), Some(MessageCode::IoError));
    assert_eq!(MessageCode::from_u8(33), Some(MessageCode::IoTimeout));
    assert_eq!(MessageCode::from_u8(42), Some(MessageCode::NoOp));
    assert_eq!(MessageCode::from_u8(86), Some(MessageCode::ErrorExit));

    // High codes: 100-102 range
    assert_eq!(MessageCode::from_u8(100), Some(MessageCode::Success));
    assert_eq!(MessageCode::from_u8(101), Some(MessageCode::Deleted));
    assert_eq!(MessageCode::from_u8(102), Some(MessageCode::NoSend));
}

/// Tests that `TryFrom<u8>` succeeds for all 18 defined message codes.
///
/// Unlike `from_u8` which returns `Option`, `TryFrom` returns a `Result`
/// with an `EnvelopeError` on failure. This test verifies the success path.
#[test]
fn message_code_try_from_u8_succeeds_for_all_18_codes() {
    // Contiguous range 0-10
    assert_eq!(MessageCode::try_from(0_u8).unwrap(), MessageCode::Data);
    assert_eq!(MessageCode::try_from(1_u8).unwrap(), MessageCode::ErrorXfer);
    assert_eq!(MessageCode::try_from(2_u8).unwrap(), MessageCode::Info);
    assert_eq!(MessageCode::try_from(3_u8).unwrap(), MessageCode::Error);
    assert_eq!(MessageCode::try_from(4_u8).unwrap(), MessageCode::Warning);
    assert_eq!(
        MessageCode::try_from(5_u8).unwrap(),
        MessageCode::ErrorSocket
    );
    assert_eq!(MessageCode::try_from(6_u8).unwrap(), MessageCode::Log);
    assert_eq!(MessageCode::try_from(7_u8).unwrap(), MessageCode::Client);
    assert_eq!(MessageCode::try_from(8_u8).unwrap(), MessageCode::ErrorUtf8);
    assert_eq!(MessageCode::try_from(9_u8).unwrap(), MessageCode::Redo);
    assert_eq!(MessageCode::try_from(10_u8).unwrap(), MessageCode::Stats);

    // Sparse codes
    assert_eq!(MessageCode::try_from(22_u8).unwrap(), MessageCode::IoError);
    assert_eq!(
        MessageCode::try_from(33_u8).unwrap(),
        MessageCode::IoTimeout
    );
    assert_eq!(MessageCode::try_from(42_u8).unwrap(), MessageCode::NoOp);
    assert_eq!(
        MessageCode::try_from(86_u8).unwrap(),
        MessageCode::ErrorExit
    );

    // High codes
    assert_eq!(MessageCode::try_from(100_u8).unwrap(), MessageCode::Success);
    assert_eq!(MessageCode::try_from(101_u8).unwrap(), MessageCode::Deleted);
    assert_eq!(MessageCode::try_from(102_u8).unwrap(), MessageCode::NoSend);
}

/// Verifies that `MessageCode::ALL` contains exactly 18 codes and that each
/// code's numeric value matches its expected wire representation.
#[test]
fn message_code_all_array_contains_exactly_18_codes_with_correct_values() {
    assert_eq!(MessageCode::ALL.len(), 18);

    // Map expected (variant, numeric_value) pairs
    let expected: [(MessageCode, u8); 18] = [
        (MessageCode::Data, 0),
        (MessageCode::ErrorXfer, 1),
        (MessageCode::Info, 2),
        (MessageCode::Error, 3),
        (MessageCode::Warning, 4),
        (MessageCode::ErrorSocket, 5),
        (MessageCode::Log, 6),
        (MessageCode::Client, 7),
        (MessageCode::ErrorUtf8, 8),
        (MessageCode::Redo, 9),
        (MessageCode::Stats, 10),
        (MessageCode::IoError, 22),
        (MessageCode::IoTimeout, 33),
        (MessageCode::NoOp, 42),
        (MessageCode::ErrorExit, 86),
        (MessageCode::Success, 100),
        (MessageCode::Deleted, 101),
        (MessageCode::NoSend, 102),
    ];

    for (code, value) in expected {
        assert!(
            MessageCode::ALL.contains(&code),
            "MessageCode::ALL missing {code:?}"
        );
        assert_eq!(
            code.as_u8(),
            value,
            "MessageCode::{code:?} has wrong numeric value"
        );
    }
}

// ============================================================================
// Phase 2.14: Test unknown code rejection with appropriate errors
// ============================================================================

/// Tests that `MessageCode::from_u8` returns `None` for values in gaps between
/// defined codes.
///
/// The rsync protocol uses sparse code values with gaps. This test ensures
/// undefined values in those gaps are properly rejected.
#[test]
fn message_code_from_u8_rejects_gap_values() {
    // Gap between Stats (10) and IoError (22)
    for value in 11..22 {
        assert_eq!(
            MessageCode::from_u8(value),
            None,
            "from_u8({value}) should return None"
        );
    }

    // Gap between IoError (22) and IoTimeout (33)
    for value in 23..33 {
        assert_eq!(
            MessageCode::from_u8(value),
            None,
            "from_u8({value}) should return None"
        );
    }

    // Gap between IoTimeout (33) and NoOp (42)
    for value in 34..42 {
        assert_eq!(
            MessageCode::from_u8(value),
            None,
            "from_u8({value}) should return None"
        );
    }

    // Gap between NoOp (42) and ErrorExit (86)
    for value in 43..86 {
        assert_eq!(
            MessageCode::from_u8(value),
            None,
            "from_u8({value}) should return None"
        );
    }

    // Gap between ErrorExit (86) and Success (100)
    for value in 87..100 {
        assert_eq!(
            MessageCode::from_u8(value),
            None,
            "from_u8({value}) should return None"
        );
    }

    // Values above NoSend (102)
    for value in 103..=255 {
        assert_eq!(
            MessageCode::from_u8(value),
            None,
            "from_u8({value}) should return None"
        );
    }
}

/// Tests that `TryFrom<u8>` returns `EnvelopeError::UnknownMessageCode` for
/// undefined values.
///
/// This test verifies the error type and that the error preserves the invalid
/// value for diagnostic purposes.
#[test]
fn message_code_try_from_u8_returns_unknown_message_code_error() {
    // Test representative values from different gap ranges
    let invalid_values: &[u8] = &[
        11,  // First gap (11-21)
        21,  // Last value in first gap
        23,  // Second gap (23-32)
        34,  // Third gap (34-41)
        43,  // Fourth gap (43-85)
        85,  // Last value in fourth gap
        87,  // Fifth gap (87-99)
        99,  // Last value in fifth gap
        103, // First value above defined range
        127, // Mid-range undefined
        200, // High undefined
        254, // Near max
        255, // Max u8 value
    ];

    for &value in invalid_values {
        let result = MessageCode::try_from(value);
        assert!(
            result.is_err(),
            "TryFrom<u8> for {value} should fail but succeeded"
        );
        assert_eq!(
            result.unwrap_err(),
            EnvelopeError::UnknownMessageCode(value),
            "Wrong error variant for value {value}"
        );
    }
}

/// Tests that the error message for unknown codes includes the invalid value.
#[test]
fn message_code_unknown_error_displays_invalid_value() {
    let err = MessageCode::try_from(42_u8 + 1).unwrap_err();
    let display = err.to_string();
    assert!(
        display.contains("43"),
        "Error display should contain invalid value: {display}"
    );
    assert!(
        display.contains("unknown"),
        "Error display should indicate unknown code: {display}"
    );
}

// ============================================================================
// Roundtrip conversion tests between MessageCode and u8
// ============================================================================

/// Tests bidirectional conversion: MessageCode -> u8 -> MessageCode for all codes.
#[test]
fn message_code_roundtrip_via_as_u8_and_from_u8() {
    for &code in MessageCode::all() {
        let wire_value = code.as_u8();
        let recovered = MessageCode::from_u8(wire_value);
        assert_eq!(
            recovered,
            Some(code),
            "Roundtrip failed for {code:?} (wire value {wire_value})"
        );
    }
}

/// Tests bidirectional conversion using `Into<u8>` and `TryFrom<u8>`.
#[test]
fn message_code_roundtrip_via_into_and_try_from() {
    for &code in MessageCode::all() {
        let wire_value: u8 = code.into();
        let recovered = MessageCode::try_from(wire_value);
        assert_eq!(
            recovered,
            Ok(code),
            "Roundtrip failed for {code:?} (wire value {wire_value})"
        );
    }
}

/// Tests that `as_u8()` and `Into<u8>` produce identical results.
#[test]
fn message_code_as_u8_equals_into_u8() {
    for &code in MessageCode::all() {
        let via_method = code.as_u8();
        let via_into: u8 = code.into();
        assert_eq!(
            via_method, via_into,
            "as_u8() and Into<u8> differ for {code:?}"
        );
    }
}

/// Tests that `from_u8()` and `TryFrom<u8>` agree on success cases.
#[test]
fn message_code_from_u8_agrees_with_try_from_on_success() {
    for &code in MessageCode::all() {
        let value = code.as_u8();
        let via_from = MessageCode::from_u8(value);
        let via_try = MessageCode::try_from(value).ok();
        assert_eq!(
            via_from, via_try,
            "from_u8() and TryFrom<u8> differ for value {value}"
        );
    }
}

/// Tests that `from_u8()` and `TryFrom<u8>` agree on failure cases.
#[test]
fn message_code_from_u8_agrees_with_try_from_on_failure() {
    // Sample of invalid values
    for value in [11_u8, 50, 99, 150, 255] {
        let via_from = MessageCode::from_u8(value);
        let via_try = MessageCode::try_from(value).ok();
        assert_eq!(via_from, None, "from_u8({value}) should return None");
        assert_eq!(via_try, None, "TryFrom<u8>({value}) should return None");
    }
}

// ============================================================================
// Display and Debug trait tests
// ============================================================================

/// Tests that `Display` outputs the upstream MSG_* identifier for all codes.
#[test]
fn message_code_display_outputs_msg_identifier() {
    let expected: [(MessageCode, &str); 18] = [
        (MessageCode::Data, "MSG_DATA"),
        (MessageCode::ErrorXfer, "MSG_ERROR_XFER"),
        (MessageCode::Info, "MSG_INFO"),
        (MessageCode::Error, "MSG_ERROR"),
        (MessageCode::Warning, "MSG_WARNING"),
        (MessageCode::ErrorSocket, "MSG_ERROR_SOCKET"),
        (MessageCode::Log, "MSG_LOG"),
        (MessageCode::Client, "MSG_CLIENT"),
        (MessageCode::ErrorUtf8, "MSG_ERROR_UTF8"),
        (MessageCode::Redo, "MSG_REDO"),
        (MessageCode::Stats, "MSG_STATS"),
        (MessageCode::IoError, "MSG_IO_ERROR"),
        (MessageCode::IoTimeout, "MSG_IO_TIMEOUT"),
        (MessageCode::NoOp, "MSG_NOOP"),
        (MessageCode::ErrorExit, "MSG_ERROR_EXIT"),
        (MessageCode::Success, "MSG_SUCCESS"),
        (MessageCode::Deleted, "MSG_DELETED"),
        (MessageCode::NoSend, "MSG_NO_SEND"),
    ];

    for (code, expected_display) in expected {
        assert_eq!(
            format!("{code}"),
            expected_display,
            "Display mismatch for {code:?}"
        );
    }
}

/// Tests that `Display` and `name()` produce identical output.
#[test]
fn message_code_display_equals_name() {
    for &code in MessageCode::all() {
        assert_eq!(
            format!("{code}"),
            code.name(),
            "Display and name() differ for {code:?}"
        );
    }
}

/// Tests that `Debug` output includes the variant name.
#[test]
fn message_code_debug_includes_variant_name() {
    let expected: [(MessageCode, &str); 18] = [
        (MessageCode::Data, "Data"),
        (MessageCode::ErrorXfer, "ErrorXfer"),
        (MessageCode::Info, "Info"),
        (MessageCode::Error, "Error"),
        (MessageCode::Warning, "Warning"),
        (MessageCode::ErrorSocket, "ErrorSocket"),
        (MessageCode::Log, "Log"),
        (MessageCode::Client, "Client"),
        (MessageCode::ErrorUtf8, "ErrorUtf8"),
        (MessageCode::Redo, "Redo"),
        (MessageCode::Stats, "Stats"),
        (MessageCode::IoError, "IoError"),
        (MessageCode::IoTimeout, "IoTimeout"),
        (MessageCode::NoOp, "NoOp"),
        (MessageCode::ErrorExit, "ErrorExit"),
        (MessageCode::Success, "Success"),
        (MessageCode::Deleted, "Deleted"),
        (MessageCode::NoSend, "NoSend"),
    ];

    for (code, variant_name) in expected {
        let debug_output = format!("{code:?}");
        assert!(
            debug_output.contains(variant_name),
            "Debug output '{debug_output}' should contain '{variant_name}'"
        );
    }
}

/// Tests that Debug and Display produce different output (Debug shows Rust
/// variant, Display shows wire name).
#[test]
fn message_code_debug_differs_from_display() {
    for &code in MessageCode::all() {
        let debug_output = format!("{code:?}");
        let display_output = format!("{code}");
        // Debug shows "Data", Display shows "MSG_DATA"
        assert_ne!(
            debug_output, display_output,
            "Debug and Display should differ for {code:?}"
        );
    }
}

// ============================================================================
// Additional edge case and property tests
// ============================================================================

/// Tests that no two message codes share the same numeric value.
#[test]
fn message_code_values_are_unique() {
    let mut seen = HashSet::new();
    for &code in MessageCode::all() {
        let value = code.as_u8();
        assert!(
            seen.insert(value),
            "Duplicate value {value} for code {code:?}"
        );
    }
    assert_eq!(seen.len(), 18);
}

/// Tests that the FLUSH alias constant has the expected properties.
#[test]
fn message_code_flush_alias_properties() {
    // FLUSH is an alias for Info
    assert_eq!(MessageCode::FLUSH, MessageCode::Info);
    assert_eq!(MessageCode::FLUSH.as_u8(), 2);
    assert_eq!(MessageCode::FLUSH.name(), "MSG_INFO");

    // MSG_FLUSH parses to Info
    let parsed: MessageCode = "MSG_FLUSH".parse().expect("MSG_FLUSH should parse");
    assert_eq!(parsed, MessageCode::FLUSH);
    assert_eq!(parsed, MessageCode::Info);
}

#[test]
fn log_codes_are_hashable() {
    let mut set = HashSet::new();
    assert!(set.insert(LogCode::Info));
    assert!(set.contains(&LogCode::Info));
    assert!(!set.insert(LogCode::Info));
}

#[test]
fn message_codes_are_hashable() {
    let mut set = HashSet::new();
    assert!(set.insert(MessageCode::Data));
    assert!(set.contains(&MessageCode::Data));
    assert!(!set.insert(MessageCode::Data));
}

#[test]
fn message_code_variants_round_trip_through_try_from() {
    for &code in MessageCode::all() {
        let raw = code.as_u8();
        let decoded = MessageCode::try_from(raw).expect("known code");
        assert_eq!(decoded, code);
    }
}

#[test]
fn message_code_into_u8_matches_as_u8() {
    for &code in MessageCode::all() {
        let converted: u8 = code.into();
        assert_eq!(converted, code.as_u8());
    }
}

#[test]
fn message_code_from_u8_matches_try_from() {
    for &code in MessageCode::all() {
        let raw = code.as_u8();
        assert_eq!(MessageCode::from_u8(raw), Some(code));
        assert_eq!(MessageCode::try_from(raw).ok(), MessageCode::from_u8(raw));
    }
}

#[test]
fn message_code_from_u8_rejects_unknown_values() {
    assert_eq!(MessageCode::from_u8(11), None);
    assert_eq!(MessageCode::from_u8(0xFF), None);
}

#[test]
fn message_code_from_str_parses_known_names() {
    for &code in MessageCode::all() {
        let parsed: MessageCode = code.name().parse().expect("known name");
        assert_eq!(parsed, code);
    }
}

#[test]
fn message_code_from_str_rejects_unknown_names() {
    let err = "MSG_SOMETHING_ELSE".parse::<MessageCode>().unwrap_err();
    assert_eq!(err.invalid_name(), "MSG_SOMETHING_ELSE");
    assert_eq!(
        err.to_string(),
        "unknown multiplexed message code name: \"MSG_SOMETHING_ELSE\""
    );
}

#[test]
fn message_code_all_is_sorted_by_numeric_value() {
    let all = MessageCode::all();
    for window in all.windows(2) {
        let first = window[0];
        let second = window[1];
        assert!(
            first.as_u8() <= second.as_u8(),
            "MessageCode::all() is not sorted: {all:?}"
        );
    }
}

#[test]
fn logging_classification_matches_upstream_set() {
    const LOGGING_CODES: &[MessageCode] = &[
        MessageCode::ErrorXfer,
        MessageCode::Info,
        MessageCode::Error,
        MessageCode::Warning,
        MessageCode::ErrorSocket,
        MessageCode::ErrorUtf8,
        MessageCode::Log,
        MessageCode::Client,
    ];

    for &code in MessageCode::all() {
        let expected = LOGGING_CODES.contains(&code);
        assert_eq!(code.is_logging(), expected, "mismatch for code {code:?}");
    }
}

#[test]
fn message_code_name_matches_upstream_identifiers() {
    use super::MessageCode::*;

    let expected = [
        (Data, "MSG_DATA"),
        (ErrorXfer, "MSG_ERROR_XFER"),
        (Info, "MSG_INFO"),
        (Error, "MSG_ERROR"),
        (Warning, "MSG_WARNING"),
        (ErrorSocket, "MSG_ERROR_SOCKET"),
        (Log, "MSG_LOG"),
        (Client, "MSG_CLIENT"),
        (ErrorUtf8, "MSG_ERROR_UTF8"),
        (Redo, "MSG_REDO"),
        (Stats, "MSG_STATS"),
        (IoError, "MSG_IO_ERROR"),
        (IoTimeout, "MSG_IO_TIMEOUT"),
        (NoOp, "MSG_NOOP"),
        (ErrorExit, "MSG_ERROR_EXIT"),
        (Success, "MSG_SUCCESS"),
        (Deleted, "MSG_DELETED"),
        (NoSend, "MSG_NO_SEND"),
    ];

    for &(code, name) in &expected {
        assert_eq!(code.name(), name);
        assert_eq!(code.to_string(), name);
    }
}

#[test]
fn message_code_flush_alias_matches_info() {
    assert_eq!(MessageCode::FLUSH, MessageCode::Info);
    assert_eq!(MessageCode::FLUSH.as_u8(), MessageCode::Info.as_u8());

    let parsed: MessageCode = "MSG_FLUSH".parse().expect("known alias");
    assert_eq!(parsed, MessageCode::Info);
}

#[test]
fn log_code_all_is_sorted_by_numeric_value() {
    let all = LogCode::all();
    for window in all.windows(2) {
        let first = window[0];
        let second = window[1];
        assert!(
            first.as_u8() <= second.as_u8(),
            "LogCode::all() unsorted: {all:?}"
        );
    }
}

#[test]
fn log_code_from_u8_matches_try_from() {
    for &code in LogCode::all() {
        let raw = code.as_u8();
        assert_eq!(LogCode::from_u8(raw), Some(code));
        assert_eq!(LogCode::try_from(raw).ok(), LogCode::from_u8(raw));
    }
}

#[test]
fn log_code_from_u8_rejects_unknown_values() {
    assert_eq!(LogCode::from_u8(9), None);
    let err = LogCode::try_from(9).unwrap_err();
    assert_eq!(err.invalid_value(), Some(9));
    assert_eq!(err.to_string(), "unknown log code value: 9");
}

#[test]
fn log_code_from_str_parses_known_names() {
    for &code in LogCode::all() {
        let parsed: LogCode = code.name().parse().expect("known log code name");
        assert_eq!(parsed, code);
    }
}

#[test]
fn log_code_from_str_rejects_unknown_names() {
    let err = "FUNKNOWN".parse::<LogCode>().unwrap_err();
    assert_eq!(err.invalid_name(), Some("FUNKNOWN"));
    assert_eq!(err.to_string(), "unknown log code name: \"FUNKNOWN\"");
    assert_eq!(err.invalid_value(), None);
}

#[test]
fn log_code_name_matches_upstream_identifiers() {
    use super::LogCode::*;

    let expected = [
        (None, "FNONE"),
        (ErrorXfer, "FERROR_XFER"),
        (Info, "FINFO"),
        (Error, "FERROR"),
        (Warning, "FWARNING"),
        (ErrorSocket, "FERROR_SOCKET"),
        (Log, "FLOG"),
        (Client, "FCLIENT"),
        (ErrorUtf8, "FERROR_UTF8"),
    ];

    for &(code, name) in &expected {
        assert_eq!(code.name(), name);
        assert_eq!(code.to_string(), name);
    }
}
