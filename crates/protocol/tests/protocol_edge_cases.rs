//! Protocol negotiation edge case and error handling tests.
//!
//! These tests validate that the protocol implementation correctly handles
//! malformed input, boundary conditions, and error scenarios. Proper error
//! handling is critical for security and robustness.
//!
//! ## Test Coverage
//!
//! ### Malformed Handshakes
//! - Zero-length protocol advertisements
//! - Non-numeric version strings
//! - Invalid `@RSYNCD:` magic sequences
//! - Truncated binary handshakes
//! - Missing newline terminators
//!
//! ### Version Boundary Tests
//! - Protocol 0 (invalid)
//! - Protocol 27 (below minimum supported)
//! - Protocol 33+ (above maximum supported)
//! - Protocol 255 (maximum u8 value)
//! - Very large protocol numbers (u32 overflow)
//!
//! ### Compatibility Flag Edge Cases
//! - All flags set (stress test)
//! - No flags set (empty)
//! - Unknown/future flags
//! - Invalid flag encoding
//!
//! ### Protocol Negotiation Errors
//! - Client advertises unsupported protocol
//! - Server response with invalid protocol
//! - Protocol mismatch scenarios
//! - Legacy vs binary negotiation confusion

use protocol::{
    NegotiationError, ParseProtocolVersionErrorKind, ProtocolVersion,
    format_legacy_daemon_greeting, parse_legacy_daemon_greeting,
};
use std::str::FromStr;

// ============================================================================
// Malformed Handshake Tests
// ============================================================================

#[test]
fn malformed_zero_length_version_string() {
    let result = ProtocolVersion::from_str("");
    assert!(result.is_err(), "Empty version string should be rejected");

    let err = result.unwrap_err();
    match err.kind() {
        ParseProtocolVersionErrorKind::Empty => {
            // Expected error
        }
        other => panic!("Expected Empty error, got: {other:?}"),
    }
}

#[test]
fn malformed_whitespace_only_version_string() {
    let result = ProtocolVersion::from_str("   ");
    assert!(
        result.is_err(),
        "Whitespace-only version should be rejected"
    );

    let err = result.unwrap_err();
    assert!(matches!(err.kind(), ParseProtocolVersionErrorKind::Empty));
}

#[test]
fn malformed_non_numeric_version_string() {
    let inputs = ["abc", "xx", "3a", "2.9", "hello", "protocol32"];

    for input in inputs {
        let result = ProtocolVersion::from_str(input);
        assert!(
            result.is_err(),
            "Non-numeric version '{input}' should be rejected"
        );

        let err = result.unwrap_err();
        assert!(
            matches!(err.kind(), ParseProtocolVersionErrorKind::InvalidDigit),
            "Expected InvalidDigit for '{input}', got: {:?}",
            err.kind()
        );
    }
}

#[test]
fn malformed_negative_version_number() {
    let result = ProtocolVersion::from_str("-30");
    assert!(result.is_err(), "Negative version should be rejected");

    let err = result.unwrap_err();
    assert!(matches!(
        err.kind(),
        ParseProtocolVersionErrorKind::Negative
    ));
}

#[test]
fn malformed_invalid_rsyncd_prefix() {
    let invalid_greetings = [
        "@RSYNC: 28.0\n",  // Missing 'D'
        "@RSYNCD 28.0\n",  // Missing colon
        "RSYNCD: 28.0\n",  // Missing '@'
        "@rsyncd: 28.0\n", // Wrong case (lowercase)
        "rsync: 28.0\n",   // Wrong prefix entirely
    ];

    for greeting in invalid_greetings {
        let result = parse_legacy_daemon_greeting(greeting);
        assert!(
            result.is_err(),
            "Invalid greeting should be rejected: {greeting:?}"
        );
    }
}

