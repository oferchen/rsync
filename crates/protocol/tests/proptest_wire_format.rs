//! Property-based roundtrip tests for wire format encode/decode.
//!
//! Verifies that encoding then decoding produces the original value for
//! arbitrary inputs across wire protocol types not already covered by
//! `proptest_codec_roundtrips.rs`. Focus areas:
//!
//! - Signature block write/read roundtrips
//! - Delta operations (internal opcode format) write/read roundtrips
//! - Token stream (upstream format) write/read roundtrips
//! - TransferStats wire format roundtrips across protocol versions
//! - DeleteStats wire format roundtrips
//! - MessageHeader encode_raw/from_raw roundtrips
//! - MessageCode u8 conversion roundtrips
//! - varlong30 encode/decode roundtrips

use proptest::prelude::*;
use protocol::wire::{
    DeltaOp, SignatureBlock, read_delta, read_delta_op, read_signature, read_token, write_delta,
    write_delta_op, write_signature, write_token_block_match, write_token_end, write_token_literal,
};
use protocol::{
    DeleteStats, MessageCode, MessageHeader, ProtocolVersion, TransferStats, read_varlong30,
    write_varlong30,
};
use std::io::Cursor;

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

/// Generates a valid strong checksum length (1-16 bytes).
fn strong_sum_length_strategy() -> impl Strategy<Value = u8> {
    prop_oneof![Just(2u8), Just(4u8), Just(8u8), Just(16u8),]
}

/// Generates a block length value (typical rsync block sizes).
fn block_length_strategy() -> impl Strategy<Value = u32> {
    prop_oneof![
        Just(512u32),
        Just(1024u32),
        Just(2048u32),
        Just(4096u32),
        Just(8192u32),
        Just(16384u32),
    ]
}

/// Generates a literal DeltaOp with bounded data size.
fn literal_op_strategy() -> impl Strategy<Value = DeltaOp> {
    prop::collection::vec(any::<u8>(), 1..=1024).prop_map(DeltaOp::Literal)
}

/// Generates a copy DeltaOp with reasonable values.
fn copy_op_strategy() -> impl Strategy<Value = DeltaOp> {
    (0u32..10000, 1u32..=8192).prop_map(|(block_index, length)| DeltaOp::Copy {
        block_index,
        length,
    })
}

/// Generates an arbitrary DeltaOp (literal or copy).
fn delta_op_strategy() -> impl Strategy<Value = DeltaOp> {
    prop_oneof![literal_op_strategy(), copy_op_strategy(),]
}

/// Generates valid message codes for proptest.
fn message_code_strategy() -> impl Strategy<Value = MessageCode> {
    prop::sample::select(MessageCode::ALL.to_vec())
}

/// Generates protocol versions for testing.
fn protocol_version_strategy() -> impl Strategy<Value = ProtocolVersion> {
    prop::sample::select(vec![
        ProtocolVersion::V28,
        ProtocolVersion::V29,
        ProtocolVersion::V30,
        ProtocolVersion::V31,
        ProtocolVersion::V32,
    ])
}

/// Generates u64 values that fit safely in varlong30 with min_bytes=3.
///
/// varlong30 uses signed i64 internally, and the maximum safe value depends
/// on min_bytes. With min_bytes=3, the limit is approximately 288 PB.
fn stat_value_strategy() -> impl Strategy<Value = u64> {
    prop_oneof![
        Just(0u64),
        1u64..=4096,
        4097u64..=1_000_000,
        1_000_001u64..=1_000_000_000,
        1_000_000_001u64..=100_000_000_000,
    ]
}

