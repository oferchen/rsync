//! Fuzz-like tests for protocol wire format parsing.
//!
//! These tests feed arbitrary/random bytes to various protocol parsers to verify:
//! - Parsers never panic on malformed input
//! - Parsers return appropriate errors for invalid data
//! - Parsers handle edge cases gracefully
//!
//! Since full cargo-fuzz setup can be complex, these tests use random byte generation
//! within standard test infrastructure.

use protocol::codec::{
    LegacyNdxCodec, ModernNdxCodec, NdxCodec, NdxState, ProtocolCodec, create_ndx_codec,
    create_protocol_codec,
};
use protocol::wire::{read_delta_op, read_token};
use protocol::{MessageHeader, decode_varint, read_int, read_varint, read_varlong};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Cursor;

// ============================================================================
// Test Utilities: Pseudo-random byte generation
// ============================================================================

/// Generates a deterministic sequence of pseudo-random bytes for reproducible testing.
fn generate_random_bytes(seed: u64, count: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(count);
    for i in 0..count {
        let mut hasher = DefaultHasher::new();
        (seed, i).hash(&mut hasher);
        bytes.push((hasher.finish() & 0xFF) as u8);
    }
    bytes
}

/// Generates a set of edge-case byte patterns for comprehensive testing.
fn edge_case_byte_patterns() -> Vec<Vec<u8>> {
    vec![
        // Empty
        vec![],
        // Single bytes
        vec![0x00],
        vec![0x01],
        vec![0x7F],
        vec![0x80],
        vec![0xFE],
        vec![0xFF],
        // Two bytes
        vec![0x00, 0x00],
        vec![0xFF, 0xFF],
        vec![0x80, 0x00],
        vec![0xFE, 0x00],
        vec![0xFE, 0x80],
        vec![0xFF, 0x00],
        vec![0xFF, 0xFE],
        vec![0xFF, 0xFF],
        // Three bytes
        vec![0x00, 0x00, 0x00],
        vec![0xFF, 0xFF, 0xFF],
        vec![0xFE, 0x00, 0x00],
        vec![0xFE, 0x80, 0x00],
        vec![0xFE, 0xFF, 0xFF],
        vec![0xFF, 0xFE, 0x00],
        // Four bytes (typical i32)
        vec![0x00, 0x00, 0x00, 0x00],
        vec![0xFF, 0xFF, 0xFF, 0xFF],
        vec![0x01, 0x00, 0x00, 0x00],
        vec![0xFF, 0xFF, 0xFF, 0x7F],
        vec![0x00, 0x00, 0x00, 0x80],
        // Five bytes (varint max)
        vec![0xF0, 0x00, 0x00, 0x00, 0x00],
        vec![0xF0, 0xFF, 0xFF, 0xFF, 0xFF],
        // Longer sequences
        vec![0x00; 8],
        vec![0xFF; 8],
        vec![0xFE; 8],
        vec![0x80; 8],
        // Protocol-specific patterns
        vec![0xFE, 0x00, 0x00, 0x00, 0x00], // Modern NDX extended
        vec![0xFE, 0x80, 0x00, 0x00, 0x00], // Modern NDX 4-byte mode
        vec![0xFF, 0x01],                   // Modern NDX negative prefix
        vec![0xFF, 0xFE, 0x00, 0x00],       // Modern NDX negative extended
    ]
}

// ============================================================================
// Module: Varint Decoder Fuzz Tests
// ============================================================================

mod varint_fuzz {
    use super::*;

    /// Verify read_varint never panics on arbitrary bytes.
    #[test]
    fn read_varint_no_panic_random_bytes() {
        for seed in 0..100 {
            for len in 0..=16 {
                let bytes = generate_random_bytes(seed, len);
                let mut cursor = Cursor::new(&bytes[..]);
                // Should not panic - result is either Ok or Err
                let _ = read_varint(&mut cursor);
            }
        }
    }

