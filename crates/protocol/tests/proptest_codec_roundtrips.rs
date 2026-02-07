//! Property-based roundtrip tests for protocol codecs using proptest.
//!
//! This module verifies that encoding followed by decoding produces the original
//! value for arbitrary inputs across all protocol codec implementations:
//!
//! - Varint encoding/decoding for i32 values
//! - Varlong encoding/decoding for i64 values (file sizes, timestamps)
//! - NDX codec encoding/decoding for file-list indices
//! - Message frame encoding/decoding for multiplexed protocol messages
//! - Protocol codec wire format roundtrips (file size, mtime, long name length)
//!
//! # Property-Based Testing Strategy
//!
//! These tests use proptest to generate arbitrary values within the valid ranges
//! for each codec. This approach catches edge cases that hand-written tests might
//! miss, such as:
//!
//! - Boundary values at byte boundaries
//! - Negative values with two's complement representation
//! - Sequential value encoding with delta compression
//! - Protocol version-specific encoding differences

use proptest::prelude::*;
use protocol::codec::{
    NDX_DEL_STATS, NDX_DONE, NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, ProtocolCodec,
    create_ndx_codec, create_protocol_codec,
};
use protocol::{
    MessageCode, MessageFrame, MessageHeader, decode_varint, encode_varint_to_vec, read_int,
    read_longint, read_varint, read_varint30_int, read_varlong, write_int, write_longint,
    write_varint, write_varint30_int, write_varlong,
};
use std::io::Cursor;

// ============================================================================
// Varint Roundtrip Tests (i32)
// ============================================================================

proptest! {
    /// All i32 values should roundtrip through varint encoding.
    #[test]
    fn varint_roundtrips_arbitrary_i32(value in any::<i32>()) {
        // Encode to vec
        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);

        // Decode from slice
        let (decoded, remainder) = decode_varint(&encoded)?;
        prop_assert_eq!(decoded, value);
        prop_assert!(remainder.is_empty());

        // Decode from reader
        let mut cursor = Cursor::new(&encoded);
        let read_back = read_varint(&mut cursor)?;
        prop_assert_eq!(read_back, value);
        prop_assert_eq!(cursor.position() as usize, encoded.len());
    }

    /// Varint encoding should be deterministic.
    #[test]
    fn varint_encoding_is_deterministic(value in any::<i32>()) {
        let mut enc1 = Vec::new();
        let mut enc2 = Vec::new();

        encode_varint_to_vec(value, &mut enc1);
        encode_varint_to_vec(value, &mut enc2);

        prop_assert_eq!(enc1, enc2);
    }

    /// write_varint and encode_varint_to_vec should produce identical output.
    #[test]
    fn varint_encode_methods_are_consistent(value in any::<i32>()) {
        let mut vec_encoded = Vec::new();
        encode_varint_to_vec(value, &mut vec_encoded);

        let mut write_encoded = Vec::new();
        write_varint(&mut write_encoded, value)?;

        prop_assert_eq!(vec_encoded, write_encoded);
    }

    /// Sequences of varints should roundtrip correctly.
    #[test]
    fn varint_sequences_roundtrip(values in prop::collection::vec(any::<i32>(), 1..32)) {
        let mut encoded = Vec::new();
        for &v in &values {
            encode_varint_to_vec(v, &mut encoded);
        }

        // Decode via slice
        let mut remaining = encoded.as_slice();
        for &expected in &values {
            let (decoded, rest) = decode_varint(remaining)?;
            prop_assert_eq!(decoded, expected);
            remaining = rest;
        }
        prop_assert!(remaining.is_empty());

        // Decode via reader
        let mut cursor = Cursor::new(&encoded);
        for &expected in &values {
            let decoded = read_varint(&mut cursor)?;
            prop_assert_eq!(decoded, expected);
        }
        prop_assert_eq!(cursor.position() as usize, encoded.len());
    }
}

// ============================================================================
// Fixed Integer (i32) Roundtrip Tests
// ============================================================================