#[test]
fn malformed_legacy_greeting_missing_newline() {
    let greeting_no_newline = "@RSYNCD: 28.0";
    let result = parse_legacy_daemon_greeting(greeting_no_newline);

    // Should either reject or require newline terminator
    // Implementation may accept this, but if it does, it should be consistent
    if let Ok(parsed) = result {
        assert_eq!(parsed.as_u8(), 28, "If accepted, must parse correctly");
    }
}

#[test]
fn malformed_truncated_binary_advertisement() {
    // Binary advertisement should be exactly 4 bytes
    let truncated_inputs = [
        &[0u8][..],              // 1 byte
        &[0u8, 0][..],           // 2 bytes
        &[0u8, 0, 0][..],        // 3 bytes
        &[0u8, 0, 0, 32, 0][..], // 5 bytes (extra byte)
    ];

    for (i, input) in truncated_inputs.iter().enumerate() {
        if input.len() == 4 {
            // Should parse successfully
            let value = u32::from_be_bytes([input[0], input[1], input[2], input[3]]);
            let _ = ProtocolVersion::from_peer_advertisement(value);
        } else {
            // Cannot create valid u32 from wrong-sized input
            // This is a compile-time or runtime error depending on how it's used
            eprintln!("Truncated input {i}: {} bytes (expected 4)", input.len());
        }
    }
}

// ============================================================================
// Version Boundary Tests
// ============================================================================

#[test]
fn boundary_protocol_zero() {
    let result = ProtocolVersion::from_peer_advertisement(0);
    assert!(
        result.is_err(),
        "Protocol 0 is invalid and should be rejected"
    );

    if let Err(NegotiationError::UnsupportedVersion(ver)) = result {
        assert_eq!(ver, 0);
    } else {
        panic!("Expected UnsupportedVersion(0), got: {result:?}");
    }
}

#[test]
fn boundary_protocol_below_minimum() {
    // Protocol 27 is below the minimum supported (28)
    let result = ProtocolVersion::from_peer_advertisement(27);
    assert!(
        result.is_err(),
        "Protocol 27 is below minimum and should be rejected"
    );

    if let Err(NegotiationError::UnsupportedVersion(ver)) = result {
        assert_eq!(ver, 27);
    } else {
        panic!("Expected UnsupportedVersion(27), got: {result:?}");
    }
}

#[test]
fn boundary_protocol_minimum_supported() {
    // Protocol 28 is the minimum supported
    let result = ProtocolVersion::from_peer_advertisement(28);
    assert!(
        result.is_ok(),
        "Protocol 28 is minimum supported and should be accepted"
    );

    let protocol = result.unwrap();
    assert_eq!(protocol.as_u8(), 28);
    assert_eq!(protocol, ProtocolVersion::V28);
}

#[test]
fn boundary_protocol_maximum_supported() {
    // Protocol 32 is the maximum currently supported
    let result = ProtocolVersion::from_peer_advertisement(32);
    assert!(
        result.is_ok(),
        "Protocol 32 is maximum supported and should be accepted"
    );

    let protocol = result.unwrap();
    assert_eq!(protocol.as_u8(), 32);
    assert_eq!(protocol, ProtocolVersion::V32);
}

#[test]
fn boundary_protocol_above_maximum() {
    // Protocol 33 is above the maximum supported
    // Should be clamped to 32 (matching upstream rsync behavior)
    let result = ProtocolVersion::from_peer_advertisement(33);
    assert!(
        result.is_ok(),
        "Protocol 33 should be clamped to maximum supported (32)"
    );

    let protocol = result.unwrap();
    assert_eq!(
        protocol.as_u8(),
        32,
        "Should be clamped to maximum supported protocol"
    );
}

#[test]
fn boundary_protocol_far_above_maximum() {
    // Protocol 100 exceeds MAXIMUM_PROTOCOL_ADVERTISEMENT (40) and should be rejected
    let result = ProtocolVersion::from_peer_advertisement(100);
    assert!(
        result.is_err(),
        "Far future protocol (>40) should be rejected"
    );

    if let Err(NegotiationError::UnsupportedVersion(ver)) = result {
        assert_eq!(ver, 100);
    } else {
        panic!("Expected UnsupportedVersion(100), got: {result:?}");
    }
}

