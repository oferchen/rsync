//! Comprehensive tests for varint/vstring codec operations.
//!
//! These tests verify the public API for variable-length integer encoding used
//! throughout the rsync protocol. The tests cover:
//!
//! - Boundary values (0, 1, 127, 128, 255, 256, max values)
//! - Roundtrip encoding/decoding for all integer types
//! - Error cases (truncated input, invalid encoding)
//! - Edge cases specific to rsync protocol (file sizes, timestamps, compatibility flags)
//! - Protocol version-dependent encoding (varint30 vs fixed int)

use protocol::{
    decode_varint, encode_varint_to_vec, read_int, read_longint, read_varint, read_varint30_int,
    read_varlong, read_varlong30, write_int, write_longint, write_varint, write_varint30_int,
    write_varlong, write_varlong30,
};
use std::io::{self, Cursor};

// ===========================================================================
// VARINT BOUNDARY VALUE TESTS
// ===========================================================================

/// Tests varint encoding at the 1-byte boundary (0-127).
#[test]
fn varint_1byte_boundary_values() {
    let values = [0, 1, 63, 64, 126, 127];

    for value in values {
        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);
        assert_eq!(
            encoded.len(),
            1,
            "value {} should encode to 1 byte, got {}",
            value,
            encoded.len()
        );

        let (decoded, remainder) = decode_varint(&encoded).expect("decode should succeed");
        assert_eq!(decoded, value, "round-trip failed for value {}", value);
        assert!(remainder.is_empty(), "no bytes should remain after decoding");
    }
}

/// Tests varint encoding at the 2-byte boundary (128-16383).
#[test]
fn varint_2byte_boundary_values() {
    let values = [128, 129, 255, 256, 1000, 8192, 16382, 16383];

    for value in values {
        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);
        assert_eq!(
            encoded.len(),
            2,
            "value {} should encode to 2 bytes, got {}",
            value,
            encoded.len()
        );

        let (decoded, _) = decode_varint(&encoded).expect("decode should succeed");
        assert_eq!(decoded, value, "round-trip failed for value {}", value);
    }
}

/// Tests varint encoding at the 3-byte boundary (16384-2097151).
#[test]
fn varint_3byte_boundary_values() {
    let values = [16384, 16385, 100000, 1000000, 2097150, 2097151];

    for value in values {
        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);
        assert_eq!(
            encoded.len(),
            3,
            "value {} should encode to 3 bytes, got {}",
            value,
            encoded.len()
        );

        let (decoded, _) = decode_varint(&encoded).expect("decode should succeed");
        assert_eq!(decoded, value, "round-trip failed for value {}", value);
    }
}

/// Tests varint encoding at the 4-byte boundary (2097152-268435455).
#[test]
fn varint_4byte_boundary_values() {
    let values = [2097152, 2097153, 10000000, 100000000, 268435454, 268435455];

    for value in values {
        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);
        assert_eq!(
            encoded.len(),
            4,
            "value {} should encode to 4 bytes, got {}",
            value,
            encoded.len()
        );

        let (decoded, _) = decode_varint(&encoded).expect("decode should succeed");
        assert_eq!(decoded, value, "round-trip failed for value {}", value);
    }
}

/// Tests varint encoding for 5-byte values (>268435455 and negatives).
#[test]
fn varint_5byte_boundary_values() {
    let values = [268435456, 500000000, i32::MAX, -1, -128, i32::MIN];

    for value in values {
        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);
        assert_eq!(
            encoded.len(),
            5,
            "value {} should encode to 5 bytes, got {}",
            value,
            encoded.len()
        );

        let (decoded, _) = decode_varint(&encoded).expect("decode should succeed");
        assert_eq!(decoded, value, "round-trip failed for value {}", value);
    }
}

// ===========================================================================
// VARINT ROUNDTRIP TESTS
// ===========================================================================

