//! Property-based roundtrip tests for delta script encode/decode.
//!
//! Verifies that arbitrary delta token streams survive encode-then-decode
//! roundtrips through both the internal opcode-based wire format
//! (`write_delta`/`read_delta`) and the upstream token-based wire format
//! (`write_token_stream`/`read_token`).
//!
//! Coverage targets:
//! - Arbitrary interleaved COPY and DATA (Literal) tokens
//! - Empty delta streams (EOF-only)
//! - Large block indices near the i32 boundary
//! - Large literal payloads that trigger CHUNK_SIZE splitting
//! - Byte-level content preservation for literals
//! - Copy offset and length preserved exactly

use proptest::prelude::*;
use protocol::wire::{
    CHUNK_SIZE, DeltaOp, read_delta, read_delta_op, read_int, read_token, write_delta,
    write_delta_op, write_token_block_match, write_token_end, write_token_literal,
    write_token_stream,
};
use std::io::{Cursor, Read as _};

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

/// Generates an arbitrary `DeltaOp::Literal` with bounded size.
fn arb_literal(max_len: usize) -> impl Strategy<Value = DeltaOp> {
    prop::collection::vec(any::<u8>(), 1..=max_len).prop_map(DeltaOp::Literal)
}

/// Generates an arbitrary `DeltaOp::Copy` with block_index in varint-safe range.
///
/// The internal format encodes block_index as a varint (i32), so we cap at
/// `i32::MAX` to stay in the representable range.
fn arb_copy() -> impl Strategy<Value = DeltaOp> {
    (0u32..=0x7FFF_FFFFu32, 1u32..=0x7FFF_FFFFu32).prop_map(|(block_index, length)| DeltaOp::Copy {
        block_index,
        length,
    })
}

/// Generates an arbitrary `DeltaOp` (Literal or Copy).
fn arb_delta_op() -> impl Strategy<Value = DeltaOp> {
    prop_oneof![arb_literal(512), arb_copy(),]
}

/// Generates an arbitrary sequence of `DeltaOp` values representing a delta script.
fn arb_delta_script(max_ops: usize) -> impl Strategy<Value = Vec<DeltaOp>> {
    prop::collection::vec(arb_delta_op(), 0..=max_ops)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Decodes a token-based stream written by `write_token_stream` back into
/// `(literals, block_indices)`.
///
/// This mirrors the receiver-side token reader: positive i32 = literal length,
/// negative i32 = block match at `-(token+1)`, zero = end marker.
fn decode_token_stream(data: &[u8]) -> std::io::Result<(Vec<Vec<u8>>, Vec<u32>)> {
    let mut cursor = Cursor::new(data);
    let mut literals = Vec::new();
    let mut blocks = Vec::new();

    loop {
        match read_token(&mut cursor)? {
            None => break,
            Some(token) if token > 0 => {
                let len = token as usize;
                let mut chunk = vec![0u8; len];
                cursor.read_exact(&mut chunk)?;
                literals.push(chunk);
            }
            Some(token) => {
                let block_index = (-(token + 1)) as u32;
                blocks.push(block_index);
            }
        }
    }

    Ok((literals, blocks))
}

// ---------------------------------------------------------------------------
// Internal opcode format roundtrips
// ---------------------------------------------------------------------------

mod internal_format {
    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(500))]

        /// A single arbitrary Literal op roundtrips through write/read_delta_op.
        #[test]
        fn single_literal_roundtrip(data in prop::collection::vec(any::<u8>(), 0..=4096)) {
            let op = DeltaOp::Literal(data);
            let mut buf = Vec::new();
            write_delta_op(&mut buf, &op).unwrap();
            let decoded = read_delta_op(&mut Cursor::new(&buf)).unwrap();
            prop_assert_eq!(decoded, op);
        }

        /// A single arbitrary Copy op roundtrips through write/read_delta_op.
        #[test]
        fn single_copy_roundtrip(
            block_index in 0u32..=0x7FFF_FFFFu32,
            length in 0u32..=0x7FFF_FFFFu32,
        ) {
            let op = DeltaOp::Copy { block_index, length };
            let mut buf = Vec::new();
            write_delta_op(&mut buf, &op).unwrap();
            let decoded = read_delta_op(&mut Cursor::new(&buf)).unwrap();
            prop_assert_eq!(decoded, op);
        }

        /// An arbitrary interleaved sequence of ops roundtrips through write/read_delta.
        #[test]
        fn interleaved_stream_roundtrip(ops in arb_delta_script(16)) {
            let mut buf = Vec::new();
            write_delta(&mut buf, &ops).unwrap();
            let decoded = read_delta(&mut Cursor::new(&buf)).unwrap();
            prop_assert_eq!(decoded, ops);
        }

        /// An empty delta stream roundtrips (zero ops).
        #[test]
        fn empty_stream_roundtrip(_dummy in Just(())) {
            let ops: Vec<DeltaOp> = vec![];
            let mut buf = Vec::new();
            write_delta(&mut buf, &ops).unwrap();
            let decoded = read_delta(&mut Cursor::new(&buf)).unwrap();
            prop_assert!(decoded.is_empty());
        }

        /// Copy ops at the upper boundary of varint range roundtrip.
        #[test]
        fn copy_boundary_values_roundtrip(
            block_index in prop::sample::select(vec![0u32, 1, 127, 128, 16383, 16384, 0x7FFF_FFFFu32]),
            length in prop::sample::select(vec![1u32, 127, 128, 16383, 16384, 0x7FFF_FFFFu32]),
        ) {
            let op = DeltaOp::Copy { block_index, length };
            let mut buf = Vec::new();
            write_delta_op(&mut buf, &op).unwrap();
            let decoded = read_delta_op(&mut Cursor::new(&buf)).unwrap();
            prop_assert_eq!(decoded, op);
        }

        /// Large literal payloads (multi-kilobyte) preserve every byte.
        #[test]
        fn large_literal_content_preserved(
            seed in any::<u8>(),
            size in 1usize..=8192,
        ) {
            let data: Vec<u8> = (0..size).map(|i| seed.wrapping_add(i as u8)).collect();
            let op = DeltaOp::Literal(data);
            let mut buf = Vec::new();
            write_delta_op(&mut buf, &op).unwrap();
            let decoded = read_delta_op(&mut Cursor::new(&buf)).unwrap();
            prop_assert_eq!(decoded, op);
        }
    }
}