    /// Verify read_varint handles all edge case patterns.
    #[test]
    fn read_varint_edge_case_patterns() {
        for pattern in edge_case_byte_patterns() {
            let mut cursor = Cursor::new(&pattern[..]);
            let result = read_varint(&mut cursor);
            // Should not panic, just return Ok or Err
            match result {
                Ok(value) => {
                    // Valid decode - value should be reasonable
                    assert!((i32::MIN..=i32::MAX).contains(&value));
                }
                Err(e) => {
                    // Expected errors for truncated/invalid input
                    assert!(
                        e.kind() == std::io::ErrorKind::UnexpectedEof
                            || e.kind() == std::io::ErrorKind::InvalidData
                    );
                }
            }
        }
    }

    /// Verify decode_varint handles empty input correctly.
    #[test]
    fn decode_varint_empty_input() {
        let result = decode_varint(&[]);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }

    /// Verify varint decoder handles high-bit patterns that indicate overflow.
    #[test]
    fn read_varint_overflow_patterns() {
        // Patterns that should trigger overflow detection
        let overflow_patterns = [
            vec![0xFC, 0x00, 0x00, 0x00, 0x00, 0x00], // extra=5
            vec![0xFE, 0x00, 0x00, 0x00, 0x00, 0x00], // extra=5
            vec![0xFF, 0x00, 0x00, 0x00, 0x00, 0x00], // extra=6
        ];

        for pattern in overflow_patterns {
            let mut cursor = Cursor::new(&pattern[..]);
            let result = read_varint(&mut cursor);
            // Should either succeed or return InvalidData for overflow
            // Should NOT panic
            match result {
                Ok(_) | Err(_) => {} // Both are acceptable, no panic
            }
        }
    }

    /// Stress test with many random sequences.
    #[test]
    fn read_varint_stress_test() {
        for seed in 0..500 {
            let len = (seed % 20) as usize;
            let bytes = generate_random_bytes(seed, len);
            let mut cursor = Cursor::new(&bytes[..]);

            // Try to read multiple varints from the same buffer
            for _ in 0..5 {
                let _ = read_varint(&mut cursor);
            }
        }
    }
}

// ============================================================================
// Module: Varlong Decoder Fuzz Tests
// ============================================================================

mod varlong_fuzz {
    use super::*;

    /// Verify read_varlong never panics on arbitrary bytes.
    #[test]
    fn read_varlong_no_panic_random_bytes() {
        for seed in 0..100 {
            for len in 0..=16 {
                for min_bytes in [1, 2, 3, 4, 5, 6, 7, 8] {
                    let bytes = generate_random_bytes(seed, len);
                    let mut cursor = Cursor::new(&bytes[..]);
                    // Should not panic
                    let _ = read_varlong(&mut cursor, min_bytes);
                }
            }
        }
    }

    /// Verify read_varlong with various min_bytes values.
    #[test]
    fn read_varlong_min_bytes_variations() {
        for pattern in edge_case_byte_patterns() {
            for min_bytes in [1u8, 2, 3, 4, 5, 6, 7, 8] {
                let mut cursor = Cursor::new(&pattern[..]);
                let result = read_varlong(&mut cursor, min_bytes);
                // Should not panic
                match result {
                    Ok(_) | Err(_) => {}
                }
            }
        }
    }

    /// Stress test varlong with random inputs.
    #[test]
    fn read_varlong_stress_test() {
        for seed in 0..200 {
            let len = (seed % 20) as usize + 1;
            let bytes = generate_random_bytes(seed, len);

            for min_bytes in [3u8, 4] {
                let mut cursor = Cursor::new(&bytes[..]);
                // Should not panic
                let _ = read_varlong(&mut cursor, min_bytes);
            }
        }
    }
}

// ============================================================================
// Module: NDX Decoder Fuzz Tests
// ============================================================================

mod ndx_fuzz {
    use super::*;

