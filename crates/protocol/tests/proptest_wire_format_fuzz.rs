//! Proptest-based fuzz tests for protocol wire format parsing.
//!
//! These tests use proptest's property-based testing to feed arbitrary byte
//! sequences to protocol decoders, verifying that:
//!
//! - Decoders never panic on arbitrary/malformed input
//! - Encode-then-decode roundtrips preserve the original value
//! - Truncated or corrupted data yields errors rather than panics
//! - Edge cases (empty input, maximum/minimum values) are handled gracefully
//!
//! This complements the existing `wire_format_fuzz.rs` (manual random bytes)
//! and `proptest_codec_roundtrips.rs` (roundtrip-focused) with true
//! proptest-driven arbitrary-input coverage over security-critical decode paths.

use proptest::prelude::*;
use protocol::codec::{
    LegacyNdxCodec, ModernNdxCodec, NdxCodec, NdxState, ProtocolCodec, create_ndx_codec,
    create_protocol_codec,
};
use protocol::wire::file_entry::{XMIT_EXTENDED_FLAGS, XMIT_LONG_NAME, XMIT_SAME_MODE};
use protocol::wire::file_entry_decode::{
    decode_atime, decode_checksum, decode_crtime, decode_flags, decode_gid, decode_hardlink_dev_ino,
    decode_hardlink_idx, decode_mode, decode_mtime, decode_mtime_nsec, decode_name, decode_rdev,
    decode_size, decode_symlink_target, decode_uid,
};
use protocol::wire::{
    DeltaOp, SignatureBlock, read_delta, read_delta_op, read_signature, read_token, write_delta,
    write_delta_op, write_signature, write_token_block_match, write_whole_file_delta,
};
use protocol::{
    MessageCode, MessageHeader, ProtocolVersion, decode_varint, encode_varint_to_vec, read_int,
    read_longint, read_varint, read_varlong, write_longint,
};
use std::io::Cursor;

// ============================================================================
// Strategy helpers
// ============================================================================

/// Generates arbitrary byte vectors of bounded length for fuzz testing.
fn arbitrary_bytes(max_len: usize) -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..=max_len)
}

/// Generates supported protocol version numbers.
fn protocol_version_strategy() -> impl Strategy<Value = u8> {
    prop::sample::select(vec![28u8, 29, 30, 31, 32])
}

// ============================================================================
// Module: Varint/Varlong arbitrary-input fuzz
// ============================================================================

mod varint_arbitrary_input {
    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1000))]

        /// read_varint must not panic on any byte sequence.
        #[test]
        fn read_varint_never_panics(data in arbitrary_bytes(32)) {
            let mut cursor = Cursor::new(&data[..]);
            let _ = read_varint(&mut cursor);
        }

        /// decode_varint (slice-based) must not panic on any byte sequence.
        #[test]
        fn decode_varint_never_panics(data in arbitrary_bytes(32)) {
            let _ = decode_varint(&data);
        }

        /// read_varlong must not panic on any byte sequence with any min_bytes.
        #[test]
        fn read_varlong_never_panics(
            data in arbitrary_bytes(32),
            min_bytes in 1u8..=8u8
        ) {
            let mut cursor = Cursor::new(&data[..]);
            let _ = read_varlong(&mut cursor, min_bytes);
        }

        /// read_int must not panic on any byte sequence.
        #[test]
        fn read_int_never_panics(data in arbitrary_bytes(16)) {
            let mut cursor = Cursor::new(&data[..]);
            let _ = read_int(&mut cursor);
        }

        /// read_longint must not panic on any byte sequence.
        #[test]
        fn read_longint_never_panics(data in arbitrary_bytes(32)) {
            let mut cursor = Cursor::new(&data[..]);
            let _ = read_longint(&mut cursor);
        }

        /// Varint decode must return error for empty input.
        #[test]
        fn varint_empty_input_is_error(_dummy in Just(())) {
            let result = decode_varint(&[]);
            prop_assert!(result.is_err());
            prop_assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof);
        }

        /// read_longint roundtrip for values in the 4-byte inline range.
        #[test]
        fn longint_inline_roundtrip(value in 0i64..=0x7FFF_FFFFi64) {
            let mut buf = Vec::new();
            write_longint(&mut buf, value).unwrap();
            prop_assert_eq!(buf.len(), 4);

            let mut cursor = Cursor::new(&buf);
            let decoded = read_longint(&mut cursor).unwrap();
            prop_assert_eq!(decoded, value);
        }

        /// read_longint roundtrip for the extended 12-byte format.
        #[test]
        fn longint_extended_roundtrip(value in (0x8000_0000i64..=i64::MAX).prop_union(i64::MIN..=(-1i64))) {
            let mut buf = Vec::new();
            write_longint(&mut buf, value).unwrap();
            prop_assert_eq!(buf.len(), 12);

            let mut cursor = Cursor::new(&buf);
            let decoded = read_longint(&mut cursor).unwrap();
            prop_assert_eq!(decoded, value);
        }

        /// Sequential varint decoding from arbitrary bytes must not panic.
        #[test]
        fn sequential_varint_decode_no_panic(data in arbitrary_bytes(256)) {
            let mut cursor = Cursor::new(&data[..]);
            for _ in 0..50 {
                if cursor.position() as usize >= data.len() {
                    break;
                }
                let _ = read_varint(&mut cursor);
            }
        }

        /// Sequential varlong decoding from arbitrary bytes must not panic.
        #[test]
        fn sequential_varlong_decode_no_panic(data in arbitrary_bytes(256)) {
            let mut cursor = Cursor::new(&data[..]);
            for min_bytes in [3u8, 4] {
                cursor.set_position(0);
                for _ in 0..20 {
                    if cursor.position() as usize >= data.len() {
                        break;
                    }
                    let _ = read_varlong(&mut cursor, min_bytes);
                }
            }
        }
    }
}