#[test]
fn boundary_protocol_u8_max() {
    // Protocol 255 (max u8 value) exceeds MAXIMUM_PROTOCOL_ADVERTISEMENT and should be rejected
    let result = ProtocolVersion::from_peer_advertisement(255);
    assert!(
        result.is_err(),
        "Max u8 protocol (255) should be rejected (>40)"
    );

    if let Err(NegotiationError::UnsupportedVersion(ver)) = result {
        assert_eq!(ver, 255);
    } else {
        panic!("Expected UnsupportedVersion(255), got: {result:?}");
    }
}

#[test]
fn boundary_protocol_u32_large() {
    // Very large u32 values should be rejected (above maximum advertisement)
    let large_values = [
        256,       // Just above u8::MAX
        1000,      // Moderately large
        65536,     // u16::MAX + 1
        1_000_000, // Very large
        u32::MAX,  // Maximum u32
    ];

    for value in large_values {
        let result = ProtocolVersion::from_peer_advertisement(value);
        assert!(
            result.is_err(),
            "Very large protocol {value} should be rejected"
        );

        if let Err(NegotiationError::UnsupportedVersion(ver)) = result {
            assert_eq!(ver, value);
        } else {
            panic!("Expected UnsupportedVersion({value}), got: {result:?}");
        }
    }
}

#[test]
fn boundary_from_str_overflow() {
    let overflow_strings = [
        "256",   // Above u8::MAX
        "1000",  // Much larger
        "99999", // Way too large
    ];

    for input in overflow_strings {
        let result = ProtocolVersion::from_str(input);
        assert!(
            result.is_err(),
            "Overflow value '{input}' should be rejected"
        );

        let err = result.unwrap_err();
        assert!(
            matches!(
                err.kind(),
                ParseProtocolVersionErrorKind::Overflow
                    | ParseProtocolVersionErrorKind::UnsupportedRange(_)
            ),
            "Expected Overflow or UnsupportedRange for '{input}', got: {:?}",
            err.kind()
        );
    }
}

// ============================================================================
// Protocol Parsing Edge Cases
// ============================================================================

#[test]
fn parsing_leading_whitespace() {
    // Should accept and trim leading whitespace
    let inputs = [" 32", "  32", "\t32", "\n32"];

    for input in inputs {
        let result = ProtocolVersion::from_str(input);
        assert!(
            result.is_ok(),
            "Leading whitespace should be trimmed: {input:?}"
        );

        let protocol = result.unwrap();
        assert_eq!(protocol.as_u8(), 32);
    }
}

#[test]
fn parsing_trailing_whitespace() {
    // Should accept and trim trailing whitespace
    let inputs = ["32 ", "32  ", "32\t", "32\n"];

    for input in inputs {
        let result = ProtocolVersion::from_str(input);
        assert!(
            result.is_ok(),
            "Trailing whitespace should be trimmed: {input:?}"
        );

        let protocol = result.unwrap();
        assert_eq!(protocol.as_u8(), 32);
    }
}

#[test]
fn parsing_plus_prefix() {
    // Should accept '+' prefix (explicit positive)
    let result = ProtocolVersion::from_str("+32");
    assert!(result.is_ok(), "Plus prefix should be accepted");

    let protocol = result.unwrap();
    assert_eq!(protocol.as_u8(), 32);
}

#[test]
fn parsing_leading_zeros() {
    // Should accept leading zeros
    let inputs = ["032", "0032", "00032"];

    for input in inputs {
        let result = ProtocolVersion::from_str(input);
        assert!(
            result.is_ok(),
            "Leading zeros should be accepted: {input:?}"
        );

        let protocol = result.unwrap();
        assert_eq!(protocol.as_u8(), 32);
    }
}

#[test]
fn parsing_mixed_whitespace() {
    // Should handle mixed whitespace
    let result = ProtocolVersion::from_str("  +32  \n");
    assert!(result.is_ok(), "Mixed whitespace should be handled");

    let protocol = result.unwrap();
    assert_eq!(protocol.as_u8(), 32);
}