    /// Verify legacy NDX codec never panics on arbitrary 4-byte inputs.
    #[test]
    fn legacy_ndx_no_panic_random_bytes() {
        let mut codec = LegacyNdxCodec::new(29);

        for seed in 0..100 {
            for len in 0..=8 {
                let bytes = generate_random_bytes(seed, len);
                let mut cursor = Cursor::new(&bytes[..]);
                // Should not panic
                let _ = codec.read_ndx(&mut cursor);
            }
        }
    }

    /// Verify modern NDX codec never panics on arbitrary bytes.
    #[test]
    fn modern_ndx_no_panic_random_bytes() {
        for seed in 0..100 {
            // Create fresh codec for each test since it has state
            let mut codec = ModernNdxCodec::new(30);

            for len in 0..=8 {
                let bytes = generate_random_bytes(seed * 100 + len as u64, len);
                let mut cursor = Cursor::new(&bytes[..]);
                // Should not panic
                let _ = codec.read_ndx(&mut cursor);
            }
        }
    }

    /// Verify NDX codec handles edge case byte patterns.
    #[test]
    fn ndx_codec_edge_case_patterns() {
        for pattern in edge_case_byte_patterns() {
            // Test legacy codec
            let mut legacy = LegacyNdxCodec::new(29);
            let mut cursor = Cursor::new(&pattern[..]);
            let _ = legacy.read_ndx(&mut cursor);

            // Test modern codec
            let mut modern = ModernNdxCodec::new(30);
            let mut cursor = Cursor::new(&pattern[..]);
            let _ = modern.read_ndx(&mut cursor);
        }
    }

    /// Verify NdxState handles arbitrary bytes without panic.
    #[test]
    fn ndx_state_no_panic_random_bytes() {
        for seed in 0..100 {
            let mut state = NdxState::default();

            for len in 0..=8 {
                let bytes = generate_random_bytes(seed * 100 + len as u64, len);
                let mut cursor = Cursor::new(&bytes[..]);
                // Should not panic
                let _ = state.read_ndx(&mut cursor);
            }
        }
    }

    /// Test NDX codec with specific protocol-edge byte sequences.
    #[test]
    fn ndx_protocol_edge_bytes() {
        let edge_sequences = [
            vec![0x00],                         // NDX_DONE in modern
            vec![0xFF],                         // Negative prefix (truncated)
            vec![0xFF, 0x00],                   // Negative with zero
            vec![0xFF, 0x01],                   // Negative with delta 1
            vec![0xFF, 0xFE],                   // Negative with extended marker
            vec![0xFF, 0xFE, 0x00],             // Negative extended (truncated)
            vec![0xFF, 0xFE, 0x00, 0x00],       // Negative extended 2-byte
            vec![0xFF, 0xFE, 0x80, 0x00, 0x00], // Negative extended 4-byte (truncated)
            vec![0xFE],                         // Extended marker (truncated)
            vec![0xFE, 0x00],                   // Extended with one byte
            vec![0xFE, 0x00, 0x00],             // Extended 2-byte diff
            vec![0xFE, 0x80],                   // Extended 4-byte mode (truncated)
            vec![0xFE, 0x80, 0x00, 0x00, 0x00], // Extended 4-byte mode
        ];

        for seq in &edge_sequences {
            let mut modern = ModernNdxCodec::new(30);
            let mut cursor = Cursor::new(&seq[..]);
            // Should not panic, just return result
            let _ = modern.read_ndx(&mut cursor);
        }
    }

    /// Verify all protocol versions handle random bytes without panic.
    #[test]
    fn ndx_all_versions_no_panic() {
        for version in [28u8, 29, 30, 31, 32] {
            let mut codec = create_ndx_codec(version);

            for seed in 0..50 {
                let bytes = generate_random_bytes(seed, 8);
                let mut cursor = Cursor::new(&bytes[..]);
                // Should not panic
                let _ = codec.read_ndx(&mut cursor);
            }
        }
    }