// ============================================================================
// Module: Envelope / MessageHeader arbitrary-input fuzz
// ============================================================================

mod envelope_arbitrary_input {
    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        /// MessageHeader::decode must not panic on any byte slice.
        #[test]
        fn message_header_decode_never_panics(data in arbitrary_bytes(16)) {
            let _ = MessageHeader::decode(&data);
        }

        /// MessageHeader::from_raw must not panic on any u32.
        #[test]
        fn message_header_from_raw_never_panics(raw in any::<u32>()) {
            let _ = MessageHeader::from_raw(raw);
        }

        /// Truncated header (< 4 bytes) must return TruncatedHeader error.
        #[test]
        fn message_header_truncated_is_error(len in 0usize..4) {
            let data = vec![0xFFu8; len];
            let result = MessageHeader::decode(&data);
            prop_assert!(result.is_err());
        }

        /// Raw values with tag < MPLEX_BASE (7) must return InvalidTag error.
        #[test]
        fn message_header_low_tag_is_error(tag in 0u8..7u8, low24 in 0u32..0x00FF_FFFFu32) {
            let raw = ((tag as u32) << 24) | low24;
            let result = MessageHeader::from_raw(raw);
            prop_assert!(result.is_err());
        }

        /// Encode-then-decode roundtrip for all valid message code + payload combinations.
        #[test]
        fn message_header_roundtrip(
            code_idx in 0usize..18usize,
            payload_len in 0u32..=0x00FF_FFFFu32,
        ) {
            let code = MessageCode::ALL[code_idx];
            let header = MessageHeader::new(code, payload_len).unwrap();
            let encoded = header.encode();
            let decoded = MessageHeader::decode(&encoded).unwrap();
            prop_assert_eq!(decoded.code(), code);
            prop_assert_eq!(decoded.payload_len(), payload_len);
        }

        /// MessageCode::from_u8 must not panic for any u8 value.
        #[test]
        fn message_code_from_u8_never_panics(value in any::<u8>()) {
            let _ = MessageCode::from_u8(value);
        }

        /// MessageCode::from_u8 roundtrip for all known codes.
        #[test]
        fn message_code_roundtrip(code_idx in 0usize..18usize) {
            let code = MessageCode::ALL[code_idx];
            let value = code.as_u8();
            let decoded = MessageCode::from_u8(value);
            prop_assert_eq!(decoded, Some(code));
        }
    }
}

// ============================================================================
// Module: File entry decode arbitrary-input fuzz
// ============================================================================