proptest! {
    /// All i32 values should roundtrip through 4-byte fixed encoding.
    #[test]
    fn fixed_int_roundtrips_arbitrary_i32(value in any::<i32>()) {
        let mut encoded = Vec::new();
        write_int(&mut encoded, value)?;

        prop_assert_eq!(encoded.len(), 4, "fixed int always 4 bytes");

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_int(&mut cursor)?;
        prop_assert_eq!(decoded, value);
    }
}

// ============================================================================
// Longint (i64) Roundtrip Tests
// ============================================================================

proptest! {
    /// Non-negative i64 values within longint range should roundtrip.
    ///
    /// Longint uses 4 bytes for values 0 to 0x7FFFFFFF, and 12 bytes for larger values.
    /// Negative values use the 12-byte extended format.
    #[test]
    fn longint_roundtrips_non_negative_i64(value in 0i64..=i64::MAX) {
        let mut encoded = Vec::new();
        write_longint(&mut encoded, value)?;

        // Verify encoding size
        if value <= 0x7FFF_FFFF {
            prop_assert_eq!(encoded.len(), 4, "inline encoding for small values");
        } else {
            prop_assert_eq!(encoded.len(), 12, "extended encoding for large values");
        }

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_longint(&mut cursor)?;
        prop_assert_eq!(decoded, value);
    }

    /// Negative i64 values should roundtrip through longint (12-byte format).
    #[test]
    fn longint_roundtrips_negative_i64(value in i64::MIN..0i64) {
        let mut encoded = Vec::new();
        write_longint(&mut encoded, value)?;

        // Negative values always use extended encoding
        prop_assert_eq!(encoded.len(), 12, "extended encoding for negative values");

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_longint(&mut cursor)?;
        prop_assert_eq!(decoded, value);
    }
}

// ============================================================================
// Varlong (i64) Roundtrip Tests
// ============================================================================

/// Strategy for generating valid file size values (used with min_bytes=3).
///
/// The varlong codec has encoding limitations based on min_bytes:
/// - min_bytes=3: maximum safe value is approximately 0x03FF_FFFF_FFFF_FFFF (~288 PB)
fn file_size_strategy() -> impl Strategy<Value = i64> {
    // File sizes are non-negative
    0i64..=0x03FF_FFFF_FFFF_FFFFi64
}

/// Strategy for generating valid timestamp values (used with min_bytes=4).
fn timestamp_strategy() -> impl Strategy<Value = i64> {
    // Timestamps can be larger with min_bytes=4
    0i64..=0x003F_FFFF_FFFF_FFFFi64
}

proptest! {
    /// File sizes should roundtrip through varlong with min_bytes=3.
    #[test]
    fn varlong_roundtrips_file_sizes(value in file_size_strategy()) {
        let min_bytes = 3u8;
        let mut encoded = Vec::new();
        write_varlong(&mut encoded, value, min_bytes)?;

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varlong(&mut cursor, min_bytes)?;
        prop_assert_eq!(decoded, value);
    }

    /// Timestamps should roundtrip through varlong with min_bytes=4.
    #[test]
    fn varlong_roundtrips_timestamps(value in timestamp_strategy()) {
        let min_bytes = 4u8;
        let mut encoded = Vec::new();
        write_varlong(&mut encoded, value, min_bytes)?;

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varlong(&mut cursor, min_bytes)?;
        prop_assert_eq!(decoded, value);
    }

    /// Varlong encoding with different min_bytes values.
    #[test]
    fn varlong_roundtrips_with_various_min_bytes(
        value in 0i64..=0x00FF_FFFFi64,
        min_bytes in 1u8..=8u8
    ) {
        let mut encoded = Vec::new();
        write_varlong(&mut encoded, value, min_bytes)?;

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varlong(&mut cursor, min_bytes)?;
        prop_assert_eq!(decoded, value);
    }
}

// ============================================================================
// Varint30 Protocol-Dependent Roundtrip Tests
// ============================================================================

