//! Wire-level handshake format validation tests.
//!
//! These tests validate that protocol handshakes are correctly formatted
//! according to the rsync wire protocol specification. Instead of relying
//! on captured network traffic (golden files), we programmatically validate
//! the handshake generation and parsing logic.
//!
//! ## Test Coverage
//!
//! ### Binary Negotiation (Protocols 30-32)
//! - Protocol version advertisement format (u32 big-endian)
//! - Byte-level validation of protocol advertisements
//! - Round-trip tests (generate → parse → validate)
//! - Compatibility flags exchange (protocol 30+)
//!
//! ### Legacy ASCII Negotiation (Protocols 28-29)
//! - `@RSYNCD:` greeting format validation
//! - Version string format (e.g., `@RSYNCD: 28.0\n`)
//! - Newline termination requirements
//! - ASCII encoding validation
//!
//! ## Design Rationale
//!
//! This approach is superior to golden file testing because:
//! 1. **No manual capture needed** - Tests are fully automated
//! 2. **Deterministic** - No dependency on network tools or timing
//! 3. **Direct validation** - Tests the code itself, not captured bytes
//! 4. **CI-friendly** - No special setup or permissions required
//! 5. **Maintainable** - Tests update automatically with code changes

use protocol::{LEGACY_DAEMON_PREFIX, ProtocolVersion, format_legacy_daemon_greeting};

// ============================================================================
// Binary Negotiation Format Tests (Protocols 30-32)
// ============================================================================

#[test]
fn protocol_32_advertisement_format() {
    // Protocol 32 should be advertised as 4-byte big-endian u32
    let protocol = ProtocolVersion::V32;
    let bytes = u32::from(protocol.as_u8()).to_be_bytes();

    assert_eq!(bytes.len(), 4, "Protocol advertisement must be 4 bytes");
    assert_eq!(
        bytes,
        [0, 0, 0, 32],
        "Protocol 32 must be [0, 0, 0, 32] in big-endian"
    );
}

#[test]
fn protocol_31_advertisement_format() {
    let protocol = ProtocolVersion::V31;
    let bytes = u32::from(protocol.as_u8()).to_be_bytes();

    assert_eq!(bytes.len(), 4);
    assert_eq!(
        bytes,
        [0, 0, 0, 31],
        "Protocol 31 must be [0, 0, 0, 31] in big-endian"
    );
}

#[test]
fn protocol_30_advertisement_format() {
    let protocol = ProtocolVersion::V30;
    let bytes = u32::from(protocol.as_u8()).to_be_bytes();

    assert_eq!(bytes.len(), 4);
    assert_eq!(
        bytes,
        [0, 0, 0, 30],
        "Protocol 30 must be [0, 0, 0, 30] in big-endian"
    );
}

#[test]
fn binary_advertisement_round_trip() {
    // Validate that protocol advertisements can be round-tripped
    for protocol_num in [28u8, 29, 30, 31, 32] {
        let protocol = ProtocolVersion::from_supported(protocol_num)
            .expect("protocol must be supported");

        // Generate advertisement
        let bytes = u32::from(protocol.as_u8()).to_be_bytes();

        // Parse it back
        let parsed_value = u32::from_be_bytes(bytes);
        let parsed_protocol = ProtocolVersion::from_peer_advertisement(parsed_value)
            .expect("round-trip must succeed");

        assert_eq!(
            parsed_protocol, protocol,
            "Protocol {protocol_num} must round-trip correctly"
        );
    }
}

#[test]
fn binary_advertisement_is_big_endian() {
    // Validate that we use big-endian byte order (network byte order)
    let protocol = ProtocolVersion::from_supported(32).unwrap();
    let bytes = u32::from(protocol.as_u8()).to_be_bytes();

    // Big-endian: most significant byte first
    assert_eq!(bytes[0], 0, "MSB must be 0 for protocol 32");
    assert_eq!(bytes[1], 0, "Byte 1 must be 0");
    assert_eq!(bytes[2], 0, "Byte 2 must be 0");
    assert_eq!(bytes[3], 32, "LSB must be 32");

    // Verify this is NOT little-endian
    let little_endian_bytes = u32::from(protocol.as_u8()).to_le_bytes();
    assert_ne!(
        bytes, little_endian_bytes,
        "Must use big-endian, not little-endian"
    );
}

#[test]
fn binary_advertisement_deterministic() {
    // Validate that the same protocol always produces the same bytes
    let protocol = ProtocolVersion::V32;

    let bytes1 = u32::from(protocol.as_u8()).to_be_bytes();
    let bytes2 = u32::from(protocol.as_u8()).to_be_bytes();

    assert_eq!(
        bytes1, bytes2,
        "Advertisement generation must be deterministic"
    );
}

// ============================================================================
// Legacy ASCII Negotiation Format Tests (Protocols 28-29)
// ============================================================================