mod file_entry_decode_arbitrary_input {
    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(500))]

        /// decode_flags must not panic on arbitrary bytes for any protocol version.
        #[test]
        fn decode_flags_never_panics(
            data in arbitrary_bytes(16),
            protocol_version in protocol_version_strategy(),
            use_varint in any::<bool>(),
        ) {
            let mut cursor = Cursor::new(&data[..]);
            let _ = decode_flags(&mut cursor, protocol_version, use_varint);
        }

        /// decode_name must not panic on arbitrary bytes.
        ///
        /// Note: decode_name reads a suffix_len and allocates a buffer. With
        /// XMIT_LONG_NAME set, it reads a varint/int for the length which can
        /// decode to billions. We mask out XMIT_LONG_NAME to keep the suffix_len
        /// as a single u8 (max 255), preventing huge allocations.
        #[test]
        fn decode_name_never_panics(
            data in arbitrary_bytes(32),
            raw_flags in any::<u32>(),
            prev_name in arbitrary_bytes(16),
            protocol_version in protocol_version_strategy(),
        ) {
            // Clear XMIT_LONG_NAME bit to prevent varint-sized allocation
            let flags = raw_flags & !(XMIT_LONG_NAME as u32);
            let mut cursor = Cursor::new(&data[..]);
            let _ = decode_name(&mut cursor, flags, &prev_name, protocol_version);
        }

        /// decode_size must not panic on arbitrary bytes.
        #[test]
        fn decode_size_never_panics(
            data in arbitrary_bytes(32),
            protocol_version in protocol_version_strategy(),
        ) {
            let mut cursor = Cursor::new(&data[..]);
            let _ = decode_size(&mut cursor, protocol_version);
        }

        /// decode_mtime must not panic on arbitrary bytes.
        #[test]
        fn decode_mtime_never_panics(
            data in arbitrary_bytes(32),
            flags in any::<u32>(),
            prev_mtime in any::<i64>(),
            protocol_version in protocol_version_strategy(),
        ) {
            let mut cursor = Cursor::new(&data[..]);
            let _ = decode_mtime(&mut cursor, flags, prev_mtime, protocol_version);
        }

        /// decode_mtime_nsec must not panic on arbitrary bytes.
        #[test]
        fn decode_mtime_nsec_never_panics(
            data in arbitrary_bytes(16),
            flags in any::<u32>(),
        ) {
            let mut cursor = Cursor::new(&data[..]);
            let _ = decode_mtime_nsec(&mut cursor, flags);
        }

        /// decode_atime must not panic on arbitrary bytes.
        #[test]
        fn decode_atime_never_panics(
            data in arbitrary_bytes(32),
            flags in any::<u32>(),
            prev_atime in any::<i64>(),
        ) {
            let mut cursor = Cursor::new(&data[..]);
            let _ = decode_atime(&mut cursor, flags, prev_atime);
        }

        /// decode_crtime must not panic on arbitrary bytes.
        #[test]
        fn decode_crtime_never_panics(
            data in arbitrary_bytes(32),
            flags in any::<u32>(),
            mtime in any::<i64>(),
        ) {
            let mut cursor = Cursor::new(&data[..]);
            let _ = decode_crtime(&mut cursor, flags, mtime);
        }

        /// decode_mode must not panic on arbitrary bytes.
        #[test]
        fn decode_mode_never_panics(
            data in arbitrary_bytes(16),
            flags in any::<u32>(),
            prev_mode in any::<u32>(),
        ) {
            let mut cursor = Cursor::new(&data[..]);
            let _ = decode_mode(&mut cursor, flags, prev_mode);
        }

        /// decode_uid must not panic on arbitrary bytes.
        #[test]
        fn decode_uid_never_panics(
            data in arbitrary_bytes(64),
            flags in any::<u32>(),
            prev_uid in any::<u32>(),
            protocol_version in protocol_version_strategy(),
        ) {
            let mut cursor = Cursor::new(&data[..]);
            let _ = decode_uid(&mut cursor, flags, prev_uid, protocol_version);
        }

        /// decode_gid must not panic on arbitrary bytes.
        #[test]
        fn decode_gid_never_panics(
            data in arbitrary_bytes(64),
            flags in any::<u32>(),
            prev_gid in any::<u32>(),
            protocol_version in protocol_version_strategy(),
        ) {
            let mut cursor = Cursor::new(&data[..]);
            let _ = decode_gid(&mut cursor, flags, prev_gid, protocol_version);
        }

        /// decode_rdev must not panic on arbitrary bytes.
        #[test]
        fn decode_rdev_never_panics(
            data in arbitrary_bytes(32),
            flags in any::<u32>(),
            prev_rdev_major in any::<u32>(),
            protocol_version in protocol_version_strategy(),
        ) {
            let mut cursor = Cursor::new(&data[..]);
            let _ = decode_rdev(&mut cursor, flags, prev_rdev_major, protocol_version);
        }

        /// decode_symlink_target must not panic on arbitrary bytes.
        ///
        /// Note: decode_symlink_target reads a varint30/int length and allocates,
        /// so we prepend a small length value to avoid multi-GB allocations.
        /// For protocol < 30 it reads a 4-byte LE int; for >= 30 a varint.
        #[test]
        fn decode_symlink_target_never_panics(
            small_len in 0u8..64,
            trailing in arbitrary_bytes(32),
            protocol_version in protocol_version_strategy(),
        ) {
            let mut input = Vec::new();
            if protocol_version < 30 {
                // read_int: 4-byte LE i32
                input.extend_from_slice(&(small_len as i32).to_le_bytes());
            } else {
                // read_varint: single byte with value < 0x80 encodes directly
                input.push(small_len & 0x3F); // keep it small (0..63)
            }
            input.extend_from_slice(&trailing);
            let mut cursor = Cursor::new(&input[..]);
            let _ = decode_symlink_target(&mut cursor, protocol_version);
        }

        /// decode_hardlink_idx must not panic on arbitrary bytes.
        #[test]
        fn decode_hardlink_idx_never_panics(
            data in arbitrary_bytes(16),
            flags in any::<u32>(),
        ) {
            let mut cursor = Cursor::new(&data[..]);
            let _ = decode_hardlink_idx(&mut cursor, flags);
        }

        /// decode_hardlink_dev_ino must not panic on arbitrary bytes.
        #[test]
        fn decode_hardlink_dev_ino_never_panics(
            data in arbitrary_bytes(32),
            flags in any::<u32>(),
            prev_dev in any::<i64>(),
        ) {
            let mut cursor = Cursor::new(&data[..]);
            let _ = decode_hardlink_dev_ino(&mut cursor, flags, prev_dev);
        }

        /// decode_checksum must not panic on arbitrary bytes with any checksum length.
        #[test]
        fn decode_checksum_never_panics(
            data in arbitrary_bytes(64),
            checksum_len in 0usize..=32,
        ) {
            let mut cursor = Cursor::new(&data[..]);
            let _ = decode_checksum(&mut cursor, checksum_len);
        }

        /// decode_flags returns error on empty input (non-varint mode).
        #[test]
        fn decode_flags_empty_is_error(
            protocol_version in protocol_version_strategy(),
        ) {
            let mut cursor = Cursor::new(&[][..]);
            let result = decode_flags(&mut cursor, protocol_version, false);
            prop_assert!(result.is_err());
        }

        /// decode_size returns error on empty input.
        #[test]
        fn decode_size_empty_is_error(
            protocol_version in protocol_version_strategy(),
        ) {
            let mut cursor = Cursor::new(&[][..]);
            let result = decode_size(&mut cursor, protocol_version);
            prop_assert!(result.is_err());
        }
    }
}