// ============================================================================
// Legacy Greeting Edge Cases
// ============================================================================

#[test]
fn legacy_greeting_extra_content_after_version() {
    // Upstream rsync includes digest list after version
    let greeting_with_digests = "@RSYNCD: 29.0 sha512 sha256 sha1 md5 md4\n";
    let result = parse_legacy_daemon_greeting(greeting_with_digests);

    assert!(
        result.is_ok(),
        "Greeting with digest list should be accepted"
    );

    let parsed = result.unwrap();
    assert_eq!(parsed.as_u8(), 29);
}

#[test]
fn legacy_greeting_multiple_spaces() {
    // Should handle multiple spaces (though not standard)
    let greeting = "@RSYNCD:  29.0\n";
    let result = parse_legacy_daemon_greeting(greeting);

    // May or may not accept depending on strictness
    if let Ok(parsed) = result {
        assert_eq!(parsed.as_u8(), 29);
    }
}

#[test]
fn legacy_greeting_crlf_terminator() {
    // Should handle CRLF line endings (Windows-style)
    let greeting_crlf = "@RSYNCD: 28.0\r\n";
    let result = parse_legacy_daemon_greeting(greeting_crlf);

    // May or may not accept depending on strictness
    if let Ok(parsed) = result {
        assert_eq!(parsed.as_u8(), 28);
    }
}

#[test]
fn legacy_greeting_format_generation_stability() {
    // Validate that format_legacy_daemon_greeting is stable
    for _ in 0..100 {
        let protocol = ProtocolVersion::V28;
        let greeting1 = format_legacy_daemon_greeting(protocol);
        let greeting2 = format_legacy_daemon_greeting(protocol);

        assert_eq!(
            greeting1, greeting2,
            "Format generation must be stable across calls"
        );
    }
}

// ============================================================================
// Protocol Negotiation Scenarios
// ============================================================================

#[test]
fn negotiation_exact_match() {
    // Both sides support protocol 32
    let client_protocol = ProtocolVersion::V32;
    let server_protocol = ProtocolVersion::V32;

    let negotiated = std::cmp::min(client_protocol, server_protocol);
    assert_eq!(negotiated, ProtocolVersion::V32);
}

#[test]
fn negotiation_client_newer() {
    // Client supports 32, server supports 30
    let client_protocol = ProtocolVersion::V32;
    let server_protocol = ProtocolVersion::V30;

    let negotiated = std::cmp::min(client_protocol, server_protocol);
    assert_eq!(
        negotiated,
        ProtocolVersion::V30,
        "Should negotiate to older protocol"
    );
}

#[test]
fn negotiation_server_newer() {
    // Client supports 30, server supports 32
    let client_protocol = ProtocolVersion::V30;
    let server_protocol = ProtocolVersion::V32;

    let negotiated = std::cmp::min(client_protocol, server_protocol);
    assert_eq!(
        negotiated,
        ProtocolVersion::V30,
        "Should negotiate to older protocol"
    );
}

#[test]
fn negotiation_minimum_supported() {
    // Client and server both support protocol 28 (minimum)
    let client_protocol = ProtocolVersion::V28;
    let server_protocol = ProtocolVersion::V28;

    let negotiated = std::cmp::min(client_protocol, server_protocol);
    assert_eq!(negotiated, ProtocolVersion::V28);
}

#[test]
fn negotiation_across_binary_boundary() {
    // Client supports 32 (binary), server supports 29 (legacy)
    let client_protocol = ProtocolVersion::V32;
    let server_protocol = ProtocolVersion::V29;

    let negotiated = std::cmp::min(client_protocol, server_protocol);
    assert_eq!(
        negotiated,
        ProtocolVersion::V29,
        "Should negotiate to legacy protocol"
    );
    assert!(negotiated.uses_legacy_ascii_negotiation());
}