/// Tests complete roundtrip through all public varint APIs.
#[test]
fn varint_complete_api_roundtrip() {
    let test_values = [
        0,
        1,
        127,
        128,
        255,
        256,
        16383,
        16384,
        2097151,
        2097152,
        268435455,
        268435456,
        i32::MAX,
        -1,
        -127,
        -128,
        i32::MIN,
    ];

    for value in test_values {
        // Test encode_varint_to_vec + decode_varint
        let mut vec_encoded = Vec::new();
        encode_varint_to_vec(value, &mut vec_encoded);
        let (decoded_from_vec, remainder) =
            decode_varint(&vec_encoded).expect("decode_varint should succeed");
        assert_eq!(decoded_from_vec, value, "vec roundtrip failed for {}", value);
        assert!(remainder.is_empty());

        // Test write_varint + read_varint
        let mut write_encoded = Vec::new();
        write_varint(&mut write_encoded, value).expect("write_varint should succeed");
        let mut cursor = Cursor::new(&write_encoded);
        let decoded_from_stream =
            read_varint(&mut cursor).expect("read_varint should succeed");
        assert_eq!(
            decoded_from_stream, value,
            "stream roundtrip failed for {}",
            value
        );
        assert_eq!(cursor.position() as usize, write_encoded.len());

        // Verify both methods produce identical encoding
        assert_eq!(
            vec_encoded, write_encoded,
            "encode methods should produce identical output for {}",
            value
        );
    }
}

/// Tests decoding multiple sequential varints from a single buffer.
#[test]
fn varint_sequential_decoding() {
    let values = [0, 127, 128, 16384, i32::MAX, -1, i32::MIN];
    let mut encoded = Vec::new();

    // Encode all values
    for &v in &values {
        encode_varint_to_vec(v, &mut encoded);
    }

    // Decode via slice
    let mut remaining = encoded.as_slice();
    for &expected in &values {
        let (decoded, rest) = decode_varint(remaining).expect("decode should succeed");
        assert_eq!(decoded, expected);
        remaining = rest;
    }
    assert!(remaining.is_empty(), "all bytes should be consumed");

    // Decode via stream
    let mut cursor = Cursor::new(&encoded);
    for &expected in &values {
        let decoded = read_varint(&mut cursor).expect("read should succeed");
        assert_eq!(decoded, expected);
    }
    assert_eq!(cursor.position() as usize, encoded.len());
}

// ===========================================================================
// VARINT ERROR CASES
// ===========================================================================