// ============================================================================
// Module: File entry encode-then-decode roundtrip via proptest
// ============================================================================

mod file_entry_roundtrip {
    use super::*;
    use protocol::wire::file_entry::{encode_flags, encode_mode, encode_name, encode_size};

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(500))]

        /// encode_flags -> decode_flags roundtrip for protocol 28+ (non-varint).
        ///
        /// Note: encode_flags only sets XMIT_EXTENDED_FLAGS when high bits are
        /// present or flags == 0. If the caller passes flags with XMIT_EXTENDED_FLAGS
        /// already set but no high bits, encode writes 1 byte but decode expects 2.
        /// We avoid this by masking out XMIT_EXTENDED_FLAGS from the low byte
        /// when there are no high bits - matching how the encoder uses the flag.
        #[test]
        fn flags_roundtrip_non_varint(
            raw_flags in 0u32..=0xFFFFu32,
        ) {
            let protocol_version = 32u8;
            let is_end = false;
            let use_varint_flags = false;
            let is_dir = false;

            // Ensure we don't accidentally create an end marker (flags == 0 byte)
            let mut flags = if raw_flags == 0 { 1 } else { raw_flags };

            // When no high bits are set, the encoder only writes 1 byte and never
            // sets XMIT_EXTENDED_FLAGS itself. If we pass it with the bit already
            // set, the single-byte write confuses the decoder. Clear the bit to
            // match the encoder's own behavior.
            if flags & 0xFF00 == 0 {
                flags &= !(XMIT_EXTENDED_FLAGS as u32);
                if flags == 0 { flags = 1; }
            }

            let mut buf = Vec::new();
            encode_flags(&mut buf, flags, protocol_version, use_varint_flags, is_dir).unwrap();

            let mut cursor = Cursor::new(&buf);
            let (decoded_flags, decoded_is_end) =
                decode_flags(&mut cursor, protocol_version, use_varint_flags).unwrap();

            prop_assert_eq!(decoded_is_end, is_end);
            // For single-byte flags without extended, the low byte should match
            if flags <= 0xFF && flags & (XMIT_EXTENDED_FLAGS as u32) == 0 {
                prop_assert_eq!(decoded_flags, flags & 0xFF);
            }
        }

        /// encode_size -> decode_size roundtrip for various protocol versions.
        #[test]
        fn size_roundtrip(
            size in 0u64..=0xFFFF_FFFF_FFFFu64,
            protocol_version in protocol_version_strategy(),
        ) {
            let mut buf = Vec::new();
            encode_size(&mut buf, size, protocol_version).unwrap();

            let mut cursor = Cursor::new(&buf);
            let decoded = decode_size(&mut cursor, protocol_version).unwrap();
            prop_assert_eq!(decoded, size as i64);
        }

        /// encode_mode -> decode_mode roundtrip.
        #[test]
        fn mode_roundtrip(mode in any::<u32>()) {
            let mut buf = Vec::new();
            encode_mode(&mut buf, mode).unwrap();

            let mut cursor = Cursor::new(&buf);
            // flags=0 means XMIT_SAME_MODE is not set, so mode will be decoded
            let decoded = decode_mode(&mut cursor, 0, 0).unwrap();
            prop_assert_eq!(decoded, Some(mode));
        }

        /// decode_mode with XMIT_SAME_MODE returns previous mode.
        #[test]
        fn mode_same_flag_returns_previous(prev_mode in any::<u32>()) {
            let flags = XMIT_SAME_MODE as u32;
            let mut cursor = Cursor::new(&[][..]);
            let decoded = decode_mode(&mut cursor, flags, prev_mode).unwrap();
            prop_assert_eq!(decoded, Some(prev_mode));
        }

        /// encode_name -> decode_name roundtrip for short names (no prefix sharing).
        #[test]
        fn name_roundtrip_no_prefix(
            name_bytes in prop::collection::vec(1u8..=127u8, 1..=200),
            protocol_version in protocol_version_strategy(),
        ) {
            let flags = 0u32; // No XMIT_SAME_NAME, no XMIT_LONG_NAME
            let xflags = flags;

            let mut buf = Vec::new();
            let prefix_len = 0;
            encode_name(&mut buf, &name_bytes, prefix_len, xflags, protocol_version).unwrap();

            let mut cursor = Cursor::new(&buf);
            let decoded = decode_name(&mut cursor, flags, b"", protocol_version).unwrap();
            prop_assert_eq!(decoded, name_bytes);
        }
    }
}

// ============================================================================
// Module: Signature wire format arbitrary-input fuzz
// ============================================================================