proptest! {
    /// Varint30 encoding should roundtrip for all protocol versions.
    #[test]
    fn varint30_roundtrips_all_protocols(
        value in any::<i32>(),
        protocol_version in prop::sample::select(vec![28u8, 29, 30, 31, 32])
    ) {
        let mut encoded = Vec::new();
        write_varint30_int(&mut encoded, value, protocol_version)?;

        // Protocol < 30 uses 4-byte fixed encoding
        if protocol_version < 30 {
            prop_assert_eq!(encoded.len(), 4);
        }

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varint30_int(&mut cursor, protocol_version)?;
        prop_assert_eq!(decoded, value);
    }
}

// ============================================================================
// NDX Codec Roundtrip Tests
// ============================================================================

/// Strategy for generating valid NDX values.
///
/// NDX values include:
/// - Positive file indices (0..N)
/// - Special negative constants (NDX_DONE, NDX_FLIST_EOF, etc.)
fn ndx_value_strategy() -> impl Strategy<Value = i32> {
    prop_oneof![
        // Positive file indices (common case)
        0i32..=1_000_000,
        // Small sequential indices (most common in practice)
        0i32..=1000,
        // Boundary values
        Just(0),
        Just(253),
        Just(254),
        Just(255),
        Just(32767),
        Just(32768),
        Just(65535),
        Just(65536),
        // Special NDX constants
        Just(NDX_DONE),
        Just(NDX_FLIST_EOF),
        Just(NDX_DEL_STATS),
        Just(NDX_FLIST_OFFSET),
    ]
}

proptest! {
    /// Single NDX values should roundtrip for all protocol versions.
    #[test]
    fn ndx_single_value_roundtrips(
        value in ndx_value_strategy(),
        protocol_version in prop::sample::select(vec![28u8, 29, 30, 31, 32])
    ) {
        let mut write_codec = create_ndx_codec(protocol_version);
        let mut buf = Vec::new();
        write_codec.write_ndx(&mut buf, value)?;

        let mut read_codec = create_ndx_codec(protocol_version);
        let mut cursor = Cursor::new(&buf);
        let decoded = read_codec.read_ndx(&mut cursor)?;

        prop_assert_eq!(decoded, value, "NDX roundtrip failed for value={}, protocol={}", value, protocol_version);
    }

    /// Sequences of NDX values should roundtrip correctly.
    ///
    /// The modern NDX codec uses delta encoding, so sequential values are encoded
    /// more compactly. This test verifies that state is maintained correctly.
    #[test]
    fn ndx_sequence_roundtrips(
        values in prop::collection::vec(ndx_value_strategy(), 1..32),
        protocol_version in prop::sample::select(vec![28u8, 29, 30, 31, 32])
    ) {
        let mut write_codec = create_ndx_codec(protocol_version);
        let mut buf = Vec::new();

        for &value in &values {
            write_codec.write_ndx(&mut buf, value)?;
        }

        let mut read_codec = create_ndx_codec(protocol_version);
        let mut cursor = Cursor::new(&buf);

        for (i, &expected) in values.iter().enumerate() {
            let decoded = read_codec.read_ndx(&mut cursor)?;
            prop_assert_eq!(
                decoded, expected,
                "NDX sequence roundtrip failed at index {} for value={}, protocol={}",
                i, expected, protocol_version
            );
        }

        prop_assert_eq!(
            cursor.position() as usize,
            buf.len(),
            "Not all bytes consumed"
        );
    }

    /// Sequential monotonically increasing indices should be compact in modern protocol.
    #[test]
    fn ndx_sequential_indices_are_compact(
        start in 0i32..1000,
        count in 1usize..100
    ) {
        let values: Vec<i32> = (start..).take(count).collect();

        let mut legacy_codec = create_ndx_codec(29);
        let mut modern_codec = create_ndx_codec(30);

        let mut legacy_buf = Vec::new();
        let mut modern_buf = Vec::new();

        for &v in &values {
            legacy_codec.write_ndx(&mut legacy_buf, v)?;
        }

        for &v in &values {
            modern_codec.write_ndx(&mut modern_buf, v)?;
        }

        // Legacy always uses 4 bytes per value
        prop_assert_eq!(legacy_buf.len(), count * 4);

        // Modern uses delta encoding, so sequential values are 1 byte each
        // (after the first value which depends on the initial state)
        prop_assert!(
            modern_buf.len() < legacy_buf.len(),
            "Modern encoding should be more compact for sequential values"
        );

        // Verify both roundtrip correctly
        let mut read_legacy = create_ndx_codec(29);
        let mut cursor = Cursor::new(&legacy_buf);
        for &expected in &values {
            let decoded = read_legacy.read_ndx(&mut cursor)?;
            prop_assert_eq!(decoded, expected);
        }

        let mut read_modern = create_ndx_codec(30);
        let mut cursor = Cursor::new(&modern_buf);
        for &expected in &values {
            let decoded = read_modern.read_ndx(&mut cursor)?;
            prop_assert_eq!(decoded, expected);
        }
    }
}

