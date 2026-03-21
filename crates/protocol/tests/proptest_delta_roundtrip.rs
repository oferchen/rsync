//! Property-based tests for delta script encode/decode roundtrip.
//!
//! Verifies that both the internal opcode-based format and the upstream
//! token-based wire format correctly roundtrip `DeltaOp` sequences through
//! encode then decode.

use proptest::prelude::*;
use protocol::wire::{
    DeltaOp, read_delta, read_delta_op, read_token, write_delta, write_delta_op,
    write_token_block_match, write_token_end, write_token_literal, write_token_stream,
};
use std::io::Cursor;

// ---------------------------------------------------------------------------
// Proptest strategies for DeltaOp generation
// ---------------------------------------------------------------------------

/// Strategy for generating arbitrary literal data (0..=4096 bytes).
fn arb_literal() -> impl Strategy<Value = DeltaOp> {
    prop::collection::vec(any::<u8>(), 0..=4096).prop_map(DeltaOp::Literal)
}

/// Strategy for generating arbitrary Copy operations.
///
/// Block index and length are constrained to non-negative i32 range because the
/// internal format encodes them via varint (i32).
fn arb_copy() -> impl Strategy<Value = DeltaOp> {
    (0u32..=i32::MAX as u32, 1u32..=i32::MAX as u32).prop_map(|(block_index, length)| {
        DeltaOp::Copy {
            block_index,
            length,
        }
    })
}

/// Strategy for generating an arbitrary `DeltaOp`.
fn arb_delta_op() -> impl Strategy<Value = DeltaOp> {
    prop_oneof![arb_literal(), arb_copy(),]
}

/// Strategy for generating a vector of arbitrary delta operations.
fn arb_delta_ops() -> impl Strategy<Value = Vec<DeltaOp>> {
    prop::collection::vec(arb_delta_op(), 0..=32)
}

// ---------------------------------------------------------------------------
// Internal format: single DeltaOp roundtrip
// ---------------------------------------------------------------------------

proptest! {
    /// A single Copy operation roundtrips through the internal opcode format.
    #[test]
    fn internal_copy_roundtrip(
        block_index in 0u32..=i32::MAX as u32,
        length in 1u32..=i32::MAX as u32,
    ) {
        let op = DeltaOp::Copy { block_index, length };
        let mut buf = Vec::new();
        write_delta_op(&mut buf, &op)?;

        let mut cursor = Cursor::new(&buf);
        let decoded = read_delta_op(&mut cursor)?;
        prop_assert_eq!(decoded, op);
        prop_assert_eq!(cursor.position() as usize, buf.len());
    }

    /// A single Literal operation roundtrips through the internal opcode format.
    #[test]
    fn internal_literal_roundtrip(data in prop::collection::vec(any::<u8>(), 0..=4096)) {
        let op = DeltaOp::Literal(data);
        let mut buf = Vec::new();
        write_delta_op(&mut buf, &op)?;

        let mut cursor = Cursor::new(&buf);
        let decoded = read_delta_op(&mut cursor)?;
        prop_assert_eq!(decoded, op);
        prop_assert_eq!(cursor.position() as usize, buf.len());
    }
}

// ---------------------------------------------------------------------------
// Internal format: complete delta stream roundtrip
// ---------------------------------------------------------------------------

proptest! {
    /// A complete delta stream roundtrips through the internal format.
    #[test]
    fn internal_delta_stream_roundtrip(ops in arb_delta_ops()) {
        let mut buf = Vec::new();
        write_delta(&mut buf, &ops)?;

        let mut cursor = Cursor::new(&buf);
        let decoded = read_delta(&mut cursor)?;
        prop_assert_eq!(decoded, ops);
        prop_assert_eq!(cursor.position() as usize, buf.len());
    }

    /// An empty delta stream roundtrips through the internal format.
    #[test]
    fn internal_empty_stream_roundtrip(_dummy in 0u8..1u8) {
        let ops: Vec<DeltaOp> = Vec::new();
        let mut buf = Vec::new();
        write_delta(&mut buf, &ops)?;

        let mut cursor = Cursor::new(&buf);
        let decoded = read_delta(&mut cursor)?;
        prop_assert_eq!(decoded, ops);
    }
}