mod signature_arbitrary_input {
    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(500))]

        /// read_signature must not panic on arbitrary bytes.
        ///
        /// The first varint is interpreted as block_count and used for allocation.
        /// We prepend a small block_count (0..64) encoded as a single-byte varint,
        /// then append arbitrary trailing bytes to fuzz the remaining fields.
        #[test]
        fn read_signature_never_panics(
            block_count in 0u8..64,
            trailing in arbitrary_bytes(32),
        ) {
            let mut data = vec![block_count];
            data.extend_from_slice(&trailing);
            let mut cursor = Cursor::new(&data[..]);
            let _ = read_signature(&mut cursor);
        }

        /// Signature encode-then-decode roundtrip.
        #[test]
        fn signature_roundtrip(
            block_count in 0u32..=8,
            block_length in 1u32..=65536,
            strong_sum_length in 1u8..=32,
            rolling_sums in prop::collection::vec(any::<u32>(), 0..=8),
        ) {
            // Align block count with actual blocks
            let actual_count = block_count.min(rolling_sums.len() as u32);
            let blocks: Vec<SignatureBlock> = rolling_sums.iter().take(actual_count as usize)
                .enumerate()
                .map(|(i, &rolling_sum)| SignatureBlock {
                    index: i as u32,
                    rolling_sum,
                    strong_sum: vec![0xAA; strong_sum_length as usize],
                })
                .collect();

            let mut buf = Vec::new();
            write_signature(&mut buf, actual_count, block_length, strong_sum_length, &blocks).unwrap();

            let mut cursor = Cursor::new(&buf);
            let (decoded_bl, decoded_bc, decoded_ssl, decoded_blocks) =
                read_signature(&mut cursor).unwrap();

            prop_assert_eq!(decoded_bc, actual_count);
            prop_assert_eq!(decoded_bl, block_length);
            prop_assert_eq!(decoded_ssl, strong_sum_length);
            prop_assert_eq!(decoded_blocks.len(), actual_count as usize);

            for (i, block) in decoded_blocks.iter().enumerate() {
                prop_assert_eq!(block.rolling_sum, blocks[i].rolling_sum);
                prop_assert_eq!(&block.strong_sum, &blocks[i].strong_sum);
            }
        }

        /// read_signature with empty input returns error.
        #[test]
        fn read_signature_empty_is_error(_dummy in Just(())) {
            let mut cursor = Cursor::new(&[][..]);
            let result = read_signature(&mut cursor);
            prop_assert!(result.is_err());
        }
    }
}

// ============================================================================
// Module: Delta wire format arbitrary-input fuzz
// ============================================================================

mod delta_arbitrary_input {
    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(500))]

        /// read_delta_op must not panic on arbitrary bytes.
        ///
        /// For literal ops (opcode 0x00), the varint-decoded length is used
        /// for allocation. We constrain the leading byte to either a non-literal
        /// opcode or a literal with a small length varint following.
        #[test]
        fn read_delta_op_never_panics(
            opcode in any::<u8>(),
            trailing in arbitrary_bytes(16),
        ) {
            // For opcode 0x00 (Literal), the next varint is the allocation length.
            // Constrain it to a safe range by ensuring the trailing data starts
            // with a single-byte varint (0..127).
            let mut data = vec![opcode];
            if opcode == 0x00 && !trailing.is_empty() {
                // Force a small length value
                data.push(trailing[0] & 0x7F);
                data.extend_from_slice(&trailing[1..]);
            } else {
                data.extend_from_slice(&trailing);
            }
            let mut cursor = Cursor::new(&data[..]);
            let _ = read_delta_op(&mut cursor);
        }

        /// read_delta (full stream) must not panic on arbitrary bytes.
        ///
        /// read_delta reads a count varint then that many delta ops. We prepend
        /// a small count to avoid huge allocation from a random first varint.
        #[test]
        fn read_delta_never_panics(
            count in 0u8..16,
            trailing in arbitrary_bytes(32),
        ) {
            let mut data = vec![count];
            data.extend_from_slice(&trailing);
            let mut cursor = Cursor::new(&data[..]);
            let _ = read_delta(&mut cursor);
        }

        /// read_token must not panic on arbitrary bytes.
        #[test]
        fn read_token_never_panics(data in arbitrary_bytes(16)) {
            let mut cursor = Cursor::new(&data[..]);
            let _ = read_token(&mut cursor);
        }

        /// write_delta_op -> read_delta_op roundtrip for Literal operations.
        #[test]
        fn delta_op_literal_roundtrip(data in prop::collection::vec(any::<u8>(), 0..=256)) {
            let op = DeltaOp::Literal(data);

            let mut buf = Vec::new();
            write_delta_op(&mut buf, &op).unwrap();

            let mut cursor = Cursor::new(&buf);
            let decoded = read_delta_op(&mut cursor).unwrap();
            prop_assert_eq!(decoded, op);
        }

        /// write_delta_op -> read_delta_op roundtrip for Copy operations.
        #[test]
        fn delta_op_copy_roundtrip(
            block_index in 0u32..=0x7FFF_FFFFu32,
            length in 0u32..=0x7FFF_FFFFu32,
        ) {
            let op = DeltaOp::Copy { block_index, length };

            let mut buf = Vec::new();
            write_delta_op(&mut buf, &op).unwrap();

            let mut cursor = Cursor::new(&buf);
            let decoded = read_delta_op(&mut cursor).unwrap();
            prop_assert_eq!(decoded, op);
        }

        /// write_delta -> read_delta roundtrip for complete delta streams.
        #[test]
        fn delta_stream_roundtrip(
            literal_count in 0usize..=4,
            copy_count in 0usize..=4,
        ) {
            let mut ops = Vec::new();
            for i in 0..literal_count {
                ops.push(DeltaOp::Literal(vec![i as u8; (i + 1) * 10]));
            }
            for i in 0..copy_count {
                ops.push(DeltaOp::Copy {
                    block_index: i as u32,
                    length: ((i + 1) * 4096) as u32,
                });
            }

            let mut buf = Vec::new();
            write_delta(&mut buf, &ops).unwrap();

            let mut cursor = Cursor::new(&buf);
            let decoded = read_delta(&mut cursor).unwrap();
            prop_assert_eq!(decoded, ops);
        }

        /// Token-based stream roundtrip for whole-file deltas.
        #[test]
        fn token_whole_file_roundtrip(data in prop::collection::vec(any::<u8>(), 0..=1024)) {
            let mut buf = Vec::new();
            write_whole_file_delta(&mut buf, &data).unwrap();

            // Read back: should get literal tokens then end marker
            let mut cursor = Cursor::new(&buf);
            let mut reconstructed = Vec::new();

            loop {
                let token = read_token(&mut cursor).unwrap();
                match token {
                    Some(n) if n > 0 => {
                        let mut chunk = vec![0u8; n as usize];
                        std::io::Read::read_exact(&mut cursor, &mut chunk).unwrap();
                        reconstructed.extend_from_slice(&chunk);
                    }
                    Some(_) => {
                        // Block match -- shouldn't happen in whole-file delta
                        break;
                    }
                    None => break, // End marker
                }
            }
            prop_assert_eq!(reconstructed, data);
        }

        /// Token block match roundtrip.
        #[test]
        fn token_block_match_roundtrip(block_index in 0u32..=0x7FFF_FFFFu32) {
            let mut buf = Vec::new();
            write_token_block_match(&mut buf, block_index).unwrap();

            let mut cursor = Cursor::new(&buf);
            let token = read_token(&mut cursor).unwrap();
            // Block matches are encoded as -(block_index + 1)
            let expected_token = -((block_index as i32) + 1);
            prop_assert_eq!(token, Some(expected_token));
        }

        /// Invalid delta opcodes (not 0x00 or 0x01) return InvalidData error.
        #[test]
        fn delta_op_invalid_opcode_is_error(opcode in 2u8..=255u8) {
            let data = vec![opcode, 0, 0, 0, 0, 0];
            let mut cursor = Cursor::new(&data[..]);
            let result = read_delta_op(&mut cursor);
            prop_assert!(result.is_err());
            prop_assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
        }
    }
}