// ============================================================================
// Message Frame Roundtrip Tests
// ============================================================================

/// Strategy for generating arbitrary message payloads.
fn payload_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..4096)
}

/// Strategy for generating valid message codes.
fn message_code_strategy() -> impl Strategy<Value = MessageCode> {
    prop::sample::select(MessageCode::ALL.to_vec())
}

proptest! {
    /// Message frames should roundtrip through encode/decode.
    #[test]
    fn message_frame_roundtrips(
        code in message_code_strategy(),
        payload in payload_strategy()
    ) {
        let frame = MessageFrame::new(code, payload.clone())?;

        // Encode
        let mut encoded = Vec::new();
        frame.encode_into_vec(&mut encoded)?;

        // Decode
        let (decoded, remainder) = MessageFrame::decode_from_slice(&encoded)?;

        prop_assert!(remainder.is_empty(), "No trailing bytes expected");
        prop_assert_eq!(decoded.code(), code);
        prop_assert_eq!(decoded.payload(), payload.as_slice());
    }

    /// Message header roundtrips through encode/decode.
    #[test]
    fn message_header_roundtrips(
        code in message_code_strategy(),
        // Maximum payload length is 0x00FFFFFF (24 bits)
        payload_len in 0u32..=0x00FFFFFFu32
    ) {
        let header = MessageHeader::new(code, payload_len)?;

        // Encode
        let encoded = header.encode();
        prop_assert_eq!(encoded.len(), 4, "Header is always 4 bytes");

        // Decode
        let decoded = MessageHeader::decode(&encoded)?;

        prop_assert_eq!(decoded.code(), code);
        prop_assert_eq!(decoded.payload_len(), payload_len);
    }

    /// Sequence of message frames should roundtrip.
    #[test]
    fn message_frame_sequence_roundtrips(
        frames in prop::collection::vec(
            (message_code_strategy(), prop::collection::vec(any::<u8>(), 0..256)),
            1..8
        )
    ) {
        let mut encoded = Vec::new();
        let mut original_frames = Vec::new();

        for (code, payload) in &frames {
            let frame = MessageFrame::new(*code, payload.clone())?;
            frame.encode_into_vec(&mut encoded)?;
            original_frames.push(frame);
        }

        // Decode all frames
        let mut remaining = encoded.as_slice();
        for original in &original_frames {
            let (decoded, rest) = MessageFrame::decode_from_slice(remaining)?;
            prop_assert_eq!(decoded.code(), original.code());
            prop_assert_eq!(decoded.payload(), original.payload());
            remaining = rest;
        }

        prop_assert!(remaining.is_empty());
    }
}

// ============================================================================
// Protocol Codec Roundtrip Tests
// ============================================================================