    /// Test that truncated modern NDX reads return errors, not panics.
    #[test]
    fn modern_ndx_truncated_returns_error() {
        let truncated_cases = [
            (vec![0xFE], "extended marker only"),
            (vec![0xFE, 0x00], "extended 2-byte incomplete"),
            (vec![0xFE, 0x80], "extended 4-byte mode incomplete"),
            (vec![0xFE, 0x80, 0x00], "extended 4-byte missing 2"),
            (vec![0xFE, 0x80, 0x00, 0x00], "extended 4-byte missing 1"),
            (vec![0xFF], "negative prefix only"),
            (vec![0xFF, 0xFE], "negative extended marker"),
            (vec![0xFF, 0xFE, 0x00], "negative extended incomplete"),
        ];

        for (bytes, desc) in truncated_cases {
            let mut codec = ModernNdxCodec::new(30);
            let mut cursor = Cursor::new(&bytes[..]);
            let result = codec.read_ndx(&mut cursor);
            assert!(
                result.is_err(),
                "Expected error for truncated input: {desc}"
            );
        }
    }

    /// Test that legacy NDX reads with < 4 bytes return errors.
    #[test]
    fn legacy_ndx_truncated_returns_error() {
        let truncated_cases = [vec![], vec![0x00], vec![0x00, 0x00], vec![0x00, 0x00, 0x00]];

        for bytes in truncated_cases {
            let mut codec = LegacyNdxCodec::new(29);
            let mut cursor = Cursor::new(&bytes[..]);
            let result = codec.read_ndx(&mut cursor);
            assert!(result.is_err(), "Expected error for {} bytes", bytes.len());
            assert_eq!(
                result.unwrap_err().kind(),
                std::io::ErrorKind::UnexpectedEof
            );
        }
    }
}

// ============================================================================
// Module: Message Frame Parser Fuzz Tests
// ============================================================================

mod message_frame_fuzz {
    use super::*;

    /// Verify MessageHeader::decode never panics on arbitrary bytes.
    #[test]
    fn message_header_decode_no_panic() {
        for seed in 0..100 {
            for len in 0..=8 {
                let bytes = generate_random_bytes(seed, len);
                // Should not panic
                let _ = MessageHeader::decode(&bytes);
            }
        }
    }

    /// Verify MessageHeader handles edge case patterns.
    #[test]
    fn message_header_edge_patterns() {
        for pattern in edge_case_byte_patterns() {
            // Should not panic
            let result = MessageHeader::decode(&pattern);

            if pattern.len() < 4 {
                // Truncated input should error
                assert!(result.is_err());
            }
            // Otherwise it may succeed or fail based on tag validity
        }
    }

    /// Verify MessageHeader::from_raw never panics on any u32.
    #[test]
    fn message_header_from_raw_no_panic() {
        // Test specific edge values
        let edge_values: Vec<u32> = vec![
            0,
            1,
            0x7F,
            0x80,
            0xFF,
            0x100,
            0xFFFF,
            0x10000,
            0xFFFFFF,
            0x1000000,
            0x07000000, // Tag = 7 (MPLEX_BASE)
            0x06000000, // Tag < MPLEX_BASE (invalid)
            0x0F000000, // Tag = 15 (may be invalid code)
            0xFF000000, // Tag = 255
            0xFFFFFFFF,
            u32::MAX,
            u32::MAX / 2,
        ];

        for raw in edge_values {
            // Should not panic
            let _ = MessageHeader::from_raw(raw);
        }

        // Test random values
        for seed in 0..1000 {
            let mut hasher = DefaultHasher::new();
            seed.hash(&mut hasher);
            let raw = hasher.finish() as u32;
            // Should not panic
            let _ = MessageHeader::from_raw(raw);
        }
    }

    /// Verify invalid tags return errors, not panics.
    #[test]
    fn message_header_invalid_tag_returns_error() {
        // Tags below MPLEX_BASE (7) should be invalid
        for tag in 0u8..7 {
            let raw = (tag as u32) << 24;
            let result = MessageHeader::from_raw(raw);
            assert!(result.is_err(), "Tag {tag} should be invalid");
        }
    }