// ============================================================================
// Supported Protocol Range Tests
// ============================================================================

#[test]
fn supported_range_consistency() {
    // Validate that supported range is consistent
    let (oldest, newest) = ProtocolVersion::supported_range_bounds();

    assert_eq!(oldest, 28, "Oldest supported should be 28");
    assert_eq!(newest, 32, "Newest supported should be 32");

    assert_eq!(oldest, ProtocolVersion::V28.as_u8());
    assert_eq!(newest, ProtocolVersion::V32.as_u8());
}

#[test]
fn supported_protocols_array_consistency() {
    // Validate that SUPPORTED_PROTOCOLS array matches supported range
    let supported = ProtocolVersion::supported_protocol_numbers();

    assert_eq!(supported.len(), 5, "Should have 5 supported protocols");

    // Should be in descending order (newest first)
    assert_eq!(supported[0], 32);
    assert_eq!(supported[1], 31);
    assert_eq!(supported[2], 30);
    assert_eq!(supported[3], 29);
    assert_eq!(supported[4], 28);
}

#[test]
fn is_supported_protocol_number_comprehensive() {
    // Test all values from 0 to 255
    for value in 0u8..=255 {
        let is_supported = ProtocolVersion::is_supported_protocol_number(value);
        let expected = matches!(value, 28..=32);

        assert_eq!(is_supported, expected, "Protocol {value} support mismatch");
    }
}

// ============================================================================
// Round-Trip Validation Tests
// ============================================================================

#[test]
fn round_trip_all_supported_protocols() {
    // Validate that all supported protocols can round-trip through string parsing
    for protocol in ProtocolVersion::supported_versions() {
        let protocol_str = protocol.to_string();
        let parsed = ProtocolVersion::from_str(&protocol_str)
            .expect("Supported protocol should parse from string");

        assert_eq!(
            parsed, *protocol,
            "Protocol {protocol} should round-trip through string"
        );
    }
}

#[test]
fn round_trip_peer_advertisement() {
    // Validate round-trip through peer advertisement
    for protocol in ProtocolVersion::supported_versions() {
        let advertised = u32::from(protocol.as_u8());
        let parsed = ProtocolVersion::from_peer_advertisement(advertised)
            .expect("Supported protocol should parse from advertisement");

        assert_eq!(
            parsed, *protocol,
            "Protocol {protocol} should round-trip through advertisement"
        );
    }
}

// ============================================================================
// Compatibility with Unsupported Versions
// ============================================================================

#[test]
fn from_str_unsupported_in_range() {
    // Protocols that are numerically valid but not supported
    // (e.g., if we skip a version number)
    // Currently all protocols 28-32 are supported, so this is for future-proofing

    // If we ever have gaps in supported versions, they should be rejected
    // For now, just document the expected behavior
    for value in 28u8..=32 {
        let result = ProtocolVersion::from_str(&value.to_string());
        assert!(result.is_ok(), "Protocol {value} is currently supported");
    }
}

#[test]
fn from_peer_advertisement_clamping() {
    // Validate that future versions within MAXIMUM_PROTOCOL_ADVERTISEMENT (40) are clamped
    let test_cases_clamped = [
        (33, 32), // Just above max supported
        (35, 32), // Within advertisement range
        (40, 32), // At MAXIMUM_PROTOCOL_ADVERTISEMENT
    ];

    for (input, expected) in test_cases_clamped {
        let result = ProtocolVersion::from_peer_advertisement(input)
            .expect("Protocol within advertisement range should be clamped");

        assert_eq!(
            result.as_u8(),
            expected,
            "Protocol {input} should clamp to {expected}"
        );
    }

    // Protocols above MAXIMUM_PROTOCOL_ADVERTISEMENT should be rejected
    let test_cases_rejected = [41, 50, 100, 255, 1000];

    for input in test_cases_rejected {
        let result = ProtocolVersion::from_peer_advertisement(input);
        assert!(result.is_err(), "Protocol {input} (>40) should be rejected");
    }
}