proptest! {
    /// File size encoding should roundtrip for all protocol versions.
    #[test]
    fn protocol_codec_file_size_roundtrips(
        // Limit to values that work with both legacy and modern encoding
        size in 0i64..=0xFFFF_FFFF_FFFFi64,
        protocol_version in prop::sample::select(vec![28u8, 29, 30, 31, 32])
    ) {
        let codec = create_protocol_codec(protocol_version);
        let mut buf = Vec::new();
        codec.write_file_size(&mut buf, size)?;

        let mut cursor = Cursor::new(&buf);
        let decoded = codec.read_file_size(&mut cursor)?;

        prop_assert_eq!(decoded, size, "File size roundtrip failed for size={}, protocol={}", size, protocol_version);
    }

    /// Modification time encoding should roundtrip for all protocol versions.
    #[test]
    fn protocol_codec_mtime_roundtrips(
        // Legacy protocol truncates to 32-bit, so limit the range
        mtime in 0i64..=i32::MAX as i64,
        protocol_version in prop::sample::select(vec![28u8, 29, 30, 31, 32])
    ) {
        let codec = create_protocol_codec(protocol_version);
        let mut buf = Vec::new();
        codec.write_mtime(&mut buf, mtime)?;

        let mut cursor = Cursor::new(&buf);
        let decoded = codec.read_mtime(&mut cursor)?;

        prop_assert_eq!(decoded, mtime, "Mtime roundtrip failed for mtime={}, protocol={}", mtime, protocol_version);
    }

    /// Long name length encoding should roundtrip for all protocol versions.
    #[test]
    fn protocol_codec_long_name_len_roundtrips(
        // Reasonable filename length range
        len in 0usize..=0x7FFFFFusize,
        protocol_version in prop::sample::select(vec![28u8, 29, 30, 31, 32])
    ) {
        let codec = create_protocol_codec(protocol_version);
        let mut buf = Vec::new();
        codec.write_long_name_len(&mut buf, len)?;

        let mut cursor = Cursor::new(&buf);
        let decoded = codec.read_long_name_len(&mut cursor)?;

        prop_assert_eq!(decoded, len, "Long name len roundtrip failed for len={}, protocol={}", len, protocol_version);
    }

    /// Fixed integer encoding should roundtrip for all protocol versions.
    #[test]
    fn protocol_codec_int_roundtrips(
        value in any::<i32>(),
        protocol_version in prop::sample::select(vec![28u8, 29, 30, 31, 32])
    ) {
        let codec = create_protocol_codec(protocol_version);
        let mut buf = Vec::new();
        codec.write_int(&mut buf, value)?;

        prop_assert_eq!(buf.len(), 4, "Fixed int always 4 bytes");

        let mut cursor = Cursor::new(&buf);
        let decoded = codec.read_int(&mut cursor)?;

        prop_assert_eq!(decoded, value);
    }

    /// Varint encoding via protocol codec should roundtrip for all protocol versions.
    #[test]
    fn protocol_codec_varint_roundtrips(
        value in any::<i32>(),
        protocol_version in prop::sample::select(vec![28u8, 29, 30, 31, 32])
    ) {
        let codec = create_protocol_codec(protocol_version);
        let mut buf = Vec::new();
        codec.write_varint(&mut buf, value)?;

        let mut cursor = Cursor::new(&buf);
        let decoded = codec.read_varint(&mut cursor)?;

        prop_assert_eq!(decoded, value);
    }

    /// Statistics encoding should roundtrip (uses same encoding as file size).
    #[test]
    fn protocol_codec_stat_roundtrips(
        value in 0i64..=0xFFFF_FFFF_FFFFi64,
        protocol_version in prop::sample::select(vec![28u8, 29, 30, 31, 32])
    ) {
        let codec = create_protocol_codec(protocol_version);
        let mut buf = Vec::new();
        codec.write_stat(&mut buf, value)?;

        let mut cursor = Cursor::new(&buf);
        let decoded = codec.read_stat(&mut cursor)?;

        prop_assert_eq!(decoded, value);
    }
}

// ============================================================================
// Cross-Protocol Consistency Tests
// ============================================================================