    /// Verify truncated header input returns error.
    #[test]
    fn message_header_truncated_returns_error() {
        for len in 0..4 {
            let bytes = vec![0x07; len]; // Valid tag byte repeated
            let result = MessageHeader::decode(&bytes);
            assert!(result.is_err(), "Expected error for {len} bytes");
        }
    }

    /// Stress test message header parsing with random data.
    #[test]
    fn message_header_stress_test() {
        for seed in 0..500 {
            let bytes = generate_random_bytes(seed, 16);

            // Try parsing at various offsets
            for offset in 0..12 {
                if offset + 4 <= bytes.len() {
                    let _ = MessageHeader::decode(&bytes[offset..]);
                }
            }
        }
    }
}

// ============================================================================
// Module: Delta/Token Parser Fuzz Tests
// ============================================================================

mod delta_fuzz {
    use super::*;

    /// Verify read_int never panics on arbitrary bytes.
    #[test]
    fn read_int_no_panic() {
        for seed in 0..100 {
            for len in 0..=8 {
                let bytes = generate_random_bytes(seed, len);
                let mut cursor = Cursor::new(&bytes[..]);
                // Should not panic
                let _ = read_int(&mut cursor);
            }
        }
    }

    /// Verify read_token never panics on arbitrary bytes.
    #[test]
    fn read_token_no_panic() {
        for seed in 0..100 {
            for len in 0..=8 {
                let bytes = generate_random_bytes(seed, len);
                let mut cursor = Cursor::new(&bytes[..]);
                // Should not panic
                let _ = read_token(&mut cursor);
            }
        }
    }

    /// Verify read_delta_op never panics on arbitrary bytes.
    #[test]
    fn read_delta_op_no_panic() {
        for seed in 0..100 {
            for len in 0..=16 {
                let bytes = generate_random_bytes(seed, len);
                let mut cursor = Cursor::new(&bytes[..]);
                // Should not panic
                let _ = read_delta_op(&mut cursor);
            }
        }
    }

    /// Verify delta op handles invalid opcodes gracefully.
    #[test]
    fn read_delta_op_invalid_opcodes() {
        // Valid opcodes are 0x00 (Literal) and 0x01 (Copy)
        // All others should return error
        for opcode in 2u8..=255 {
            let bytes = [opcode, 0, 0, 0, 0];
            let mut cursor = Cursor::new(&bytes[..]);
            let result = read_delta_op(&mut cursor);
            assert!(result.is_err(), "Opcode {opcode:#x} should be invalid");
            let err = result.unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        }
    }

    /// Verify delta op with valid opcodes but truncated data.
    #[test]
    fn read_delta_op_truncated_data() {
        // Literal opcode with no length
        let bytes = [0x00];
        let mut cursor = Cursor::new(&bytes[..]);
        let result = read_delta_op(&mut cursor);
        assert!(result.is_err());

        // Copy opcode with no index
        let bytes = [0x01];
        let mut cursor = Cursor::new(&bytes[..]);
        let result = read_delta_op(&mut cursor);
        assert!(result.is_err());
    }

    /// Verify read_token with edge case values.
    #[test]
    fn read_token_edge_values() {
        let test_cases = [
            (vec![0x00, 0x00, 0x00, 0x00], None),           // End marker (0)
            (vec![0x01, 0x00, 0x00, 0x00], Some(1)),        // Positive
            (vec![0xFF, 0xFF, 0xFF, 0xFF], Some(-1)),       // Block match 0
            (vec![0xFE, 0xFF, 0xFF, 0xFF], Some(-2)),       // Block match 1
            (vec![0xFF, 0xFF, 0xFF, 0x7F], Some(i32::MAX)), // Max positive
        ];

        for (bytes, expected) in test_cases {
            let mut cursor = Cursor::new(&bytes[..]);
            let result = read_token(&mut cursor);
            assert!(result.is_ok());
            assert_eq!(result.unwrap(), expected);
        }
    }