// ---------------------------------------------------------------------------
// Upstream token format: individual token roundtrips
// ---------------------------------------------------------------------------

proptest! {
    /// A literal written in token format reads back correctly.
    ///
    /// The token format chunks large literals into CHUNK_SIZE pieces, so the
    /// roundtrip reassembles chunks into the original data.
    #[test]
    fn token_literal_roundtrip(data in prop::collection::vec(any::<u8>(), 1..=65536)) {
        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data)?;
        write_token_end(&mut buf)?;

        let mut cursor = Cursor::new(&buf);
        let mut reassembled = Vec::new();

        loop {
            let token = read_token(&mut cursor)?;
            match token {
                None => break,
                Some(n) if n > 0 => {
                    let mut chunk = vec![0u8; n as usize];
                    std::io::Read::read_exact(&mut cursor, &mut chunk)?;
                    reassembled.extend_from_slice(&chunk);
                }
                Some(n) => {
                    // Negative = block match, unexpected here
                    prop_assert!(false, "unexpected block match token {}", n);
                }
            }
        }
        prop_assert_eq!(reassembled, data);
    }

    /// A block match token roundtrips: block_index encodes as -(index+1).
    #[test]
    fn token_block_match_roundtrip(block_index in 0u32..=i32::MAX as u32 - 1) {
        let mut buf = Vec::new();
        write_token_block_match(&mut buf, block_index)?;

        let mut cursor = Cursor::new(&buf);
        let token = read_token(&mut cursor)?.unwrap();

        // Token encoding: -(block_index + 1)
        let decoded_index = (-(token + 1)) as u32;
        prop_assert_eq!(decoded_index, block_index);
    }
}

// ---------------------------------------------------------------------------
// Upstream token format: complete stream roundtrip
// ---------------------------------------------------------------------------

/// Reads a full token stream back into `DeltaOp` values.
///
/// Since the token format does not carry `length` for Copy operations (it is
/// determined by the checksum header's block size), we record `length = 0` for
/// decoded Copy ops and compare accordingly.
fn read_token_stream_to_ops(data: &[u8]) -> std::io::Result<Vec<DeltaOp>> {
    let mut cursor = Cursor::new(data);
    let mut ops = Vec::new();

    loop {
        let token = read_token(&mut cursor)?;
        match token {
            None => break,
            Some(n) if n > 0 => {
                let mut chunk = vec![0u8; n as usize];
                std::io::Read::read_exact(&mut cursor, &mut chunk)?;
                // Accumulate consecutive literal chunks into one Literal op
                match ops.last_mut() {
                    Some(DeltaOp::Literal(existing)) => {
                        existing.extend_from_slice(&chunk);
                    }
                    _ => {
                        ops.push(DeltaOp::Literal(chunk));
                    }
                }
            }
            Some(n) => {
                let block_index = (-(n + 1)) as u32;
                ops.push(DeltaOp::Copy {
                    block_index,
                    length: 0,
                });
            }
        }
    }

    Ok(ops)
}

/// Normalizes ops for token-format comparison: merges consecutive literals,
/// replaces Copy length with 0 (not preserved in token format), and removes
/// empty literals.
fn normalize_for_token_comparison(ops: &[DeltaOp]) -> Vec<DeltaOp> {
    let mut result: Vec<DeltaOp> = Vec::new();
    for op in ops {
        match op {
            DeltaOp::Literal(data) if data.is_empty() => {
                // Token format skips zero-length literals
                continue;
            }
            DeltaOp::Literal(data) => match result.last_mut() {
                Some(DeltaOp::Literal(existing)) => {
                    existing.extend_from_slice(data);
                }
                _ => {
                    result.push(DeltaOp::Literal(data.clone()));
                }
            },
            DeltaOp::Copy { block_index, .. } => {
                result.push(DeltaOp::Copy {
                    block_index: *block_index,
                    length: 0,
                });
            }
        }
    }
    result
}