// ============================================================================
// Module: NDX codec arbitrary-input fuzz
// ============================================================================

mod ndx_arbitrary_input {
    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1000))]

        /// Legacy NDX codec must not panic on arbitrary bytes.
        #[test]
        fn legacy_ndx_never_panics(data in arbitrary_bytes(16)) {
            let mut codec = LegacyNdxCodec::new(29);
            let mut cursor = Cursor::new(&data[..]);
            let _ = codec.read_ndx(&mut cursor);
        }

        /// Modern NDX codec must not panic on arbitrary bytes.
        #[test]
        fn modern_ndx_never_panics(data in arbitrary_bytes(16)) {
            let mut codec = ModernNdxCodec::new(32);
            let mut cursor = Cursor::new(&data[..]);
            let _ = codec.read_ndx(&mut cursor);
        }

        /// NdxCodecEnum must not panic on arbitrary bytes for any protocol version.
        #[test]
        fn ndx_codec_enum_never_panics(
            data in arbitrary_bytes(16),
            protocol_version in protocol_version_strategy(),
        ) {
            let mut codec = create_ndx_codec(protocol_version);
            let mut cursor = Cursor::new(&data[..]);
            let _ = codec.read_ndx(&mut cursor);
        }

        /// NdxState must not panic on arbitrary bytes.
        #[test]
        fn ndx_state_never_panics(data in arbitrary_bytes(16)) {
            let mut state = NdxState::default();
            let mut cursor = Cursor::new(&data[..]);
            let _ = state.read_ndx(&mut cursor);
        }

        /// Sequential modern NDX reads from arbitrary bytes must not panic.
        #[test]
        fn modern_ndx_sequential_never_panics(data in arbitrary_bytes(128)) {
            let mut codec = ModernNdxCodec::new(30);
            let mut cursor = Cursor::new(&data[..]);
            for _ in 0..20 {
                if cursor.position() as usize >= data.len() {
                    break;
                }
                let _ = codec.read_ndx(&mut cursor);
            }
        }

        /// Legacy NDX codec roundtrip for full i32 range.
        #[test]
        fn legacy_ndx_roundtrip(value in any::<i32>()) {
            let mut write_codec = LegacyNdxCodec::new(28);
            let mut buf = Vec::new();
            write_codec.write_ndx(&mut buf, value).unwrap();
            prop_assert_eq!(buf.len(), 4);

            let mut read_codec = LegacyNdxCodec::new(28);
            let mut cursor = Cursor::new(&buf);
            let decoded = read_codec.read_ndx(&mut cursor).unwrap();
            prop_assert_eq!(decoded, value);
        }
    }
}

// ============================================================================
// Module: Protocol version parsing arbitrary-input fuzz
// ============================================================================