    /// Stress test delta parsing with random data.
    #[test]
    fn delta_stress_test() {
        for seed in 0..200 {
            let len = (seed % 50) as usize + 1;
            let bytes = generate_random_bytes(seed, len);
            let mut cursor = Cursor::new(&bytes[..]);

            // Try to read multiple delta ops
            for _ in 0..10 {
                let _ = read_delta_op(&mut cursor);
                if cursor.position() as usize >= bytes.len() {
                    break;
                }
            }
        }
    }
}

// ============================================================================
// Module: Protocol Codec Fuzz Tests
// ============================================================================

mod protocol_codec_fuzz {
    use super::*;

    /// Verify protocol codec read_file_size never panics.
    #[test]
    fn read_file_size_no_panic() {
        for version in [28u8, 29, 30, 31, 32] {
            let codec = create_protocol_codec(version);

            for seed in 0..50 {
                for len in 0..=16 {
                    let bytes = generate_random_bytes(seed * 100 + len as u64, len);
                    let mut cursor = Cursor::new(&bytes[..]);
                    // Should not panic
                    let _ = codec.read_file_size(&mut cursor);
                }
            }
        }
    }

    /// Verify protocol codec read_mtime never panics.
    #[test]
    fn read_mtime_no_panic() {
        for version in [28u8, 29, 30, 31, 32] {
            let codec = create_protocol_codec(version);

            for seed in 0..50 {
                for len in 0..=16 {
                    let bytes = generate_random_bytes(seed * 100 + len as u64, len);
                    let mut cursor = Cursor::new(&bytes[..]);
                    // Should not panic
                    let _ = codec.read_mtime(&mut cursor);
                }
            }
        }
    }

    /// Verify protocol codec read_int never panics.
    #[test]
    fn read_int_no_panic() {
        for version in [28u8, 29, 30, 31, 32] {
            let codec = create_protocol_codec(version);

            for seed in 0..50 {
                for len in 0..=8 {
                    let bytes = generate_random_bytes(seed * 100 + len as u64, len);
                    let mut cursor = Cursor::new(&bytes[..]);
                    // Should not panic
                    let _ = codec.read_int(&mut cursor);
                }
            }
        }
    }

    /// Verify protocol codec read_long_name_len never panics.
    #[test]
    fn read_long_name_len_no_panic() {
        for version in [28u8, 29, 30, 31, 32] {
            let codec = create_protocol_codec(version);

            for seed in 0..50 {
                for len in 0..=8 {
                    let bytes = generate_random_bytes(seed * 100 + len as u64, len);
                    let mut cursor = Cursor::new(&bytes[..]);
                    // Should not panic
                    let _ = codec.read_long_name_len(&mut cursor);
                }
            }
        }
    }

    /// Verify protocol codec read_varint never panics.
    #[test]
    fn codec_read_varint_no_panic() {
        for version in [28u8, 29, 30, 31, 32] {
            let codec = create_protocol_codec(version);

            for seed in 0..50 {
                for len in 0..=8 {
                    let bytes = generate_random_bytes(seed * 100 + len as u64, len);
                    let mut cursor = Cursor::new(&bytes[..]);
                    // Should not panic
                    let _ = codec.read_varint(&mut cursor);
                }
            }
        }
    }

    /// Verify truncated inputs return proper errors.
    #[test]
    fn protocol_codec_truncated_errors() {
        // Legacy codec needs 4 bytes for most operations
        let legacy = create_protocol_codec(29);
        let truncated = [0x00, 0x00, 0x00]; // 3 bytes

        let mut cursor = Cursor::new(&truncated[..]);
        assert!(legacy.read_file_size(&mut cursor).is_err());

        let mut cursor = Cursor::new(&truncated[..]);
        assert!(legacy.read_mtime(&mut cursor).is_err());

        let mut cursor = Cursor::new(&truncated[..]);
        assert!(legacy.read_int(&mut cursor).is_err());

        let mut cursor = Cursor::new(&truncated[..]);
        assert!(legacy.read_long_name_len(&mut cursor).is_err());
    }