// ---------------------------------------------------------------------------
// Signature block roundtrips
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// A single signature block roundtrips through write_signature/read_signature.
    #[test]
    fn signature_single_block_roundtrips(
        rolling_sum in any::<u32>(),
        strong_sum_len in strong_sum_length_strategy(),
        strong_sum in prop::collection::vec(any::<u8>(), 1..=16),
        block_length in block_length_strategy(),
    ) {
        // Truncate strong_sum to the declared length
        let strong_sum: Vec<u8> = strong_sum.into_iter().take(strong_sum_len as usize).collect();
        if strong_sum.len() != strong_sum_len as usize {
            // Not enough bytes generated; skip
            return Ok(());
        }

        let blocks = vec![SignatureBlock {
            index: 0,
            rolling_sum,
            strong_sum: strong_sum.clone(),
        }];

        let mut buf = Vec::new();
        write_signature(&mut buf, 1, block_length, strong_sum_len, &blocks)?;

        let (decoded_block_len, decoded_count, decoded_strong_len, decoded_blocks) =
            read_signature(&mut Cursor::new(&buf))?;

        prop_assert_eq!(decoded_block_len, block_length);
        prop_assert_eq!(decoded_count, 1);
        prop_assert_eq!(decoded_strong_len, strong_sum_len);
        prop_assert_eq!(decoded_blocks.len(), 1);
        prop_assert_eq!(decoded_blocks[0].rolling_sum, rolling_sum);
        prop_assert_eq!(&decoded_blocks[0].strong_sum, &strong_sum);
    }

    /// Multiple signature blocks roundtrip correctly.
    #[test]
    fn signature_multiple_blocks_roundtrip(
        block_length in block_length_strategy(),
        strong_sum_len in strong_sum_length_strategy(),
        block_count in 1usize..=16,
    ) {
        let blocks: Vec<SignatureBlock> = (0..block_count)
            .map(|i| SignatureBlock {
                index: i as u32,
                rolling_sum: (i as u32).wrapping_mul(0x1234_5678),
                strong_sum: vec![i as u8; strong_sum_len as usize],
            })
            .collect();

        let mut buf = Vec::new();
        write_signature(
            &mut buf,
            block_count as u32,
            block_length,
            strong_sum_len,
            &blocks,
        )?;

        let (decoded_block_len, decoded_count, decoded_strong_len, decoded_blocks) =
            read_signature(&mut Cursor::new(&buf))?;

        prop_assert_eq!(decoded_block_len, block_length);
        prop_assert_eq!(decoded_count, block_count as u32);
        prop_assert_eq!(decoded_strong_len, strong_sum_len);
        prop_assert_eq!(decoded_blocks.len(), block_count);

        for (i, block) in decoded_blocks.iter().enumerate() {
            prop_assert_eq!(block.index, i as u32);
            prop_assert_eq!(block.rolling_sum, blocks[i].rolling_sum);
            prop_assert_eq!(&block.strong_sum, &blocks[i].strong_sum);
        }
    }

    /// Empty signature (zero blocks) roundtrips.
    #[test]
    fn signature_empty_roundtrips(
        block_length in block_length_strategy(),
        strong_sum_len in strong_sum_length_strategy(),
    ) {
        let mut buf = Vec::new();
        write_signature(&mut buf, 0, block_length, strong_sum_len, &[])?;

        let (decoded_block_len, decoded_count, decoded_strong_len, decoded_blocks) =
            read_signature(&mut Cursor::new(&buf))?;

        prop_assert_eq!(decoded_block_len, block_length);
        prop_assert_eq!(decoded_count, 0);
        prop_assert_eq!(decoded_strong_len, strong_sum_len);
        prop_assert!(decoded_blocks.is_empty());
    }
}

// ---------------------------------------------------------------------------
// Delta operation roundtrips (internal opcode format)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// A single literal DeltaOp roundtrips through write_delta_op/read_delta_op.
    #[test]
    fn delta_op_literal_roundtrips(data in prop::collection::vec(any::<u8>(), 1..=2048)) {
        let op = DeltaOp::Literal(data);

        let mut buf = Vec::new();
        write_delta_op(&mut buf, &op)?;

        let decoded = read_delta_op(&mut Cursor::new(&buf))?;
        prop_assert_eq!(decoded, op);
    }

    /// A single copy DeltaOp roundtrips through write_delta_op/read_delta_op.
    #[test]
    fn delta_op_copy_roundtrips(
        block_index in 0u32..1_000_000,
        length in 1u32..=65536,
    ) {
        let op = DeltaOp::Copy { block_index, length };

        let mut buf = Vec::new();
        write_delta_op(&mut buf, &op)?;

        let decoded = read_delta_op(&mut Cursor::new(&buf))?;
        prop_assert_eq!(decoded, op);
    }

    /// A sequence of DeltaOps roundtrips through write_delta/read_delta.
    #[test]
    fn delta_stream_roundtrips(ops in prop::collection::vec(delta_op_strategy(), 1..=16)) {
        let mut buf = Vec::new();
        write_delta(&mut buf, &ops)?;

        let decoded = read_delta(&mut Cursor::new(&buf))?;
        prop_assert_eq!(decoded.len(), ops.len());
        prop_assert_eq!(decoded, ops);
    }

    /// Empty delta stream roundtrips.
    #[test]
    fn delta_empty_stream_roundtrips(_dummy in Just(())) {
        let ops: Vec<DeltaOp> = vec![];
        let mut buf = Vec::new();
        write_delta(&mut buf, &ops)?;

        let decoded = read_delta(&mut Cursor::new(&buf))?;
        prop_assert!(decoded.is_empty());
    }
}