mod protocol_version_arbitrary_input {
    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1000))]

        /// ProtocolVersion::from_supported must not panic for any u8.
        #[test]
        fn from_supported_never_panics(value in any::<u8>()) {
            let _ = ProtocolVersion::from_supported(value);
        }

        /// ProtocolVersion::from_supported returns Some only for 28-32.
        #[test]
        fn from_supported_valid_range(value in 28u8..=32u8) {
            let result = ProtocolVersion::from_supported(value);
            prop_assert!(result.is_some());
            prop_assert_eq!(result.unwrap().as_u8(), value);
        }

        /// ProtocolVersion::from_supported returns None outside 28-32.
        #[test]
        fn from_supported_invalid_range(value in prop::sample::select(
            (0u8..28).chain(33u8..=255).collect::<Vec<u8>>()
        )) {
            let result = ProtocolVersion::from_supported(value);
            prop_assert!(result.is_none());
        }

        /// ProtocolVersion::from_str must not panic for any string.
        #[test]
        fn from_str_never_panics(s in ".*") {
            let _ = s.parse::<ProtocolVersion>();
        }

        /// ProtocolVersion::from_str roundtrip for valid versions.
        #[test]
        fn from_str_roundtrip(value in 28u8..=32u8) {
            let s = value.to_string();
            let parsed: ProtocolVersion = s.parse().unwrap();
            prop_assert_eq!(parsed.as_u8(), value);
        }

        /// ProtocolVersion::is_supported_protocol_number must not panic for any u8.
        #[test]
        fn is_supported_never_panics(value in any::<u8>()) {
            let _ = ProtocolVersion::is_supported_protocol_number(value);
        }

        /// Feature query methods must not panic for any supported version.
        #[test]
        fn feature_queries_never_panic(value in 28u8..=32u8) {
            let version = ProtocolVersion::from_supported(value).unwrap();
            let _ = version.uses_binary_negotiation();
            let _ = version.uses_legacy_ascii_negotiation();
            let _ = version.uses_varint_encoding();
            let _ = version.uses_fixed_encoding();
            let _ = version.supports_extended_flags();
            let _ = version.supports_flist_times();
        }
    }
}

// ============================================================================
// Module: Protocol codec arbitrary-input fuzz
// ============================================================================

mod protocol_codec_arbitrary_input {
    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(500))]

        /// ProtocolCodec::read_file_size must not panic on arbitrary bytes.
        #[test]
        fn codec_read_file_size_never_panics(
            data in arbitrary_bytes(32),
            protocol_version in protocol_version_strategy(),
        ) {
            let codec = create_protocol_codec(protocol_version);
            let mut cursor = Cursor::new(&data[..]);
            let _ = codec.read_file_size(&mut cursor);
        }

        /// ProtocolCodec::read_mtime must not panic on arbitrary bytes.
        #[test]
        fn codec_read_mtime_never_panics(
            data in arbitrary_bytes(32),
            protocol_version in protocol_version_strategy(),
        ) {
            let codec = create_protocol_codec(protocol_version);
            let mut cursor = Cursor::new(&data[..]);
            let _ = codec.read_mtime(&mut cursor);
        }

        /// ProtocolCodec::read_int must not panic on arbitrary bytes.
        #[test]
        fn codec_read_int_never_panics(
            data in arbitrary_bytes(16),
            protocol_version in protocol_version_strategy(),
        ) {
            let codec = create_protocol_codec(protocol_version);
            let mut cursor = Cursor::new(&data[..]);
            let _ = codec.read_int(&mut cursor);
        }

        /// ProtocolCodec::read_long_name_len must not panic on arbitrary bytes.
        #[test]
        fn codec_read_long_name_len_never_panics(
            data in arbitrary_bytes(16),
            protocol_version in protocol_version_strategy(),
        ) {
            let codec = create_protocol_codec(protocol_version);
            let mut cursor = Cursor::new(&data[..]);
            let _ = codec.read_long_name_len(&mut cursor);
        }

        /// ProtocolCodec::read_varint must not panic on arbitrary bytes.
        #[test]
        fn codec_read_varint_never_panics(
            data in arbitrary_bytes(16),
            protocol_version in protocol_version_strategy(),
        ) {
            let codec = create_protocol_codec(protocol_version);
            let mut cursor = Cursor::new(&data[..]);
            let _ = codec.read_varint(&mut cursor);
        }

        /// ProtocolCodec::read_stat must not panic on arbitrary bytes.
        #[test]
        fn codec_read_stat_never_panics(
            data in arbitrary_bytes(32),
            protocol_version in protocol_version_strategy(),
        ) {
            let codec = create_protocol_codec(protocol_version);
            let mut cursor = Cursor::new(&data[..]);
            let _ = codec.read_stat(&mut cursor);
        }
    }
}

// ============================================================================
// Module: Combined arbitrary-input stress testing
// ============================================================================