    /// Test with edge case byte patterns.
    #[test]
    fn protocol_codec_edge_patterns() {
        for version in [28u8, 29, 30, 31, 32] {
            let codec = create_protocol_codec(version);

            for pattern in edge_case_byte_patterns() {
                let mut cursor = Cursor::new(&pattern[..]);
                let _ = codec.read_file_size(&mut cursor);

                let mut cursor = Cursor::new(&pattern[..]);
                let _ = codec.read_mtime(&mut cursor);

                let mut cursor = Cursor::new(&pattern[..]);
                let _ = codec.read_int(&mut cursor);

                let mut cursor = Cursor::new(&pattern[..]);
                let _ = codec.read_long_name_len(&mut cursor);
            }
        }
    }
}

// ============================================================================
// Module: File Entry Decoder Fuzz Tests (flags parsing)
// ============================================================================

mod file_entry_fuzz {
    use super::*;

    /// Test that arbitrary flag bytes don't cause panics.
    /// (Flags parsing is internal, but we can test the varint-based path)
    #[test]
    fn flag_byte_parsing_no_panic() {
        // Flags are typically single bytes or varints
        for flag_byte in 0u8..=255 {
            let bytes = [flag_byte];
            let mut cursor = Cursor::new(&bytes[..]);
            // This would be used in flags parsing
            let _ = read_varint(&mut cursor);
        }

        // Extended flags (2 bytes)
        for seed in 0..100 {
            let bytes = generate_random_bytes(seed, 2);
            let mut cursor = Cursor::new(&bytes[..]);
            let _ = read_varint(&mut cursor);
        }

        // Extended 16-bit flags (3 bytes in varint)
        for seed in 0..100 {
            let bytes = generate_random_bytes(seed, 3);
            let mut cursor = Cursor::new(&bytes[..]);
            let _ = read_varint(&mut cursor);
        }
    }

    /// Test that mode values (u32 from 4-byte LE) parse without panic.
    #[test]
    fn mode_parsing_no_panic() {
        for seed in 0..100 {
            let bytes = generate_random_bytes(seed, 4);
            let result = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            // Just verify it doesn't panic and produces valid u32
            let _ = result as u32;
        }
    }
}

// ============================================================================
// Module: Combined Stress Tests
// ============================================================================

mod stress_tests {
    use super::*;

    /// Massive stress test: feed random data to all parsers.
    #[test]
    fn combined_stress_all_parsers() {
        for seed in 0..100 {
            let len = (seed % 100) as usize + 1;
            let bytes = generate_random_bytes(seed, len);

            // Varint
            let mut cursor = Cursor::new(&bytes[..]);
            let _ = read_varint(&mut cursor);

            // Varlong
            let mut cursor = Cursor::new(&bytes[..]);
            let _ = read_varlong(&mut cursor, 3);

            // NDX codecs
            let mut legacy = LegacyNdxCodec::new(29);
            let mut cursor = Cursor::new(&bytes[..]);
            let _ = legacy.read_ndx(&mut cursor);

            let mut modern = ModernNdxCodec::new(30);
            let mut cursor = Cursor::new(&bytes[..]);
            let _ = modern.read_ndx(&mut cursor);

            // Message header
            let _ = MessageHeader::decode(&bytes);

            // Delta operations
            let mut cursor = Cursor::new(&bytes[..]);
            let _ = read_delta_op(&mut cursor);

            // Protocol codecs
            for version in [29u8, 30, 31, 32] {
                let codec = create_protocol_codec(version);
                let mut cursor = Cursor::new(&bytes[..]);
                let _ = codec.read_file_size(&mut cursor);
            }
        }
    }