// ---------------------------------------------------------------------------
// Upstream token-based wire format roundtrips
// ---------------------------------------------------------------------------

mod token_format {
    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(500))]

        /// Arbitrary literal data roundtrips through write_token_literal + decode.
        ///
        /// write_token_literal chunks data into CHUNK_SIZE pieces. Decoding must
        /// reassemble them into the original payload.
        #[test]
        fn literal_roundtrip(data in prop::collection::vec(any::<u8>(), 1..=CHUNK_SIZE * 3)) {
            let mut buf = Vec::new();
            write_token_literal(&mut buf, &data).unwrap();
            write_token_end(&mut buf).unwrap();

            let (chunks, blocks) = decode_token_stream(&buf).unwrap();
            prop_assert!(blocks.is_empty());

            let reconstructed: Vec<u8> = chunks.into_iter().flatten().collect();
            prop_assert_eq!(reconstructed, data);
        }

        /// Block match tokens preserve the block index exactly.
        #[test]
        fn block_match_roundtrip(block_index in 0u32..=0x7FFF_FFFEu32) {
            let mut buf = Vec::new();
            write_token_block_match(&mut buf, block_index).unwrap();
            write_token_end(&mut buf).unwrap();

            let (literals, blocks) = decode_token_stream(&buf).unwrap();
            prop_assert!(literals.is_empty());
            prop_assert_eq!(blocks.len(), 1);
            prop_assert_eq!(blocks[0], block_index);
        }

        /// Mixed interleaved Literal and Copy ops survive token-stream roundtrip.
        ///
        /// write_token_stream encodes each op then appends an end marker.
        /// We decode and verify that all literals and block indices match.
        #[test]
        fn mixed_token_stream_roundtrip(ops in arb_delta_script(12)) {
            let mut buf = Vec::new();
            write_token_stream(&mut buf, &ops).unwrap();

            let (literal_chunks, block_indices) = decode_token_stream(&buf).unwrap();

            // Collect expected literals and block indices from the original ops.
            let mut expected_literals: Vec<u8> = Vec::new();
            let mut expected_blocks: Vec<u32> = Vec::new();
            for op in &ops {
                match op {
                    DeltaOp::Literal(data) => expected_literals.extend_from_slice(data),
                    DeltaOp::Copy { block_index, .. } => expected_blocks.push(*block_index),
                }
            }

            let actual_literals: Vec<u8> = literal_chunks.into_iter().flatten().collect();
            prop_assert_eq!(actual_literals, expected_literals, "literal data mismatch");
            prop_assert_eq!(block_indices, expected_blocks, "block index mismatch");
        }

        /// Empty token stream (EOF-only) decodes to nothing.
        #[test]
        fn empty_token_stream_roundtrip(_dummy in Just(())) {
            let mut buf = Vec::new();
            write_token_stream(&mut buf, &[]).unwrap();

            // Should be just the end marker: 4 zero bytes.
            prop_assert_eq!(buf.len(), 4);
            let end = read_int(&mut Cursor::new(&buf)).unwrap();
            prop_assert_eq!(end, 0);

            let (literals, blocks) = decode_token_stream(&buf).unwrap();
            prop_assert!(literals.is_empty());
            prop_assert!(blocks.is_empty());
        }

        /// Literals exactly at CHUNK_SIZE boundary produce a single chunk.
        #[test]
        fn literal_exactly_chunk_size(fill in any::<u8>()) {
            let data = vec![fill; CHUNK_SIZE];
            let mut buf = Vec::new();
            write_token_literal(&mut buf, &data).unwrap();
            write_token_end(&mut buf).unwrap();

            // Single chunk: 4-byte header + CHUNK_SIZE data + 4-byte end marker.
            prop_assert_eq!(buf.len(), 4 + CHUNK_SIZE + 4);

            let (chunks, _) = decode_token_stream(&buf).unwrap();
            let reconstructed: Vec<u8> = chunks.into_iter().flatten().collect();
            prop_assert_eq!(reconstructed, data);
        }

        /// Literals one byte over CHUNK_SIZE produce exactly two chunks.
        #[test]
        fn literal_one_over_chunk_size(fill in any::<u8>()) {
            let data = vec![fill; CHUNK_SIZE + 1];
            let mut buf = Vec::new();
            write_token_literal(&mut buf, &data).unwrap();
            write_token_end(&mut buf).unwrap();

            // Two chunks: (4 + CHUNK_SIZE) + (4 + 1) + 4 end marker.
            prop_assert_eq!(buf.len(), 4 + CHUNK_SIZE + 4 + 1 + 4);

            let (chunks, _) = decode_token_stream(&buf).unwrap();
            let reconstructed: Vec<u8> = chunks.into_iter().flatten().collect();
            prop_assert_eq!(reconstructed, data);
        }

        /// Multiple consecutive block matches preserve order and indices.
        #[test]
        fn consecutive_block_matches(
            indices in prop::collection::vec(0u32..=0x7FFF_FFFEu32, 1..=20),
        ) {
            let ops: Vec<DeltaOp> = indices
                .iter()
                .map(|&idx| DeltaOp::Copy {
                    block_index: idx,
                    length: 4096,
                })
                .collect();

            let mut buf = Vec::new();
            write_token_stream(&mut buf, &ops).unwrap();

            let (literals, blocks) = decode_token_stream(&buf).unwrap();
            prop_assert!(literals.is_empty());
            prop_assert_eq!(blocks, indices);
        }

        /// Multiple consecutive literals reconstruct the concatenated payload.
        #[test]
        fn consecutive_literals(
            payloads in prop::collection::vec(
                prop::collection::vec(any::<u8>(), 1..=256),
                1..=10,
            ),
        ) {
            let ops: Vec<DeltaOp> = payloads.iter().cloned().map(DeltaOp::Literal).collect();

            let mut buf = Vec::new();
            write_token_stream(&mut buf, &ops).unwrap();

            let (chunks, blocks) = decode_token_stream(&buf).unwrap();
            prop_assert!(blocks.is_empty());

            let expected: Vec<u8> = payloads.into_iter().flatten().collect();
            let actual: Vec<u8> = chunks.into_iter().flatten().collect();
            prop_assert_eq!(actual, expected);
        }

        /// Alternating Literal-Copy pattern preserves interleaving semantics.
        #[test]
        fn alternating_literal_copy(
            count in 1usize..=8,
            seed in any::<u8>(),
        ) {
            let mut ops = Vec::with_capacity(count * 2);
            for i in 0..count {
                ops.push(DeltaOp::Literal(vec![seed.wrapping_add(i as u8); 64]));
                ops.push(DeltaOp::Copy {
                    block_index: i as u32,
                    length: 4096,
                });
            }

            let mut buf = Vec::new();
            write_token_stream(&mut buf, &ops).unwrap();

            let (literal_chunks, block_indices) = decode_token_stream(&buf).unwrap();

            // Each literal is 64 bytes (fits in one chunk).
            let mut expected_literals = Vec::new();
            let mut expected_blocks = Vec::new();
            for i in 0..count {
                expected_literals
                    .extend_from_slice(&vec![seed.wrapping_add(i as u8); 64]);
                expected_blocks.push(i as u32);
            }

            let actual_literals: Vec<u8> = literal_chunks.into_iter().flatten().collect();
            prop_assert_eq!(actual_literals, expected_literals);
            prop_assert_eq!(block_indices, expected_blocks);
        }
    }
}