proptest! {
    /// Different codec instances with the same version should produce identical output.
    #[test]
    fn codec_instances_are_consistent(
        value in any::<i32>(),
        protocol_version in prop::sample::select(vec![28u8, 29, 30, 31, 32])
    ) {
        let codec1 = create_protocol_codec(protocol_version);
        let codec2 = create_protocol_codec(protocol_version);

        let mut buf1 = Vec::new();
        let mut buf2 = Vec::new();

        codec1.write_varint(&mut buf1, value)?;
        codec2.write_varint(&mut buf2, value)?;

        prop_assert_eq!(buf1, buf2, "Same version should produce identical output");
    }

    /// NDX codec instances should maintain independent state.
    #[test]
    fn ndx_codec_instances_have_independent_state(
        values1 in prop::collection::vec(0i32..1000, 1..10),
        values2 in prop::collection::vec(0i32..1000, 1..10)
    ) {
        let mut codec1 = create_ndx_codec(30);
        let mut codec2 = create_ndx_codec(30);

        let mut buf1 = Vec::new();
        let mut buf2 = Vec::new();

        for &v in &values1 {
            codec1.write_ndx(&mut buf1, v)?;
        }

        for &v in &values2 {
            codec2.write_ndx(&mut buf2, v)?;
        }

        // Verify each codec's output decodes correctly with fresh codec
        let mut read_codec1 = create_ndx_codec(30);
        let mut cursor1 = Cursor::new(&buf1);
        for &expected in &values1 {
            let decoded = read_codec1.read_ndx(&mut cursor1)?;
            prop_assert_eq!(decoded, expected);
        }

        let mut read_codec2 = create_ndx_codec(30);
        let mut cursor2 = Cursor::new(&buf2);
        for &expected in &values2 {
            let decoded = read_codec2.read_ndx(&mut cursor2)?;
            prop_assert_eq!(decoded, expected);
        }
    }
}

// ============================================================================
// Encoding Size Property Tests
// ============================================================================

proptest! {
    /// Varint encoding size should be bounded.
    #[test]
    fn varint_encoding_size_is_bounded(value in any::<i32>()) {
        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);

        // Varint uses at most 5 bytes for i32
        prop_assert!(encoded.len() <= 5, "Varint encoding too large: {} bytes", encoded.len());
        prop_assert!(!encoded.is_empty(), "Varint encoding too small");
    }

    /// Legacy file size encoding should use 4 or 12 bytes.
    #[test]
    fn legacy_file_size_encoding_size(size in 0i64..=i64::MAX) {
        let codec = create_protocol_codec(29);
        let mut buf = Vec::new();
        codec.write_file_size(&mut buf, size)?;

        if size <= 0x7FFF_FFFF {
            prop_assert_eq!(buf.len(), 4, "Small values use 4 bytes");
        } else {
            prop_assert_eq!(buf.len(), 12, "Large values use 12 bytes");
        }
    }

    /// Legacy NDX encoding always uses 4 bytes.
    #[test]
    fn legacy_ndx_encoding_always_4_bytes(value in ndx_value_strategy()) {
        let mut codec = create_ndx_codec(29);
        let mut buf = Vec::new();
        codec.write_ndx(&mut buf, value)?;

        prop_assert_eq!(buf.len(), 4, "Legacy NDX always 4 bytes");
    }

    /// Message header is always 4 bytes.
    #[test]
    fn message_header_always_4_bytes(
        code in message_code_strategy(),
        payload_len in 0u32..=0x00FFFFFFu32
    ) {
        let header = MessageHeader::new(code, payload_len)?;
        let encoded = header.encode();

        prop_assert_eq!(encoded.len(), 4);
    }
}

// ============================================================================
// Boundary Value Tests
// ============================================================================