    /// Test with pathological patterns designed to stress parsers.
    #[test]
    fn pathological_patterns() {
        let patterns = [
            // All zeros
            vec![0x00; 100],
            // All ones
            vec![0xFF; 100],
            // Alternating
            (0..100)
                .map(|i| if i % 2 == 0 { 0x00 } else { 0xFF })
                .collect(),
            // Ramp up
            (0u8..100).collect(),
            // Ramp down
            (0u8..100).rev().collect(),
            // Pattern that looks like many extended NDX markers
            [0xFE, 0x80, 0x00, 0x00, 0x00].repeat(20),
            // Pattern that looks like many negative NDX markers
            [0xFF, 0x01].repeat(50),
            // Pattern with many zeros then 0xFF markers
            [vec![0x00; 50], vec![0xFF; 50]].concat(),
        ];

        for pattern in patterns {
            // Test all parsers
            let mut cursor = Cursor::new(&pattern[..]);
            let _ = read_varint(&mut cursor);

            let mut cursor = Cursor::new(&pattern[..]);
            let _ = read_varlong(&mut cursor, 3);

            let mut legacy = LegacyNdxCodec::new(29);
            let mut cursor = Cursor::new(&pattern[..]);
            let _ = legacy.read_ndx(&mut cursor);

            let mut modern = ModernNdxCodec::new(30);
            let mut cursor = Cursor::new(&pattern[..]);
            let _ = modern.read_ndx(&mut cursor);

            let _ = MessageHeader::decode(&pattern);

            let mut cursor = Cursor::new(&pattern[..]);
            let _ = read_delta_op(&mut cursor);
        }
    }

    /// Test sequential parsing of random stream.
    #[test]
    fn sequential_parsing_stress() {
        for seed in 0..50 {
            let bytes = generate_random_bytes(seed, 1000);
            let mut cursor = Cursor::new(&bytes[..]);

            // Try to parse as many values as possible
            let mut count = 0;
            while (cursor.position() as usize) < bytes.len() - 8 {
                let _ = read_varint(&mut cursor);
                count += 1;
                if count > 500 {
                    break;
                }
            }
        }
    }
}

// ============================================================================
// Module: Error Type Verification
// ============================================================================

mod error_verification {
    use super::*;

    /// Verify that varint decoder returns UnexpectedEof for empty input.
    #[test]
    fn varint_empty_is_unexpected_eof() {
        let result = decode_varint(&[]);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }

    /// Verify that legacy NDX returns UnexpectedEof for short input.
    #[test]
    fn legacy_ndx_short_is_unexpected_eof() {
        let mut codec = LegacyNdxCodec::new(29);
        let bytes = [0x00, 0x00, 0x00]; // Only 3 bytes

        let mut cursor = Cursor::new(&bytes[..]);
        let result = codec.read_ndx(&mut cursor);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }

    /// Verify that modern NDX returns UnexpectedEof for truncated extended.
    #[test]
    fn modern_ndx_truncated_extended_is_unexpected_eof() {
        let mut codec = ModernNdxCodec::new(30);
        let bytes = [0xFE, 0x00]; // Extended marker + only 1 byte

        let mut cursor = Cursor::new(&bytes[..]);
        let result = codec.read_ndx(&mut cursor);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }

    /// Verify that delta op returns InvalidData for bad opcode.
    #[test]
    fn delta_op_bad_opcode_is_invalid_data() {
        let bytes = [0x02, 0x00, 0x00, 0x00]; // Opcode 2 is invalid

        let mut cursor = Cursor::new(&bytes[..]);
        let result = read_delta_op(&mut cursor);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    }

    /// Verify that message header returns appropriate error for bad tag.
    #[test]
    fn message_header_bad_tag_is_error() {
        // Tag < 7 (MPLEX_BASE) should fail
        let bytes = vec![0x00, 0x00, 0x00, 0x06]; // Tag = 6
        let result = MessageHeader::decode(&bytes);
        assert!(result.is_err());
    }
}