// ---------------------------------------------------------------------------
// Token stream roundtrips (upstream wire format)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// Literal data roundtrips through write_token_literal/read_token.
    #[test]
    fn token_literal_roundtrips(data in prop::collection::vec(any::<u8>(), 1..=4096)) {
        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data)?;
        write_token_end(&mut buf)?;

        let mut cursor = Cursor::new(&buf);
        let mut decoded_data = Vec::new();

        loop {
            match read_token(&mut cursor)? {
                Some(n) if n > 0 => {
                    let len = n as usize;
                    let mut chunk = vec![0u8; len];
                    std::io::Read::read_exact(&mut cursor, &mut chunk)?;
                    decoded_data.extend_from_slice(&chunk);
                }
                Some(_n) => {
                    // Block match - should not appear for literal-only streams
                    prop_assert!(false, "unexpected block match in literal stream");
                }
                None => break,
            }
        }

        prop_assert_eq!(decoded_data, data);
    }

    /// Block match token roundtrips through write_token_block_match/read_token.
    #[test]
    fn token_block_match_roundtrips(block_index in 0u32..1_000_000) {
        let mut buf = Vec::new();
        write_token_block_match(&mut buf, block_index)?;

        let mut cursor = Cursor::new(&buf);
        let token = read_token(&mut cursor)?;

        // Token encoding: -(block_index + 1)
        let expected_token = -((block_index as i32) + 1);
        prop_assert_eq!(token, Some(expected_token));

        // Verify we can recover the block index
        let recovered_index = (-(token.unwrap() + 1)) as u32;
        prop_assert_eq!(recovered_index, block_index);
    }

    /// End marker roundtrips.
    #[test]
    fn token_end_marker_roundtrips(_dummy in Just(())) {
        let mut buf = Vec::new();
        write_token_end(&mut buf)?;

        let mut cursor = Cursor::new(&buf);
        let token = read_token(&mut cursor)?;
        prop_assert_eq!(token, None);
    }
}

// ---------------------------------------------------------------------------
// TransferStats wire format roundtrips
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// TransferStats roundtrips through write_to/read_from for all protocol versions.
    #[test]
    fn transfer_stats_roundtrips(
        total_read in stat_value_strategy(),
        total_written in stat_value_strategy(),
        total_size in stat_value_strategy(),
        flist_buildtime in stat_value_strategy(),
        flist_xfertime in stat_value_strategy(),
        protocol in protocol_version_strategy(),
    ) {
        let stats = TransferStats::with_bytes(total_read, total_written, total_size)
            .with_flist_times(flist_buildtime, flist_xfertime);

        let mut buf = Vec::new();
        stats.write_to(&mut buf, protocol)?;

        let decoded = TransferStats::read_from(&mut Cursor::new(&buf), protocol)?;

        // Core fields always roundtrip
        prop_assert_eq!(decoded.total_read, total_read);
        prop_assert_eq!(decoded.total_written, total_written);
        prop_assert_eq!(decoded.total_size, total_size);

        // Flist times only roundtrip for protocol >= 29
        if protocol >= ProtocolVersion::V29 {
            prop_assert_eq!(decoded.flist_buildtime, flist_buildtime);
            prop_assert_eq!(decoded.flist_xfertime, flist_xfertime);
        } else {
            prop_assert_eq!(decoded.flist_buildtime, 0);
            prop_assert_eq!(decoded.flist_xfertime, 0);
        }
    }

    /// TransferStats with zero values roundtrips.
    #[test]
    fn transfer_stats_zeros_roundtrip(protocol in protocol_version_strategy()) {
        let stats = TransferStats::new();

        let mut buf = Vec::new();
        stats.write_to(&mut buf, protocol)?;

        let decoded = TransferStats::read_from(&mut Cursor::new(&buf), protocol)?;

        prop_assert_eq!(decoded.total_read, 0);
        prop_assert_eq!(decoded.total_written, 0);
        prop_assert_eq!(decoded.total_size, 0);
    }
}

// ---------------------------------------------------------------------------
// DeleteStats wire format roundtrips
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// DeleteStats roundtrips through write_to/read_from.
    #[test]
    fn delete_stats_roundtrips(
        files in 0u32..100_000,
        dirs in 0u32..10_000,
        symlinks in 0u32..10_000,
        devices in 0u32..1_000,
        specials in 0u32..1_000,
    ) {
        let stats = DeleteStats {
            files,
            dirs,
            symlinks,
            devices,
            specials,
        };

        let mut buf = Vec::new();
        stats.write_to(&mut buf)?;

        let decoded = DeleteStats::read_from(&mut Cursor::new(&buf))?;

        prop_assert_eq!(decoded.files, files);
        prop_assert_eq!(decoded.dirs, dirs);
        prop_assert_eq!(decoded.symlinks, symlinks);
        prop_assert_eq!(decoded.devices, devices);
        prop_assert_eq!(decoded.specials, specials);
    }

    /// DeleteStats total is preserved.
    #[test]
    fn delete_stats_total_preserved(
        files in 0u32..10_000,
        dirs in 0u32..10_000,
        symlinks in 0u32..10_000,
        devices in 0u32..1_000,
        specials in 0u32..1_000,
    ) {
        let stats = DeleteStats { files, dirs, symlinks, devices, specials };

        let mut buf = Vec::new();
        stats.write_to(&mut buf)?;

        let decoded = DeleteStats::read_from(&mut Cursor::new(&buf))?;
        prop_assert_eq!(decoded.total(), stats.total());
    }
}