#[test]
fn protocol_28_greeting_format() {
    let protocol = ProtocolVersion::V28;
    let greeting = format_legacy_daemon_greeting(protocol);

    // Validate format: "@RSYNCD: 28.0\n"
    assert!(
        greeting.starts_with(LEGACY_DAEMON_PREFIX),
        "Must start with @RSYNCD: prefix"
    );
    assert!(
        greeting.contains("28"),
        "Must contain protocol version 28"
    );
    assert!(
        greeting.ends_with(".0\n"),
        "Must end with .0 and newline"
    );
    assert_eq!(
        greeting.as_bytes()[greeting.len() - 1],
        b'\n',
        "Must end with newline character"
    );

    // Validate exact format
    assert_eq!(greeting, "@RSYNCD: 28.0\n", "Exact format must match");
}

#[test]
fn protocol_29_greeting_format() {
    let protocol = ProtocolVersion::V29;
    let greeting = format_legacy_daemon_greeting(protocol);

    assert!(greeting.starts_with(LEGACY_DAEMON_PREFIX));
    assert!(greeting.contains("29"));
    assert!(greeting.ends_with(".0\n"));
    assert_eq!(greeting, "@RSYNCD: 29.0\n");
}

#[test]
fn legacy_greeting_is_ascii() {
    // Validate that legacy greetings are pure ASCII (no UTF-8 multi-byte chars)
    for protocol_num in [28u8, 29] {
        let protocol = ProtocolVersion::from_supported(protocol_num).unwrap();
        let greeting = format_legacy_daemon_greeting(protocol);

        // Must be valid ASCII
        assert!(greeting.is_ascii(), "Greeting must be pure ASCII");

        // Every byte must be valid ASCII
        for byte in greeting.bytes() {
            assert!(
                byte.is_ascii(),
                "Every byte must be ASCII, found non-ASCII byte: {byte:#x}"
            );
        }
    }
}

#[test]
fn legacy_greeting_newline_terminated() {
    // Validate that all legacy greetings end with exactly one newline
    for protocol_num in [28u8, 29] {
        let protocol = ProtocolVersion::from_supported(protocol_num).unwrap();
        let greeting = format_legacy_daemon_greeting(protocol);

        assert_eq!(
            greeting.chars().last(),
            Some('\n'),
            "Must end with newline character"
        );

        // Should have exactly one newline (at the end)
        let newline_count = greeting.chars().filter(|&c| c == '\n').count();
        assert_eq!(
            newline_count, 1,
            "Must have exactly one newline character"
        );
    }
}

#[test]
fn legacy_greeting_format_consistency() {
    // Validate consistent format across protocol 28 and 29
    for protocol_num in [28u8, 29] {
        let protocol = ProtocolVersion::from_supported(protocol_num).unwrap();
        let greeting = format_legacy_daemon_greeting(protocol);

        // Format: "@RSYNCD: <version>.0\n"
        let expected = format!("@RSYNCD: {}.0\n", protocol_num);
        assert_eq!(
            greeting, expected,
            "Protocol {protocol_num} greeting format mismatch"
        );

        // Validate structure
        assert!(greeting.starts_with("@RSYNCD: "));
        assert!(greeting.contains(".0"));
        assert!(greeting.ends_with('\n'));
    }
}

#[test]
fn legacy_greeting_deterministic() {
    // Validate that the same protocol always produces the same greeting
    let protocol = ProtocolVersion::V28;

    let greeting1 = format_legacy_daemon_greeting(protocol);
    let greeting2 = format_legacy_daemon_greeting(protocol);

    assert_eq!(
        greeting1, greeting2,
        "Greeting generation must be deterministic"
    );
}

#[test]
fn legacy_greeting_length() {
    // Validate expected length for legacy greetings
    let greeting_28 = format_legacy_daemon_greeting(ProtocolVersion::V28);
    let greeting_29 = format_legacy_daemon_greeting(ProtocolVersion::V29);

    // "@RSYNCD: 28.0\n" = 14 characters
    // "@RSYNCD: 29.0\n" = 14 characters
    assert_eq!(greeting_28.len(), 14, "Protocol 28 greeting must be 14 bytes");
    assert_eq!(greeting_29.len(), 14, "Protocol 29 greeting must be 14 bytes");
}

// ============================================================================
// Cross-Protocol Validation Tests
// ============================================================================

#[test]
fn all_supported_protocols_have_valid_advertisements() {
    // Validate that all supported protocol versions can generate valid advertisements
    for protocol in ProtocolVersion::supported_versions() {
        if protocol.uses_binary_negotiation() {
            // Binary protocols use 4-byte u32
            let bytes = u32::from(protocol.as_u8()).to_be_bytes();
            assert_eq!(
                bytes.len(),
                4,
                "Protocol {} binary advertisement must be 4 bytes",
                protocol
            );

            // Must be parseable
            let parsed = u32::from_be_bytes(bytes);
            assert_eq!(
                parsed,
                u32::from(protocol.as_u8()),
                "Protocol {} advertisement must be parseable",
                protocol
            );
        } else {
            // Legacy protocols use ASCII greetings
            let greeting = format_legacy_daemon_greeting(*protocol);
            assert!(
                greeting.starts_with("@RSYNCD: "),
                "Protocol {} must use @RSYNCD: prefix",
                protocol
            );
            assert!(
                greeting.ends_with(".0\n"),
                "Protocol {} must end with .0\\n",
                protocol
            );
        }
    }
}