proptest! {
    /// Values at byte boundaries should roundtrip correctly.
    #[test]
    fn varint_byte_boundary_values_roundtrip(
        shift in 0u32..31u32
    ) {
        let value = 1i32 << shift;
        let value_minus_1 = value - 1;

        // Test power of 2
        let mut enc = Vec::new();
        encode_varint_to_vec(value, &mut enc);
        let (decoded, _) = decode_varint(&enc)?;
        prop_assert_eq!(decoded, value);

        // Test power of 2 minus 1
        let mut enc = Vec::new();
        encode_varint_to_vec(value_minus_1, &mut enc);
        let (decoded, _) = decode_varint(&enc)?;
        prop_assert_eq!(decoded, value_minus_1);
    }

    /// NDX boundary values should roundtrip correctly.
    ///
    /// Note: The modern NDX codec uses delta encoding with an initial state of -1,
    /// which limits the maximum first value that can be encoded without overflow.
    /// Values up to 16 million are typically safe for file list indices.
    #[test]
    fn ndx_boundary_values_roundtrip(
        protocol_version in prop::sample::select(vec![28u8, 29, 30, 31, 32])
    ) {
        // Values that are safe for both legacy and modern NDX codecs
        // The modern codec's delta encoding starts from -1, so very large
        // values on the first write can cause overflow issues.
        let boundary_values = [
            0, 1, 127, 128, 253, 254, 255, 256,
            32767, 32768, 65535, 65536,
            0x00FF_FFFF, // 16 million - reasonable file list size
        ];

        for &value in &boundary_values {
            let mut write_codec = create_ndx_codec(protocol_version);
            let mut buf = Vec::new();
            write_codec.write_ndx(&mut buf, value)?;

            let mut read_codec = create_ndx_codec(protocol_version);
            let mut cursor = Cursor::new(&buf);
            let decoded = read_codec.read_ndx(&mut cursor)?;

            prop_assert_eq!(decoded, value, "Boundary value {} failed for protocol {}", value, protocol_version);
        }
    }

    /// Legacy NDX codec handles full i32 range (4-byte fixed encoding).
    #[test]
    fn ndx_legacy_handles_full_i32_range(value in any::<i32>()) {
        let mut write_codec = create_ndx_codec(29);
        let mut buf = Vec::new();
        write_codec.write_ndx(&mut buf, value)?;

        prop_assert_eq!(buf.len(), 4, "Legacy NDX always 4 bytes");

        let mut read_codec = create_ndx_codec(29);
        let mut cursor = Cursor::new(&buf);
        let decoded = read_codec.read_ndx(&mut cursor)?;

        prop_assert_eq!(decoded, value);
    }
}

// ============================================================================
// Mixed Type Sequence Tests
// ============================================================================

proptest! {
    /// Interleaved varint and fixed int values should roundtrip correctly.
    #[test]
    fn interleaved_varint_and_fixed_int_roundtrip(
        varints in prop::collection::vec(any::<i32>(), 1..8),
        fixed_ints in prop::collection::vec(any::<i32>(), 1..8)
    ) {
        let mut buf = Vec::new();

        // Interleave writes
        for (&v, &f) in varints.iter().zip(fixed_ints.iter()) {
            write_varint(&mut buf, v)?;
            write_int(&mut buf, f)?;
        }

        // Read back interleaved
        let mut cursor = Cursor::new(&buf);
        for (&expected_v, &expected_f) in varints.iter().zip(fixed_ints.iter()) {
            let decoded_v = read_varint(&mut cursor)?;
            prop_assert_eq!(decoded_v, expected_v);

            let decoded_f = read_int(&mut cursor)?;
            prop_assert_eq!(decoded_f, expected_f);
        }
    }

    /// Mixed protocol codec operations should roundtrip correctly.
    #[test]
    fn mixed_protocol_codec_operations_roundtrip(
        file_size in 0i64..=0xFFFF_FFFFi64,
        mtime in 0i64..=i32::MAX as i64,
        name_len in 0usize..=0xFFFFusize,
        protocol_version in prop::sample::select(vec![28u8, 29, 30, 31, 32])
    ) {
        let codec = create_protocol_codec(protocol_version);
        let mut buf = Vec::new();

        codec.write_file_size(&mut buf, file_size)?;
        codec.write_mtime(&mut buf, mtime)?;
        codec.write_long_name_len(&mut buf, name_len)?;

        let mut cursor = Cursor::new(&buf);
        let decoded_size = codec.read_file_size(&mut cursor)?;
        let decoded_mtime = codec.read_mtime(&mut cursor)?;
        let decoded_len = codec.read_long_name_len(&mut cursor)?;

        prop_assert_eq!(decoded_size, file_size);
        prop_assert_eq!(decoded_mtime, mtime);
        prop_assert_eq!(decoded_len, name_len);
    }
}