// ---------------------------------------------------------------------------
// MessageHeader encode_raw/from_raw roundtrips
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// MessageHeader roundtrips through encode_raw/from_raw for all codes.
    #[test]
    fn message_header_raw_roundtrips(
        code in message_code_strategy(),
        payload_len in 0u32..=0x00FFFFFFu32,
    ) {
        let header = MessageHeader::new(code, payload_len)?;
        let raw = header.encode_raw();
        let decoded = MessageHeader::from_raw(raw)?;

        prop_assert_eq!(decoded.code(), code);
        prop_assert_eq!(decoded.payload_len(), payload_len);
    }

    /// MessageHeader encode/decode via byte array roundtrips.
    #[test]
    fn message_header_bytes_roundtrip(
        code in message_code_strategy(),
        payload_len in 0u32..=0x00FFFFFFu32,
    ) {
        let header = MessageHeader::new(code, payload_len)?;
        let bytes = header.encode();
        let decoded = MessageHeader::decode(&bytes)?;

        prop_assert_eq!(decoded.code(), code);
        prop_assert_eq!(decoded.payload_len(), payload_len);
    }

    /// MessageHeader encode_into_slice produces same bytes as encode.
    #[test]
    fn message_header_encode_into_slice_matches(
        code in message_code_strategy(),
        payload_len in 0u32..=0x00FFFFFFu32,
    ) {
        let header = MessageHeader::new(code, payload_len)?;

        let direct = header.encode();
        let mut slice_buf = [0u8; 8];
        header.encode_into_slice(&mut slice_buf)?;

        prop_assert_eq!(&slice_buf[..4], &direct[..]);
    }
}

// ---------------------------------------------------------------------------
// MessageCode u8 conversion roundtrips
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// MessageCode roundtrips through as_u8/from_u8 for all valid codes.
    #[test]
    fn message_code_u8_roundtrips(code in message_code_strategy()) {
        let value = code.as_u8();
        let decoded = MessageCode::from_u8(value);
        prop_assert_eq!(decoded, Some(code));
    }

    /// MessageCode name/from_str roundtrips for all valid codes.
    #[test]
    fn message_code_name_roundtrips(code in message_code_strategy()) {
        let name = code.name();
        let parsed: MessageCode = name.parse().unwrap();
        prop_assert_eq!(parsed, code);
    }

    /// MessageCode from_u8 returns None for invalid values.
    #[test]
    fn message_code_invalid_u8_returns_none(
        value in any::<u8>().prop_filter("exclude valid codes", |v| {
            MessageCode::from_u8(*v).is_none()
        })
    ) {
        prop_assert!(MessageCode::from_u8(value).is_none());
    }
}

// ---------------------------------------------------------------------------
// varlong30 roundtrips
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// varlong30 roundtrips with min_bytes=3 (file sizes, stats).
    #[test]
    fn varlong30_roundtrips_min3(value in 0i64..=0x03FF_FFFF_FFFF_FFFFi64) {
        let mut buf = Vec::new();
        write_varlong30(&mut buf, value, 3)?;

        let mut cursor = Cursor::new(&buf);
        let decoded = read_varlong30(&mut cursor, 3)?;
        prop_assert_eq!(decoded, value);
    }

    /// varlong30 roundtrips with min_bytes=4 (timestamps).
    #[test]
    fn varlong30_roundtrips_min4(value in 0i64..=0x003F_FFFF_FFFF_FFFFi64) {
        let mut buf = Vec::new();
        write_varlong30(&mut buf, value, 4)?;

        let mut cursor = Cursor::new(&buf);
        let decoded = read_varlong30(&mut cursor, 4)?;
        prop_assert_eq!(decoded, value);
    }

    /// varlong30 encoding is deterministic.
    #[test]
    fn varlong30_encoding_deterministic(
        value in 0i64..=0x03FF_FFFF_FFFF_FFFFi64,
        min_bytes in 1u8..=4u8,
    ) {
        let mut buf1 = Vec::new();
        let mut buf2 = Vec::new();

        write_varlong30(&mut buf1, value, min_bytes)?;
        write_varlong30(&mut buf2, value, min_bytes)?;

        prop_assert_eq!(buf1, buf2);
    }
}