/// Tests that empty input produces UnexpectedEof error.
#[test]
fn varint_empty_input_error() {
    let empty: [u8; 0] = [];

    // decode_varint
    let err = decode_varint(&empty).expect_err("empty input should fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);

    // read_varint
    let mut cursor = Cursor::new(&empty[..]);
    let err = read_varint(&mut cursor).expect_err("empty input should fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

/// Tests truncated varint input detection.
#[test]
fn varint_truncated_input_error() {
    // 2-byte varint truncated to 1 byte (0x80 indicates 1 extra byte needed)
    let truncated_2 = [0x80u8];
    let err = decode_varint(&truncated_2).expect_err("truncated 2-byte should fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);

    // 3-byte varint truncated to 2 bytes (0xC0 indicates 2 extra bytes needed)
    let truncated_3 = [0xC0u8, 0x00];
    let err = decode_varint(&truncated_3).expect_err("truncated 3-byte should fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);

    // 4-byte varint truncated to 3 bytes (0xE0 indicates 3 extra bytes needed)
    let truncated_4 = [0xE0u8, 0x00, 0x00];
    let err = decode_varint(&truncated_4).expect_err("truncated 4-byte should fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);

    // 5-byte varint truncated to 4 bytes (0xF0 indicates 4 extra bytes needed)
    let truncated_5 = [0xF0u8, 0x00, 0x00, 0x00];
    let err = decode_varint(&truncated_5).expect_err("truncated 5-byte should fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

/// Tests that overflow tag bytes are rejected with InvalidData error.
#[test]
fn varint_overflow_tag_error() {
    // 0xF8-0xFB indicate 5 extra bytes (overflow for i32)
    let overflow_5extra = [0xF8u8, 0, 0, 0, 0, 0];
    let err = decode_varint(&overflow_5extra).expect_err("overflow tag should fail");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(
        err.to_string().contains("overflow"),
        "error message should mention overflow"
    );

    // 0xFC-0xFF indicate 6 extra bytes (also overflow)
    let overflow_6extra = [0xFCu8, 0, 0, 0, 0, 0, 0];
    let err = decode_varint(&overflow_6extra).expect_err("overflow tag should fail");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

// ===========================================================================
// FIXED INT (4-BYTE) TESTS
// ===========================================================================

/// Tests fixed 4-byte integer encoding roundtrip.
#[test]
fn fixed_int_roundtrip() {
    let values = [0, 1, 127, 128, 255, 256, 65535, 65536, i32::MAX, i32::MIN, -1];

    for value in values {
        let mut encoded = Vec::new();
        write_int(&mut encoded, value).expect("write_int should succeed");
        assert_eq!(encoded.len(), 4, "fixed int should always be 4 bytes");

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_int(&mut cursor).expect("read_int should succeed");
        assert_eq!(decoded, value, "roundtrip failed for {}", value);
    }
}

/// Tests fixed int with truncated input.
#[test]
fn fixed_int_truncated_error() {
    let truncated = [0x42u8, 0x00, 0x00]; // Only 3 bytes
    let mut cursor = Cursor::new(&truncated[..]);
    let err = read_int(&mut cursor).expect_err("truncated input should fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

// ===========================================================================
// LONGINT (LEGACY 64-BIT) TESTS
// ===========================================================================

/// Tests longint encoding for values that fit in 4 bytes.
#[test]
fn longint_inline_values() {
    // Values 0 to 0x7FFFFFFF fit in 4 bytes (inline format)
    let values = [0i64, 1, 1000, 0x7FFF_FFFFi64];

    for value in values {
        let mut encoded = Vec::new();
        write_longint(&mut encoded, value).expect("write should succeed");
        assert_eq!(encoded.len(), 4, "inline longint should be 4 bytes for {}", value);

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_longint(&mut cursor).expect("read should succeed");
        assert_eq!(decoded, value, "roundtrip failed for {}", value);
    }
}

/// Tests longint encoding for values requiring extended format.
#[test]
fn longint_extended_values() {
    // Values > 0x7FFFFFFF or negative require 12 bytes (4-byte marker + 8-byte value)
    let values = [0x8000_0000i64, i64::MAX, i64::MIN, -1i64, 0x1_0000_0000i64];

    for value in values {
        let mut encoded = Vec::new();
        write_longint(&mut encoded, value).expect("write should succeed");
        assert_eq!(
            encoded.len(),
            12,
            "extended longint should be 12 bytes for {}",
            value
        );

        // First 4 bytes should be the marker 0xFFFFFFFF
        let marker = u32::from_le_bytes(encoded[0..4].try_into().unwrap());
        assert_eq!(marker, 0xFFFF_FFFF, "marker should be 0xFFFFFFFF");

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_longint(&mut cursor).expect("read should succeed");
        assert_eq!(decoded, value, "roundtrip failed for {}", value);
    }
}

/// Tests longint boundary at 0x7FFFFFFF/0x80000000.
#[test]
fn longint_boundary() {
    // 0x7FFFFFFF is max inline
    let max_inline = 0x7FFF_FFFFi64;
    let mut enc1 = Vec::new();
    write_longint(&mut enc1, max_inline).expect("write should succeed");
    assert_eq!(enc1.len(), 4);

    // 0x80000000 requires extended
    let min_extended = 0x8000_0000i64;
    let mut enc2 = Vec::new();
    write_longint(&mut enc2, min_extended).expect("write should succeed");
    assert_eq!(enc2.len(), 12);
}

/// Tests longint truncated input handling.
#[test]
fn longint_truncated_error() {
    // Marker present (0xFFFFFFFF) but value truncated
    let truncated = [0xFFu8, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00]; // Only 3 bytes of value
    let mut cursor = Cursor::new(&truncated[..]);
    let err = read_longint(&mut cursor).expect_err("truncated should fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

// ===========================================================================
// VARLONG (64-BIT VARIABLE LENGTH) TESTS
// ===========================================================================

/// Tests varlong roundtrip with various min_bytes values.
#[test]
fn varlong_roundtrip_various_min_bytes() {
    let value = 0x1234_5678i64;

    for min_bytes in 1u8..=8 {
        let mut encoded = Vec::new();
        write_varlong(&mut encoded, value, min_bytes).expect("write should succeed");

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varlong(&mut cursor, min_bytes).expect("read should succeed");
        assert_eq!(decoded, value, "roundtrip failed for min_bytes={}", min_bytes);
    }
}

/// Tests varlong with typical file size values.
#[test]
fn varlong_file_sizes() {
    // File sizes typically use min_bytes=3
    let sizes = [
        0i64,                          // Empty file
        1024,                          // 1 KB
        1_048_576,                     // 1 MB
        1_073_741_824,                 // 1 GB
        1_099_511_627_776,             // 1 TB
        100_000_000_000_000,           // 100 TB
    ];

    for size in sizes {
        let mut encoded = Vec::new();
        write_varlong(&mut encoded, size, 3).expect("write should succeed");

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varlong(&mut cursor, 3).expect("read should succeed");
        assert_eq!(decoded, size, "roundtrip failed for size {}", size);
    }
}

/// Tests varlong with typical timestamp values.
#[test]
fn varlong_timestamps() {
    // Timestamps typically use min_bytes=4
    let timestamps = [
        0i64,             // Unix epoch
        1_000_000_000,    // Sep 2001
        1_700_000_000,    // Nov 2023
        2_147_483_647,    // Jan 2038 (32-bit limit)
        2_147_483_648,    // After Y2038
        4_000_000_000,    // Dec 2096
    ];

    for ts in timestamps {
        let mut encoded = Vec::new();
        write_varlong(&mut encoded, ts, 4).expect("write should succeed");

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varlong(&mut cursor, 4).expect("read should succeed");
        assert_eq!(decoded, ts, "roundtrip failed for timestamp {}", ts);
    }
}

/// Tests varlong with zero value and various min_bytes.
#[test]
fn varlong_zero_all_min_bytes() {
    for min_bytes in 1u8..=8 {
        let mut encoded = Vec::new();
        write_varlong(&mut encoded, 0, min_bytes).expect("write should succeed");

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varlong(&mut cursor, min_bytes).expect("read should succeed");
        assert_eq!(decoded, 0, "zero failed for min_bytes={}", min_bytes);
    }
}

/// Tests varlong truncated input.
#[test]
fn varlong_truncated_error() {
    // Empty input
    let empty: [u8; 0] = [];
    let mut cursor = Cursor::new(&empty[..]);
    let err = read_varlong(&mut cursor, 3).expect_err("empty should fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);

    // Leading byte present but extra bytes missing
    let truncated = [0x80u8]; // Indicates more bytes follow
    let mut cursor = Cursor::new(&truncated[..]);
    let err = read_varlong(&mut cursor, 1).expect_err("truncated should fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

// ===========================================================================
// VARLONG30 (PROTOCOL 30+ ALIAS) TESTS
// ===========================================================================

/// Tests that varlong30 is an alias for varlong.
#[test]
fn varlong30_is_varlong_alias() {
    let value = 1_234_567_890i64;
    let min_bytes = 3u8;

    let mut enc_varlong = Vec::new();
    write_varlong(&mut enc_varlong, value, min_bytes).expect("write_varlong should succeed");

    let mut enc_varlong30 = Vec::new();
    write_varlong30(&mut enc_varlong30, value, min_bytes).expect("write_varlong30 should succeed");

    assert_eq!(enc_varlong, enc_varlong30, "encodings should be identical");

    let mut cursor = Cursor::new(&enc_varlong30);
    let decoded = read_varlong30(&mut cursor, min_bytes).expect("read_varlong30 should succeed");
    assert_eq!(decoded, value);
}

// ===========================================================================
// VARINT30_INT (PROTOCOL VERSION DEPENDENT) TESTS
// ===========================================================================

/// Tests that protocol < 30 uses fixed 4-byte encoding.
#[test]
fn varint30_int_legacy_protocol() {
    let value = 1000;

    for proto in [28u8, 29] {
        let mut encoded = Vec::new();
        write_varint30_int(&mut encoded, value, proto).expect("write should succeed");
        assert_eq!(encoded.len(), 4, "proto {} should use 4-byte encoding", proto);

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varint30_int(&mut cursor, proto).expect("read should succeed");
        assert_eq!(decoded, value);
    }
}

/// Tests that protocol >= 30 uses variable-length encoding.
#[test]
fn varint30_int_modern_protocol() {
    let value = 1000;

    for proto in [30u8, 31, 32] {
        let mut encoded = Vec::new();
        write_varint30_int(&mut encoded, value, proto).expect("write should succeed");
        assert!(
            encoded.len() < 4,
            "proto {} should use varint encoding (< 4 bytes)",
            proto
        );

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varint30_int(&mut cursor, proto).expect("read should succeed");
        assert_eq!(decoded, value);
    }
}

/// Tests varint30_int roundtrip for various values and protocols.
#[test]
fn varint30_int_roundtrip() {
    let values = [0, 1, 127, 128, 255, 16384, i32::MAX, -1];

    for proto in [28u8, 29, 30, 31, 32] {
        for value in values {
            let mut encoded = Vec::new();
            write_varint30_int(&mut encoded, value, proto).expect("write should succeed");

            let mut cursor = Cursor::new(&encoded);
            let decoded = read_varint30_int(&mut cursor, proto).expect("read should succeed");
            assert_eq!(
                decoded, value,
                "roundtrip failed for value={} proto={}",
                value, proto
            );
        }
    }
}

// ===========================================================================
// RSYNC PROTOCOL EDGE CASES
// ===========================================================================

/// Tests encoding of typical compatibility flag values.
#[test]
fn varint_compatibility_flags() {
    // Compatibility flags are typically small positive integers
    // Values 0-127 fit in 1 byte, values 128-255 need 2 bytes
    let flag_values_1byte = [
        (0b0000_0001, 1), // INC_RECURSE
        (0b0000_0011, 1), // INC_RECURSE | SYMLINK_TIMES
        (0b0000_0111, 1), // Multiple flags
        (0b0111_1111, 1), // 7 flags (max 1-byte)
    ];

    let flag_values_2byte = [
        (0b1000_0000, 2), // 8th flag alone
        (0b1111_1111, 2), // All 8 flags set (255 > 127)
    ];

    for (flags, expected_len) in flag_values_1byte.iter().chain(flag_values_2byte.iter()) {
        let mut encoded = Vec::new();
        encode_varint_to_vec(*flags, &mut encoded);
        assert_eq!(
            encoded.len(),
            *expected_len,
            "flags {} should be {} byte(s)",
            flags,
            expected_len
        );

        let (decoded, _) = decode_varint(&encoded).expect("decode should succeed");
        assert_eq!(decoded, *flags);
    }
}

/// Tests encoding of typical file list index values.
#[test]
fn varint_file_list_indices() {
    // File list indices are non-negative integers that can get large
    let indices = [0, 1, 100, 1000, 10000, 100000, 1000000];

    for idx in indices {
        let mut encoded = Vec::new();
        encode_varint_to_vec(idx, &mut encoded);

        let (decoded, _) = decode_varint(&encoded).expect("decode should succeed");
        assert_eq!(decoded, idx);
    }
}

/// Tests encoding of special NDX values used in file list protocol.
#[test]
fn varint_special_ndx_values() {
    // NDX_DONE is encoded as 0
    let ndx_done = 0;
    let mut enc = Vec::new();
    encode_varint_to_vec(ndx_done, &mut enc);
    assert_eq!(enc, [0x00]);

    // File indices are 1-based in some contexts
    let first_file = 1;
    let mut enc = Vec::new();
    encode_varint_to_vec(first_file, &mut enc);
    assert_eq!(enc, [0x01]);
}

/// Tests that negative values (used for special markers) encode correctly.
#[test]
fn varint_negative_markers() {
    // -1 is sometimes used as a special marker
    let marker = -1i32;
    let mut encoded = Vec::new();
    encode_varint_to_vec(marker, &mut encoded);
    assert_eq!(encoded.len(), 5, "-1 requires 5 bytes");

    let (decoded, _) = decode_varint(&encoded).expect("decode should succeed");
    assert_eq!(decoded, marker);
}

/// Tests encoding at exact byte boundaries for protocol efficiency verification.
#[test]
fn varint_byte_boundary_efficiency() {
    // Verify that boundary values use minimum bytes
    let efficiency_tests = [
        (127, 1, "7-bit max"),
        (128, 2, "8-bit min"),
        (16383, 2, "14-bit max"),
        (16384, 3, "15-bit min"),
        (2097151, 3, "21-bit max"),
        (2097152, 4, "22-bit min"),
        (268435455, 4, "28-bit max"),
        (268435456, 5, "29-bit min"),
    ];

    for (value, expected_len, desc) in efficiency_tests {
        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);
        assert_eq!(
            encoded.len(),
            expected_len,
            "{}: value {} should encode to {} bytes, got {}",
            desc,
            value,
            expected_len,
            encoded.len()
        );
    }
}

// ===========================================================================
// KNOWN WIRE FORMAT VERIFICATION
// ===========================================================================

/// Tests encoding against known wire format values from upstream rsync.
#[test]
fn varint_known_wire_format() {
    // These encodings match upstream rsync io.c
    let known_encodings = [
        (0, "00"),
        (1, "01"),
        (127, "7f"),
        (128, "8080"),
        (255, "80ff"),
        (256, "8100"),
        (16384, "c00040"),
        (1_073_741_824, "f000000040"),
        (-1, "f0ffffffff"),
        (-128, "f080ffffff"),
        (-129, "f07fffffff"),
        (-32768, "f00080ffff"),
    ];

    for (value, expected_hex) in known_encodings {
        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);
        let actual_hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            actual_hex, expected_hex,
            "value {} should encode as {}, got {}",
            value, expected_hex, actual_hex
        );

        // Verify roundtrip
        let (decoded, _) = decode_varint(&encoded).expect("decode should succeed");
        assert_eq!(decoded, value);
    }
}

/// Tests that longint marker bytes match upstream format.
#[test]
fn longint_marker_wire_format() {
    let large_value = 0x8000_0000i64;
    let mut encoded = Vec::new();
    write_longint(&mut encoded, large_value).expect("write should succeed");

    // First 4 bytes should be 0xFFFFFFFF marker (little-endian)
    assert_eq!(&encoded[0..4], &[0xFF, 0xFF, 0xFF, 0xFF]);

    // Next 8 bytes should be the value (little-endian)
    let value_bytes = &encoded[4..12];
    let decoded_value = i64::from_le_bytes(value_bytes.try_into().unwrap());
    assert_eq!(decoded_value, large_value);
}

// ===========================================================================
// STRESS TESTS
// ===========================================================================

/// Tests encoding/decoding many sequential values.
#[test]
fn varint_many_sequential_values() {
    let mut encoded = Vec::new();
    let count = 1000;

    // Encode 0 to count-1
    for i in 0..count {
        encode_varint_to_vec(i, &mut encoded);
    }

    // Decode and verify
    let mut remaining = encoded.as_slice();
    for expected in 0..count {
        let (decoded, rest) = decode_varint(remaining).expect("decode should succeed");
        assert_eq!(decoded, expected);
        remaining = rest;
    }
    assert!(remaining.is_empty());
}

/// Tests that powers of two encode and decode correctly.
#[test]
fn varint_powers_of_two() {
    for shift in 0..31 {
        let value: i32 = 1 << shift;
        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);

        let (decoded, _) = decode_varint(&encoded).expect("decode should succeed");
        assert_eq!(decoded, value, "failed for 2^{}", shift);

        // Also test value - 1 and value + 1 (if not overflow)
        if value > 1 {
            let mut enc2 = Vec::new();
            encode_varint_to_vec(value - 1, &mut enc2);
            let (dec2, _) = decode_varint(&enc2).expect("decode should succeed");
            assert_eq!(dec2, value - 1);
        }

        if value < i32::MAX {
            let mut enc3 = Vec::new();
            encode_varint_to_vec(value + 1, &mut enc3);
            let (dec3, _) = decode_varint(&enc3).expect("decode should succeed");
            assert_eq!(dec3, value + 1);
        }
    }
}

/// Tests negative powers of two.
#[test]
fn varint_negative_powers_of_two() {
    for shift in 0..31 {
        let value: i32 = -(1 << shift);
        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);

        let (decoded, _) = decode_varint(&encoded).expect("decode should succeed");
        assert_eq!(decoded, value, "failed for -(2^{})", shift);
    }
}

// ===========================================================================
// INTERLEAVED ENCODING TESTS
// ===========================================================================

/// Tests interleaved varint and fixed int encoding/decoding.
#[test]
fn interleaved_varint_and_fixed_int() {
    let mut buffer = Vec::new();

    // Write interleaved values (simulating real protocol usage)
    write_varint(&mut buffer, 42).expect("varint write");
    write_int(&mut buffer, 1000).expect("int write");
    write_varint(&mut buffer, 16384).expect("varint write");
    write_int(&mut buffer, -1).expect("int write");

    // Read them back
    let mut cursor = Cursor::new(&buffer);

    let v1 = read_varint(&mut cursor).expect("varint read");
    assert_eq!(v1, 42);

    let v2 = read_int(&mut cursor).expect("int read");
    assert_eq!(v2, 1000);

    let v3 = read_varint(&mut cursor).expect("varint read");
    assert_eq!(v3, 16384);

    let v4 = read_int(&mut cursor).expect("int read");
    assert_eq!(v4, -1);

    assert_eq!(cursor.position() as usize, buffer.len());
}

/// Tests interleaved varlong and longint encoding/decoding.
#[test]
fn interleaved_varlong_and_longint() {
    let mut buffer = Vec::new();

    // Write interleaved values
    write_varlong(&mut buffer, 1_000_000, 3).expect("varlong write");
    write_longint(&mut buffer, 0x1_0000_0000i64).expect("longint write");
    write_varlong(&mut buffer, 0, 4).expect("varlong write");
    write_longint(&mut buffer, 100).expect("longint write");

    // Read them back
    let mut cursor = Cursor::new(&buffer);

    let v1 = read_varlong(&mut cursor, 3).expect("varlong read");
    assert_eq!(v1, 1_000_000);

    let v2 = read_longint(&mut cursor).expect("longint read");
    assert_eq!(v2, 0x1_0000_0000i64);

    let v3 = read_varlong(&mut cursor, 4).expect("varlong read");
    assert_eq!(v3, 0);

    let v4 = read_longint(&mut cursor).expect("longint read");
    assert_eq!(v4, 100);

    assert_eq!(cursor.position() as usize, buffer.len());
}