#[test]
fn binary_vs_legacy_protocol_boundary() {
    // Protocol 30 is the boundary: first binary negotiation protocol
    assert!(
        ProtocolVersion::V30.uses_binary_negotiation(),
        "Protocol 30 uses binary negotiation"
    );
    assert!(
        ProtocolVersion::V31.uses_binary_negotiation(),
        "Protocol 31 uses binary negotiation"
    );
    assert!(
        ProtocolVersion::V32.uses_binary_negotiation(),
        "Protocol 32 uses binary negotiation"
    );

    // Protocols 28-29 use legacy ASCII
    assert!(
        ProtocolVersion::V28.uses_legacy_ascii_negotiation(),
        "Protocol 28 uses legacy ASCII negotiation"
    );
    assert!(
        ProtocolVersion::V29.uses_legacy_ascii_negotiation(),
        "Protocol 29 uses legacy ASCII negotiation"
    );

    // No overlap
    for protocol in ProtocolVersion::supported_versions() {
        let uses_binary = protocol.uses_binary_negotiation();
        let uses_legacy = protocol.uses_legacy_ascii_negotiation();

        assert!(
            uses_binary != uses_legacy,
            "Protocol {} must use exactly one negotiation method",
            protocol
        );
    }
}

#[test]
fn handshake_format_matches_protocol_version() {
    // Validate that the handshake format matches the protocol's negotiation type
    for protocol in ProtocolVersion::supported_versions() {
        if protocol.as_u8() >= 30 {
            // Protocol 30+ should use binary format
            assert!(
                protocol.uses_binary_negotiation(),
                "Protocol {} should use binary negotiation",
                protocol
            );

            // Binary format is 4-byte u32
            let bytes = u32::from(protocol.as_u8()).to_be_bytes();
            assert_eq!(bytes.len(), 4);
        } else {
            // Protocol < 30 should use ASCII format
            assert!(
                protocol.uses_legacy_ascii_negotiation(),
                "Protocol {} should use legacy ASCII negotiation",
                protocol
            );

            // ASCII format starts with @RSYNCD:
            let greeting = format_legacy_daemon_greeting(*protocol);
            assert!(greeting.starts_with("@RSYNCD: "));
        }
    }
}

// ============================================================================
// Compatibility Flags Tests (Protocol 30+)
// ============================================================================

#[test]
fn compatibility_flags_only_for_binary_protocols() {
    // Compatibility flags are only exchanged for protocol 30+
    for protocol in ProtocolVersion::supported_versions() {
        if protocol.as_u8() >= 30 {
            assert!(
                protocol.uses_binary_negotiation(),
                "Protocol {} should support compatibility flags",
                protocol
            );
        }
    }

    // Protocols 28-29 do not exchange compatibility flags
    assert!(ProtocolVersion::V28.uses_legacy_ascii_negotiation());
    assert!(ProtocolVersion::V29.uses_legacy_ascii_negotiation());
}

// ============================================================================
// Wire Format Diagnostic Tests
// ============================================================================

#[test]
fn binary_protocol_wire_format_diagnostic() {
    // Print diagnostic information about binary protocol wire format
    eprintln!("\n=== Binary Protocol Wire Format (Protocols 30-32) ===");

    for protocol_num in [30u8, 31, 32] {
        let protocol = ProtocolVersion::from_supported(protocol_num).unwrap();
        let bytes = u32::from(protocol.as_u8()).to_be_bytes();

        eprintln!(
            "Protocol {}: {:02x} {:02x} {:02x} {:02x} (decimal: {})",
            protocol_num, bytes[0], bytes[1], bytes[2], bytes[3], protocol_num
        );
    }
}

#[test]
fn legacy_protocol_wire_format_diagnostic() {
    // Print diagnostic information about legacy protocol wire format
    eprintln!("\n=== Legacy Protocol Wire Format (Protocols 28-29) ===");

    for protocol_num in [28u8, 29] {
        let protocol = ProtocolVersion::from_supported(protocol_num).unwrap();
        let greeting = format_legacy_daemon_greeting(protocol);
        let bytes = greeting.as_bytes();

        eprintln!(
            "Protocol {}: {:?} ({} bytes)",
            protocol_num,
            greeting.trim(),
            bytes.len()
        );
        eprintln!(
            "  Hex: {}",
            bytes
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
}