mod combined_proptest_stress {
    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        /// Feed the same arbitrary byte slice to every decoder -- none should panic.
        ///
        /// Uses small inputs because several decoders (read_signature, read_delta,
        /// decode_name) interpret leading varints as allocation lengths.
        #[test]
        fn all_decoders_no_panic_on_same_input(data in arbitrary_bytes(16)) {
            // Varint family
            let mut c = Cursor::new(&data[..]);
            let _ = read_varint(&mut c);

            let mut c = Cursor::new(&data[..]);
            let _ = read_varlong(&mut c, 3);

            let mut c = Cursor::new(&data[..]);
            let _ = read_varlong(&mut c, 4);

            let mut c = Cursor::new(&data[..]);
            let _ = read_int(&mut c);

            let mut c = Cursor::new(&data[..]);
            let _ = read_longint(&mut c);

            let _ = decode_varint(&data);

            // Envelope
            let _ = MessageHeader::decode(&data);

            // NDX codecs
            {
                let mut legacy = LegacyNdxCodec::new(29);
                let mut c = Cursor::new(&data[..]);
                let _ = legacy.read_ndx(&mut c);
            }
            {
                let mut modern = ModernNdxCodec::new(32);
                let mut c = Cursor::new(&data[..]);
                let _ = modern.read_ndx(&mut c);
            }

            // Delta -- read_token is allocation-safe (returns token value only)
            {
                let mut c = Cursor::new(&data[..]);
                let _ = read_token(&mut c);
            }

            // Note: read_delta_op, read_delta, read_signature, and decode_name
            // are tested separately with safe input constraints because they
            // interpret leading varints as allocation lengths.

            // File entry decode (allocation-safe functions only)
            {
                let mut c = Cursor::new(&data[..]);
                let _ = decode_flags(&mut c, 32, false);
            }
            {
                let mut c = Cursor::new(&data[..]);
                let _ = decode_flags(&mut c, 32, true);
            }
            {
                let mut c = Cursor::new(&data[..]);
                let _ = decode_flags(&mut c, 28, false);
            }
            {
                let mut c = Cursor::new(&data[..]);
                let _ = decode_size(&mut c, 32);
            }
            {
                let mut c = Cursor::new(&data[..]);
                let _ = decode_size(&mut c, 29);
            }
            {
                let mut c = Cursor::new(&data[..]);
                let _ = decode_mtime(&mut c, 0, 0, 32);
            }
            {
                let mut c = Cursor::new(&data[..]);
                let _ = decode_mode(&mut c, 0, 0);
            }

            // Protocol codecs
            for version in [28u8, 29, 30, 31, 32] {
                let codec = create_protocol_codec(version);
                let mut c = Cursor::new(&data[..]);
                let _ = codec.read_file_size(&mut c);

                let mut c = Cursor::new(&data[..]);
                let _ = codec.read_mtime(&mut c);

                let mut c = Cursor::new(&data[..]);
                let _ = codec.read_int(&mut c);
            }
        }

        /// Sequential multi-type decoding from a random stream must not panic.
        ///
        /// Uses small inputs because read_delta_op can allocate based on
        /// varint-decoded lengths.
        #[test]
        fn sequential_mixed_decode_no_panic(data in arbitrary_bytes(64)) {
            let mut cursor = Cursor::new(&data[..]);
            let len = data.len();

            for _ in 0..100 {
                if cursor.position() as usize >= len {
                    break;
                }
                // Alternate between different decoder types
                let pos = cursor.position() as usize;
                match pos % 4 {
                    0 => { let _ = read_varint(&mut cursor); }
                    1 => { let _ = read_int(&mut cursor); }
                    2 => { let _ = read_token(&mut cursor); }
                    _ => { let _ = read_varlong(&mut cursor, 3); }
                }
            }
        }
    }
}

// ============================================================================
// Module: Boundary value proptest coverage
// ============================================================================

mod boundary_values {
    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        /// Varint roundtrip at powers of two and their neighbors.
        #[test]
        fn varint_power_of_two_boundaries(exp in 0u32..31u32) {
            let base = 1i32 << exp;
            for offset in [-1i32, 0, 1] {
                let value = base.saturating_add(offset);
                let mut encoded = Vec::new();
                encode_varint_to_vec(value, &mut encoded);
                let (decoded, _) = decode_varint(&encoded).unwrap();
                prop_assert_eq!(decoded, value);
            }
        }

        /// Varint roundtrip for extreme i32 values.
        #[test]
        fn varint_extreme_values(value in prop::sample::select(vec![
            i32::MIN, i32::MIN + 1, -1, 0, 1, i32::MAX - 1, i32::MAX
        ])) {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);
            let (decoded, _) = decode_varint(&encoded).unwrap();
            prop_assert_eq!(decoded, value);
        }

        /// MessageHeader payload_len boundary values.
        #[test]
        fn message_header_payload_boundaries(code_idx in 0usize..18usize) {
            let code = MessageCode::ALL[code_idx];
            for payload in [0u32, 1, 0x00FF_FFFE, 0x00FF_FFFF] {
                let header = MessageHeader::new(code, payload).unwrap();
                let encoded = header.encode();
                let decoded = MessageHeader::decode(&encoded).unwrap();
                prop_assert_eq!(decoded.payload_len(), payload);
            }
        }

        /// File size boundary values roundtrip for all protocols.
        #[test]
        fn file_size_boundaries(protocol_version in protocol_version_strategy()) {
            let codec = create_protocol_codec(protocol_version);
            let boundary_sizes: Vec<i64> = vec![
                0, 1, 255, 256, 65535, 65536, 0x7FFF_FFFF, 0x8000_0000,
            ];
            for size in boundary_sizes {
                let mut buf = Vec::new();
                codec.write_file_size(&mut buf, size).unwrap();
                let mut cursor = Cursor::new(&buf);
                let decoded = codec.read_file_size(&mut cursor).unwrap();
                prop_assert_eq!(decoded, size);
            }
        }
    }
}