proptest! {
    /// A mixed delta stream roundtrips through the upstream token format.
    ///
    /// The token format merges consecutive literal chunks and does not preserve
    /// Copy length, so we normalize before comparison.
    #[test]
    fn token_stream_roundtrip(ops in arb_delta_ops()) {
        let mut buf = Vec::new();
        write_token_stream(&mut buf, &ops)?;

        let decoded = read_token_stream_to_ops(&buf)?;
        let expected = normalize_for_token_comparison(&ops);
        prop_assert_eq!(decoded, expected);
    }

    /// An empty delta stream produces only the end marker in token format.
    #[test]
    fn token_empty_stream_roundtrip(_dummy in 0u8..1u8) {
        let ops: Vec<DeltaOp> = Vec::new();
        let mut buf = Vec::new();
        write_token_stream(&mut buf, &ops)?;

        // Should be exactly 4 bytes: the end marker (write_int(0))
        prop_assert_eq!(buf.len(), 4);
        let decoded = read_token_stream_to_ops(&buf)?;
        prop_assert!(decoded.is_empty());
    }
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

proptest! {
    /// Zero-length literal roundtrips through the internal format.
    #[test]
    fn internal_zero_length_literal_roundtrip(_dummy in 0u8..1u8) {
        let op = DeltaOp::Literal(Vec::new());
        let mut buf = Vec::new();
        write_delta_op(&mut buf, &op)?;

        let mut cursor = Cursor::new(&buf);
        let decoded = read_delta_op(&mut cursor)?;
        prop_assert_eq!(decoded, op);
    }

    /// Maximum block index roundtrips through the internal format.
    #[test]
    fn internal_max_block_index_roundtrip(length in 1u32..=i32::MAX as u32) {
        let op = DeltaOp::Copy {
            block_index: i32::MAX as u32,
            length,
        };
        let mut buf = Vec::new();
        write_delta_op(&mut buf, &op)?;

        let mut cursor = Cursor::new(&buf);
        let decoded = read_delta_op(&mut cursor)?;
        prop_assert_eq!(decoded, op);
    }

    /// Encoding is deterministic: same input always produces same bytes.
    #[test]
    fn internal_encoding_deterministic(ops in arb_delta_ops()) {
        let mut enc1 = Vec::new();
        let mut enc2 = Vec::new();
        write_delta(&mut enc1, &ops)?;
        write_delta(&mut enc2, &ops)?;
        prop_assert_eq!(enc1, enc2);
    }

    /// Token encoding is deterministic: same input always produces same bytes.
    #[test]
    fn token_encoding_deterministic(ops in arb_delta_ops()) {
        let mut enc1 = Vec::new();
        let mut enc2 = Vec::new();
        write_token_stream(&mut enc1, &ops)?;
        write_token_stream(&mut enc2, &ops)?;
        prop_assert_eq!(enc1, enc2);
    }

    /// Large literal data (> CHUNK_SIZE) roundtrips through token format.
    #[test]
    fn token_large_literal_roundtrip(
        data in prop::collection::vec(any::<u8>(), 32768..=65536)
    ) {
        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data)?;
        write_token_end(&mut buf)?;

        let mut cursor = Cursor::new(&buf);
        let mut reassembled = Vec::new();

        loop {
            let token = read_token(&mut cursor)?;
            match token {
                None => break,
                Some(n) if n > 0 => {
                    let mut chunk = vec![0u8; n as usize];
                    std::io::Read::read_exact(&mut cursor, &mut chunk)?;
                    reassembled.extend_from_slice(&chunk);
                }
                Some(_) => prop_assert!(false, "unexpected block match in literal-only stream"),
            }
        }
        prop_assert_eq!(reassembled, data);
    }
}
