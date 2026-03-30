// `| 0` used deliberately in wire format constants to document the offset field.
#![allow(clippy::identity_op)]
#![cfg(feature = "lz4")]
//! Golden byte tests for LZ4 compressed token wire format.
//!
//! Verifies that our LZ4 encoder/decoder produces and consumes wire bytes
//! that match upstream rsync's CPRES_LZ4 mode (token.c:send_compressed_token
//! / recv_compressed_token, SUPPORT_LZ4 variant).
//!
//! ## Wire format (shared with zlib/zstd, upstream token.c lines 321-329)
//!
//! ```text
//! END_FLAG      = 0x00  - end of file marker
//! TOKEN_LONG    = 0x20  - followed by 32-bit LE token number
//! TOKENRUN_LONG = 0x21  - followed by 32-bit LE token + 16-bit LE run count
//! DEFLATED_DATA = 0x40  - + 6-bit high len, then low len byte, then compressed data
//! TOKEN_REL     = 0x80  - + 6-bit relative token number
//! TOKENRUN_REL  = 0xC0  - + 6-bit relative token + 16-bit LE run count
//! ```
//!
//! ## LZ4-specific behavior (upstream token.c lines 881-1027)
//!
//! - Each chunk compressed independently via `LZ4_compress_default`
//! - No persistent compression state between chunks (stateless)
//! - Literals buffered until token boundary, then compressed and emitted
//! - If compressed output exceeds MAX_DATA_COUNT, input is halved and retried
//! - `see_token` is a noop (no dictionary synchronization)

use std::io::{Cursor, Read};

use protocol::wire::{
    CompressedToken, CompressedTokenDecoder, CompressedTokenEncoder, DEFLATED_DATA, END_FLAG,
    MAX_DATA_COUNT, TOKEN_LONG, TOKEN_REL, TOKENRUN_LONG, TOKENRUN_REL,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Decodes all tokens from an LZ4-encoded byte buffer.
fn lz4_decode_all(data: &[u8]) -> (Vec<u8>, Vec<u32>) {
    let mut cursor = Cursor::new(data);
    let mut decoder = CompressedTokenDecoder::new_lz4();
    let mut literals = Vec::new();
    let mut blocks = Vec::new();

    loop {
        match decoder.recv_token(&mut cursor).unwrap() {
            CompressedToken::Literal(chunk) => literals.extend_from_slice(&chunk),
            CompressedToken::BlockMatch(idx) => blocks.push(idx),
            CompressedToken::End => break,
        }
    }

    (literals, blocks)
}

/// Encodes tokens using LZ4 and returns the raw wire bytes.
fn lz4_encode(tokens: &[Lz4TestToken]) -> Vec<u8> {
    let mut encoder = CompressedTokenEncoder::new_lz4();
    let mut output = Vec::new();

    for token in tokens {
        match token {
            Lz4TestToken::Literal(data) => encoder.send_literal(&mut output, data).unwrap(),
            Lz4TestToken::BlockMatch(idx) => encoder.send_block_match(&mut output, *idx).unwrap(),
        }
    }

    encoder.finish(&mut output).unwrap();
    output
}

#[derive(Clone)]
enum Lz4TestToken {
    Literal(Vec<u8>),
    BlockMatch(u32),
}

/// Parses wire bytes into a sequence of labeled elements for structural assertions.
/// Returns (element_labels, deflated_block_sizes).
fn parse_wire_structure(data: &[u8]) -> (Vec<&'static str>, Vec<usize>) {
    let mut cursor = Cursor::new(data);
    let mut sequence = Vec::new();
    let mut block_sizes = Vec::new();

    loop {
        let mut flag_buf = [0u8; 1];
        if cursor.read_exact(&mut flag_buf).is_err() {
            break;
        }
        let flag = flag_buf[0];

        if (flag & 0xC0) == DEFLATED_DATA {
            let high = (flag & 0x3F) as usize;
            let mut low_buf = [0u8; 1];
            cursor.read_exact(&mut low_buf).unwrap();
            let len = (high << 8) | (low_buf[0] as usize);
            block_sizes.push(len);
            // Skip compressed data
            let pos = cursor.position() as usize;
            cursor.set_position((pos + len) as u64);
            sequence.push("DEFLATED_DATA");
        } else if flag == END_FLAG {
            sequence.push("END");
            break;
        } else if flag & 0x80 != 0 {
            // TOKEN_REL or TOKENRUN_REL
            if flag & 0xC0 == TOKENRUN_REL {
                let mut run_buf = [0u8; 2];
                cursor.read_exact(&mut run_buf).unwrap();
                sequence.push("TOKENRUN_REL");
            } else {
                sequence.push("TOKEN_REL");
            }
        } else if flag & 0xE0 == TOKEN_LONG {
            let mut buf = [0u8; 4];
            cursor.read_exact(&mut buf).unwrap();
            if flag & 1 != 0 {
                // TOKENRUN_LONG
                let mut run_buf = [0u8; 2];
                cursor.read_exact(&mut run_buf).unwrap();
                sequence.push("TOKENRUN_LONG");
            } else {
                sequence.push("TOKEN_LONG");
            }
        }
    }

    (sequence, block_sizes)
}

// ===========================================================================
// Section 1: Literal-only streams
// ===========================================================================

/// A literal-only LZ4 stream must produce: DEFLATED_DATA block(s) + END_FLAG.
/// The DEFLATED_DATA header uses the same 14-bit length encoding as zlib/zstd.
/// LZ4 compresses each chunk independently - no persistent compression state.
///
/// upstream: token.c:send_compressed_token() lines 919-948 (SUPPORT_LZ4)
#[test]
fn golden_lz4_literal_only_wire_structure() {
    let encoded = lz4_encode(&[Lz4TestToken::Literal(
        b"Hello from LZ4 compressed token stream!".to_vec(),
    )]);

    let (sequence, block_sizes) = parse_wire_structure(&encoded);

    // Must be: one or more DEFLATED_DATA blocks followed by END
    assert!(
        sequence.len() >= 2,
        "expected at least DEFLATED_DATA + END, got {sequence:?}"
    );
    for label in &sequence[..sequence.len() - 1] {
        assert_eq!(
            *label, "DEFLATED_DATA",
            "all elements before END must be DEFLATED_DATA, got {label}"
        );
    }
    assert_eq!(*sequence.last().unwrap(), "END");

    // All blocks must respect MAX_DATA_COUNT
    for (i, &size) in block_sizes.iter().enumerate() {
        assert!(
            size <= MAX_DATA_COUNT,
            "block {i} size {size} exceeds MAX_DATA_COUNT"
        );
        assert!(size > 0, "block {i} must not be empty");
    }

    // Verify the last byte is END_FLAG (0x00)
    assert_eq!(encoded[encoded.len() - 1], END_FLAG);

    // Roundtrip verification
    let (literals, blocks) = lz4_decode_all(&encoded);
    assert_eq!(literals, b"Hello from LZ4 compressed token stream!");
    assert!(blocks.is_empty());
}

/// Verify exact DEFLATED_DATA header bytes for a small LZ4 literal.
/// The header format is: byte0 = DEFLATED_DATA | (len >> 8), byte1 = len & 0xFF.
///
/// upstream: token.c lines 938-939
#[test]
fn golden_lz4_deflated_data_header_exact_bytes() {
    let encoded = lz4_encode(&[Lz4TestToken::Literal(b"test".to_vec())]);

    // First two bytes are the DEFLATED_DATA header
    let byte0 = encoded[0];
    let byte1 = encoded[1];

    // Verify flag bits
    assert_eq!(
        byte0 & 0xC0,
        DEFLATED_DATA,
        "first byte must have DEFLATED_DATA flag (0x40)"
    );

    // Decode length from header
    let high = (byte0 & 0x3F) as usize;
    let low = byte1 as usize;
    let compressed_len = (high << 8) | low;

    // The compressed data must follow immediately after the 2-byte header
    assert!(
        encoded.len() >= 3 + compressed_len,
        "encoded data too short for declared length + END_FLAG"
    );

    // After compressed payload, END_FLAG
    assert_eq!(
        encoded[2 + compressed_len],
        END_FLAG,
        "END_FLAG must follow the compressed payload"
    );
}

/// LZ4 compressed output for small inputs typically expands slightly due to
/// the LZ4 frame overhead. Unlike zlib/zstd which can achieve compression on
/// small inputs, LZ4 block compression adds a fixed overhead. The compressed
/// payload must still be valid LZ4 data that roundtrips correctly.
///
/// upstream: token.c line 924 - LZ4_compress_default
#[test]
fn golden_lz4_payload_is_valid_lz4_data() {
    let input = b"Verify this data produces valid LZ4 compressed bytes on the wire";
    let encoded = lz4_encode(&[Lz4TestToken::Literal(input.to_vec())]);

    // Extract compressed payload from first DEFLATED_DATA block
    assert_eq!(encoded[0] & 0xC0, DEFLATED_DATA);
    let high = (encoded[0] & 0x3F) as usize;
    let low = encoded[1] as usize;
    let compressed_len = (high << 8) | low;
    let payload = &encoded[2..2 + compressed_len];

    // Payload must not be empty
    assert!(
        !payload.is_empty(),
        "LZ4 compressed payload must not be empty"
    );

    // Verify the full stream decodes correctly (proves payload validity)
    let (literals, _) = lz4_decode_all(&encoded);
    assert_eq!(literals, input);
}

// ===========================================================================
// Section 2: Block-match-only streams
// ===========================================================================

/// A single block match at index 0 produces TOKEN_REL | 0 + END_FLAG.
/// Token encoding is shared across all compression algorithms.
///
/// upstream: token.c lines 889-915 (LZ4 uses same run encoding as zlib/zstd)
#[test]
fn golden_lz4_single_block_match_token_rel() {
    let encoded = lz4_encode(&[Lz4TestToken::BlockMatch(0)]);

    // TOKEN_REL | 0 = 0x80, END_FLAG = 0x00
    assert_eq!(encoded.len(), 2, "single block match: TOKEN_REL + END_FLAG");
    assert_eq!(encoded[0], TOKEN_REL | 0);
    assert_eq!(encoded[1], END_FLAG);
}

/// Block match at index 42 (within relative range 0-63) uses TOKEN_REL.
/// r = run_start(42) - last_run_end(0) = 42, fits in 6 bits.
#[test]
fn golden_lz4_block_match_rel_42() {
    let encoded = lz4_encode(&[Lz4TestToken::BlockMatch(42)]);

    assert_eq!(encoded.len(), 2);
    assert_eq!(encoded[0], TOKEN_REL | 42);
    assert_eq!(encoded[1], END_FLAG);
}

/// Block match at index 100 (> 63) requires TOKEN_LONG absolute encoding.
/// r = 100 - 0 = 100 > 63, so TOKEN_LONG + 4-byte LE index.
///
/// upstream: token.c line 391 TOKEN_LONG path
#[test]
fn golden_lz4_block_match_token_long() {
    let encoded = lz4_encode(&[Lz4TestToken::BlockMatch(100)]);

    // TOKEN_LONG (0x20) + 4-byte LE index + END_FLAG
    assert_eq!(encoded.len(), 6);
    assert_eq!(encoded[0], TOKEN_LONG);
    assert_eq!(encoded[1..5], 100i32.to_le_bytes());
    assert_eq!(encoded[5], END_FLAG);
}

/// Non-consecutive block matches use separate TOKEN_REL encodings.
/// After block 0, last_run_end = 0. Block 5: r = 5 - 0 = 5 (fits in 6 bits).
#[test]
fn golden_lz4_non_consecutive_blocks_separate_tokens() {
    let encoded = lz4_encode(&[Lz4TestToken::BlockMatch(0), Lz4TestToken::BlockMatch(5)]);

    assert_eq!(encoded[0], TOKEN_REL | 0);
    assert_eq!(encoded[1], TOKEN_REL | 5);
    assert_eq!(encoded[2], END_FLAG);
}

// ===========================================================================
// Section 3: Token run encoding (consecutive block matches)
// ===========================================================================

/// Consecutive blocks 0,1,2 use TOKENRUN_REL encoding.
/// run_start=0, last_token=2, n=2, r=0. All fit in relative encoding.
///
/// upstream: token.c lines 889-915 (LZ4 uses same run detection as zlib/zstd)
#[test]
fn golden_lz4_consecutive_blocks_tokenrun_rel() {
    let encoded = lz4_encode(&[
        Lz4TestToken::BlockMatch(0),
        Lz4TestToken::BlockMatch(1),
        Lz4TestToken::BlockMatch(2),
    ]);

    // TOKENRUN_REL | 0 = 0xC0, n_lo=2, n_hi=0, END_FLAG
    assert_eq!(encoded.len(), 4);
    assert_eq!(encoded[0], TOKENRUN_REL | 0);
    assert_eq!(encoded[1], 2); // n & 0xFF
    assert_eq!(encoded[2], 0); // n >> 8
    assert_eq!(encoded[3], END_FLAG);
}

/// Consecutive blocks starting at 100 use TOKENRUN_LONG (r > 63).
/// run_start=100, last_token=101, n=1, r=100 > 63.
///
/// upstream: token.c lines 391-397
#[test]
fn golden_lz4_consecutive_blocks_tokenrun_long() {
    let encoded = lz4_encode(&[Lz4TestToken::BlockMatch(100), Lz4TestToken::BlockMatch(101)]);

    // TOKENRUN_LONG (0x21) + 4-byte LE run_start + 2-byte LE n + END_FLAG
    assert_eq!(encoded.len(), 8);
    assert_eq!(encoded[0], TOKENRUN_LONG);
    assert_eq!(encoded[1..5], 100i32.to_le_bytes());
    assert_eq!(encoded[5], 1); // n & 0xFF
    assert_eq!(encoded[6], 0); // n >> 8
    assert_eq!(encoded[7], END_FLAG);
}

/// Four consecutive blocks 10,11,12,13: run_start=10, n=3, r=10.
/// 10 fits in 6 bits, so TOKENRUN_REL.
#[test]
fn golden_lz4_four_consecutive_tokenrun_rel() {
    let encoded = lz4_encode(&[
        Lz4TestToken::BlockMatch(10),
        Lz4TestToken::BlockMatch(11),
        Lz4TestToken::BlockMatch(12),
        Lz4TestToken::BlockMatch(13),
    ]);

    // TOKENRUN_REL | 10, n_lo=3, n_hi=0, END_FLAG
    assert_eq!(encoded.len(), 4);
    assert_eq!(encoded[0], TOKENRUN_REL | 10);
    assert_eq!(encoded[1], 3); // n & 0xFF
    assert_eq!(encoded[2], 0); // n >> 8
    assert_eq!(encoded[3], END_FLAG);
}

/// Run followed by a separate block: blocks 0,1,2 then 10.
/// First run: TOKENRUN_REL | 0, n=2.
/// Second: TOKEN_REL | (10 - 2) = TOKEN_REL | 8.
/// (last_run_end = last_token = 2, r = 10 - 2 = 8)
#[test]
fn golden_lz4_run_then_separate_block() {
    let encoded = lz4_encode(&[
        Lz4TestToken::BlockMatch(0),
        Lz4TestToken::BlockMatch(1),
        Lz4TestToken::BlockMatch(2),
        Lz4TestToken::BlockMatch(10),
    ]);

    // TOKENRUN_REL | 0 (0xC0), n=2 (LE16), TOKEN_REL | 8 (0x88), END_FLAG
    assert_eq!(encoded.len(), 5);
    assert_eq!(encoded[0], TOKENRUN_REL | 0);
    assert_eq!(encoded[1], 2); // n & 0xFF
    assert_eq!(encoded[2], 0); // n >> 8
    assert_eq!(encoded[3], TOKEN_REL | 8);
    assert_eq!(encoded[4], END_FLAG);
}

// ===========================================================================
// Section 4: Mixed literal + block match streams
// ===========================================================================

/// Literal data followed by a block match: DEFLATED_DATA + TOKEN_REL + END_FLAG.
/// Literals are compressed and emitted at the token boundary (block match).
///
/// upstream: token.c lines 919-948 - compress_and_emit before writing token
#[test]
fn golden_lz4_mixed_literal_then_block() {
    let encoded = lz4_encode(&[
        Lz4TestToken::Literal(b"literal before block".to_vec()),
        Lz4TestToken::BlockMatch(0),
    ]);

    let (sequence, block_sizes) = parse_wire_structure(&encoded);

    // Structure: DEFLATED_DATA(s), TOKEN_REL, END
    assert!(sequence.len() >= 3, "expected DEFLATED_DATA + TOKEN + END");
    assert_eq!(sequence[0], "DEFLATED_DATA");
    assert_eq!(sequence[sequence.len() - 2], "TOKEN_REL");
    assert_eq!(sequence[sequence.len() - 1], "END");

    // All DEFLATED_DATA blocks must respect MAX_DATA_COUNT
    for &size in &block_sizes {
        assert!(size <= MAX_DATA_COUNT);
        assert!(size > 0);
    }

    // Roundtrip verification
    let (literals, blocks) = lz4_decode_all(&encoded);
    assert_eq!(literals, b"literal before block");
    assert_eq!(blocks, vec![0]);
}

/// Block match followed by literal data: TOKEN_REL + DEFLATED_DATA + END_FLAG.
/// The block match with no preceding literals produces no DEFLATED_DATA.
/// The subsequent literal is flushed at finish().
#[test]
fn golden_lz4_mixed_block_then_literal() {
    let encoded = lz4_encode(&[
        Lz4TestToken::BlockMatch(0),
        Lz4TestToken::Literal(b"literal after block".to_vec()),
    ]);

    let (sequence, _) = parse_wire_structure(&encoded);

    // Structure: TOKEN_REL, DEFLATED_DATA(s), END
    assert!(sequence.len() >= 3);
    assert_eq!(sequence[0], "TOKEN_REL");
    assert_eq!(
        sequence[1], "DEFLATED_DATA",
        "literal after block must produce DEFLATED_DATA"
    );
    assert_eq!(*sequence.last().unwrap(), "END");

    // Roundtrip
    let (literals, blocks) = lz4_decode_all(&encoded);
    assert_eq!(literals, b"literal after block");
    assert_eq!(blocks, vec![0]);
}

/// Interleaved pattern: lit, block, lit, block, lit, end.
/// Each literal is flushed before its following token.
///
/// upstream: token.c lines 889-948 - has_literals triggers compress_and_emit
#[test]
fn golden_lz4_interleaved_literal_block_literal() {
    let encoded = lz4_encode(&[
        Lz4TestToken::Literal(b"first".to_vec()),
        Lz4TestToken::BlockMatch(0),
        Lz4TestToken::Literal(b"second".to_vec()),
        Lz4TestToken::BlockMatch(5),
        Lz4TestToken::Literal(b"third".to_vec()),
    ]);

    let (sequence, _) = parse_wire_structure(&encoded);

    // Verify ordering: DEFLATED_DATA TOKEN DEFLATED_DATA TOKEN DEFLATED_DATA END
    let mut deflated_count = 0;
    let mut token_count = 0;
    for label in &sequence {
        match *label {
            "DEFLATED_DATA" => deflated_count += 1,
            "TOKEN_REL" | "TOKEN_LONG" | "TOKENRUN_REL" | "TOKENRUN_LONG" => token_count += 1,
            "END" => {}
            other => panic!("unexpected wire element: {other}"),
        }
    }
    assert!(
        deflated_count >= 3,
        "3 literals should produce at least 3 DEFLATED_DATA blocks, got {deflated_count}"
    );
    assert_eq!(token_count, 2, "should have exactly 2 token elements");
    assert_eq!(*sequence.last().unwrap(), "END");

    // Roundtrip
    let (literals, blocks) = lz4_decode_all(&encoded);
    assert_eq!(literals, b"firstsecondthird");
    assert_eq!(blocks, vec![0, 5]);
}

/// Complex mixed stream with consecutive blocks (run encoding) interleaved
/// with literals.
#[test]
fn golden_lz4_mixed_with_run_encoding() {
    let encoded = lz4_encode(&[
        Lz4TestToken::Literal(b"prefix data".to_vec()),
        Lz4TestToken::BlockMatch(0),
        Lz4TestToken::BlockMatch(1),
        Lz4TestToken::BlockMatch(2),
        Lz4TestToken::Literal(b"suffix data".to_vec()),
    ]);

    let (sequence, _) = parse_wire_structure(&encoded);

    // The 3 consecutive blocks should produce TOKENRUN_REL
    let has_tokenrun = sequence
        .iter()
        .any(|s| *s == "TOKENRUN_REL" || *s == "TOKENRUN_LONG");
    assert!(
        has_tokenrun,
        "consecutive blocks should use run encoding, got {sequence:?}"
    );
    assert_eq!(*sequence.last().unwrap(), "END");

    // Roundtrip
    let (literals, blocks) = lz4_decode_all(&encoded);
    assert_eq!(literals, b"prefix datasuffix data");
    assert_eq!(blocks, vec![0, 1, 2]);
}

// ===========================================================================
// Section 5: DEFLATED_DATA framing and flush boundaries
// ===========================================================================

/// Small literals should produce exactly one DEFLATED_DATA block per flush.
/// LZ4 compresses the entire buffered literal data in a single
/// `LZ4_compress_default` call; small inputs fit in one DEFLATED_DATA block.
///
/// upstream: token.c lines 923-946 - compress literal chunk
#[test]
fn golden_lz4_small_literal_single_deflated_block() {
    let encoded = lz4_encode(&[
        Lz4TestToken::Literal(b"small input".to_vec()),
        Lz4TestToken::BlockMatch(0),
    ]);

    // Count DEFLATED_DATA blocks before the token
    let (sequence, _) = parse_wire_structure(&encoded);
    let deflated_before_token = sequence
        .iter()
        .take_while(|s| **s == "DEFLATED_DATA")
        .count();

    assert_eq!(
        deflated_before_token, 1,
        "small literal should produce exactly one DEFLATED_DATA block before the token"
    );
}

/// Large incompressible data must split into multiple DEFLATED_DATA blocks,
/// each at most MAX_DATA_COUNT (16383) bytes. LZ4 block compression on random
/// data typically expands slightly, triggering the halving retry loop and
/// multiple output blocks.
///
/// upstream: token.c lines 927-946 - compress in MAX_DATA_COUNT chunks with halving
#[test]
fn golden_lz4_large_literal_multiple_deflated_blocks() {
    // Generate incompressible data using xorshift64
    let mut state: u64 = 0xCAFE_BABE_DEAD_BEEF;
    let data: Vec<u8> = (0..200_000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state & 0xFF) as u8
        })
        .collect();

    let encoded = lz4_encode(&[Lz4TestToken::Literal(data.clone())]);
    let (_, block_sizes) = parse_wire_structure(&encoded);

    // Must produce multiple blocks
    assert!(
        block_sizes.len() > 1,
        "200KB incompressible data should produce multiple DEFLATED_DATA blocks, got {}",
        block_sizes.len()
    );

    // All blocks within MAX_DATA_COUNT
    for (i, &size) in block_sizes.iter().enumerate() {
        assert!(
            size <= MAX_DATA_COUNT,
            "block {i} size {size} exceeds MAX_DATA_COUNT ({MAX_DATA_COUNT})"
        );
        assert!(size > 0, "block {i} must not be empty");
    }

    // Roundtrip
    let (literals, blocks) = lz4_decode_all(&encoded);
    assert_eq!(literals, data);
    assert!(blocks.is_empty());
}

/// Verify END_FLAG is always a single 0x00 byte at the end of the stream.
/// Shared across all compression algorithms.
///
/// upstream: token.c line 462 write_byte(f, END_FLAG)
#[test]
fn golden_lz4_end_flag_single_zero_byte() {
    // Empty stream (no literals, no blocks)
    let encoded = lz4_encode(&[]);
    assert_eq!(encoded.len(), 1, "empty LZ4 stream should be just END_FLAG");
    assert_eq!(encoded[0], END_FLAG);
}

/// The END_FLAG byte (0x00) is distinct from all other flag byte ranges.
/// This is critical for correct protocol parsing.
#[test]
fn golden_lz4_end_flag_not_confused_with_other_flags() {
    assert_eq!(END_FLAG, 0x00);
    assert_ne!(END_FLAG & 0xC0, DEFLATED_DATA);
    assert_ne!(END_FLAG, TOKEN_LONG);
    assert_ne!(END_FLAG, TOKENRUN_LONG);
    assert_ne!(END_FLAG & 0x80, TOKEN_REL);
    assert_ne!(END_FLAG & 0xC0, TOKENRUN_REL);
}

// ===========================================================================
// Section 6: LZ4-specific compressed payload verification
// ===========================================================================

/// LZ4 block compression produces raw LZ4 block data (no framing).
/// Unlike zstd (which has a magic number) or zlib (which has a header byte),
/// LZ4 block-compressed data has no identifying header - it is just the raw
/// compressed bytes from `LZ4_compress_default`.
///
/// upstream: token.c line 924 - LZ4_compress_default(input, obuf+2, ...)
#[test]
fn golden_lz4_payload_is_raw_block_data() {
    let input = b"Raw LZ4 block data without any framing header";
    let encoded = lz4_encode(&[Lz4TestToken::Literal(input.to_vec())]);

    // Extract compressed payload from the DEFLATED_DATA block
    assert_eq!(encoded[0] & 0xC0, DEFLATED_DATA);
    let high = (encoded[0] & 0x3F) as usize;
    let low = encoded[1] as usize;
    let compressed_len = (high << 8) | low;
    let payload = &encoded[2..2 + compressed_len];

    // LZ4 raw block data has no magic number (unlike zstd's 0xFD2FB528)
    // Just verify the payload is non-empty and roundtrips
    assert!(
        !payload.is_empty(),
        "LZ4 compressed payload must not be empty"
    );

    // If payload were zstd it would start with 0x28 0xB5 0x2F 0xFD (LE magic)
    if payload.len() >= 4 {
        let not_zstd =
            payload[0] != 0x28 || payload[1] != 0xB5 || payload[2] != 0x2F || payload[3] != 0xFD;
        assert!(not_zstd, "LZ4 payload must not look like a zstd frame");
    }

    let (literals, _) = lz4_decode_all(&encoded);
    assert_eq!(literals, input);
}

/// LZ4 output for small compressible data may be larger than the input
/// (LZ4 block compression has a minimum overhead). Verify the encoder handles
/// this expansion correctly.
///
/// upstream: token.c line 930 - halve input if compressed output too large
#[test]
fn golden_lz4_expansion_on_tiny_input() {
    // Very small input - LZ4 block compression adds overhead
    let input = b"ab";
    let encoded = lz4_encode(&[Lz4TestToken::Literal(input.to_vec())]);

    // Must still produce valid wire format
    let (sequence, _) = parse_wire_structure(&encoded);
    assert_eq!(sequence[0], "DEFLATED_DATA");
    assert_eq!(*sequence.last().unwrap(), "END");

    // Roundtrip
    let (literals, _) = lz4_decode_all(&encoded);
    assert_eq!(literals, input);
}

/// LZ4 compressed output must differ from zlib output for the same input.
/// This verifies the encoder is actually using LZ4, not accidentally falling
/// back to zlib.
#[test]
fn golden_lz4_output_differs_from_zlib() {
    use compress::zlib::CompressionLevel;

    let input = b"Compare LZ4 vs zlib compressed output bytes for this data";

    // Encode with LZ4
    let lz4_encoded = lz4_encode(&[Lz4TestToken::Literal(input.to_vec())]);

    // Encode with zlib
    let mut zlib_encoder = CompressedTokenEncoder::new(CompressionLevel::Default, 31);
    let mut zlib_encoded = Vec::new();
    zlib_encoder.send_literal(&mut zlib_encoded, input).unwrap();
    zlib_encoder.finish(&mut zlib_encoded).unwrap();

    // Both should produce DEFLATED_DATA framing (same header structure)
    assert_eq!(lz4_encoded[0] & 0xC0, DEFLATED_DATA);
    assert_eq!(zlib_encoded[0] & 0xC0, DEFLATED_DATA);

    // But the compressed payloads must differ (different algorithms)
    assert_ne!(
        lz4_encoded, zlib_encoded,
        "LZ4 and zlib encodings must differ"
    );

    // Both must decode to the same input
    let (lz4_literals, _) = lz4_decode_all(&lz4_encoded);

    let mut zlib_cursor = Cursor::new(&zlib_encoded);
    let mut zlib_decoder = CompressedTokenDecoder::new();
    let mut zlib_literals = Vec::new();
    loop {
        match zlib_decoder.recv_token(&mut zlib_cursor).unwrap() {
            CompressedToken::Literal(d) => zlib_literals.extend_from_slice(&d),
            CompressedToken::End => break,
            CompressedToken::BlockMatch(_) => {}
        }
    }

    assert_eq!(lz4_literals, input.as_slice());
    assert_eq!(zlib_literals, input.as_slice());
}

/// LZ4 and zstd compressed output must differ for the same input.
/// Verifies both codecs are distinct implementations.
#[cfg(feature = "zstd")]
#[test]
fn golden_lz4_output_differs_from_zstd() {
    let input = b"Compare LZ4 vs zstd compressed output bytes for this data";

    // Encode with LZ4
    let lz4_encoded = lz4_encode(&[Lz4TestToken::Literal(input.to_vec())]);

    // Encode with zstd
    let mut zstd_encoder = CompressedTokenEncoder::new_zstd(3).unwrap();
    let mut zstd_encoded = Vec::new();
    zstd_encoder.send_literal(&mut zstd_encoded, input).unwrap();
    zstd_encoder.finish(&mut zstd_encoded).unwrap();

    // Both should produce DEFLATED_DATA framing
    assert_eq!(lz4_encoded[0] & 0xC0, DEFLATED_DATA);
    assert_eq!(zstd_encoded[0] & 0xC0, DEFLATED_DATA);

    // But the compressed payloads must differ
    assert_ne!(
        lz4_encoded, zstd_encoded,
        "LZ4 and zstd encodings must differ"
    );

    // Both must decode to the same input
    let (lz4_literals, _) = lz4_decode_all(&lz4_encoded);

    let mut zstd_cursor = Cursor::new(&zstd_encoded);
    let mut zstd_decoder = CompressedTokenDecoder::new_zstd().unwrap();
    let mut zstd_literals = Vec::new();
    loop {
        match zstd_decoder.recv_token(&mut zstd_cursor).unwrap() {
            CompressedToken::Literal(d) => zstd_literals.extend_from_slice(&d),
            CompressedToken::End => break,
            CompressedToken::BlockMatch(_) => {}
        }
    }

    assert_eq!(lz4_literals, input.as_slice());
    assert_eq!(zstd_literals, input.as_slice());
}

// ===========================================================================
// Section 7: Decoder golden byte tests - hand-crafted wire bytes
// ===========================================================================

/// Verify the LZ4 decoder handles a hand-crafted wire stream with
/// only token bytes (no DEFLATED_DATA). Block-match-only streams have
/// identical wire encoding regardless of compression algorithm.
#[test]
fn golden_lz4_decoder_token_only_stream() {
    // TOKEN_REL | 5 (block 5), TOKEN_REL | 3 (block 5+3=8), END_FLAG
    let wire = [TOKEN_REL | 5, TOKEN_REL | 3, END_FLAG];

    let (literals, blocks) = lz4_decode_all(&wire);
    assert!(literals.is_empty());
    assert_eq!(blocks, vec![5, 8]);
}

/// Verify the LZ4 decoder handles TOKENRUN_REL in hand-crafted bytes.
/// TOKENRUN_REL | 0, run_count=4: blocks 0,1,2,3,4.
#[test]
fn golden_lz4_decoder_tokenrun_rel_hand_crafted() {
    let wire = [
        TOKENRUN_REL | 0, // rx_token = 0
        4,
        0, // n=4 (LE 16-bit) -> 4 additional tokens after first
        END_FLAG,
    ];

    let (_, blocks) = lz4_decode_all(&wire);
    assert_eq!(blocks, vec![0, 1, 2, 3, 4]);
}

/// Verify the LZ4 decoder handles TOKEN_LONG in hand-crafted bytes.
/// TOKEN_LONG, index=0x00001000 (4096), END_FLAG.
#[test]
fn golden_lz4_decoder_token_long_hand_crafted() {
    let wire = [
        TOKEN_LONG, 0x00, 0x10, 0x00, 0x00, // run_start=4096 (LE)
        END_FLAG,
    ];

    let (_, blocks) = lz4_decode_all(&wire);
    assert_eq!(blocks, vec![4096]);
}

/// Verify the LZ4 decoder handles TOKENRUN_LONG in hand-crafted bytes.
/// TOKENRUN_LONG, index=200 (LE32), n=3 (LE16): blocks 200,201,202,203.
#[test]
fn golden_lz4_decoder_tokenrun_long_hand_crafted() {
    let wire = [
        TOKENRUN_LONG,
        200,
        0,
        0,
        0, // run_start=200 (LE)
        3,
        0, // n=3 (LE 16-bit)
        END_FLAG,
    ];

    let (_, blocks) = lz4_decode_all(&wire);
    assert_eq!(blocks, vec![200, 201, 202, 203]);
}

/// Verify the LZ4 decoder handles an empty stream (just END_FLAG).
#[test]
fn golden_lz4_decoder_empty_stream() {
    let wire = [END_FLAG];
    let (literals, blocks) = lz4_decode_all(&wire);
    assert!(literals.is_empty());
    assert!(blocks.is_empty());
}

/// Hand-crafted DEFLATED_DATA block with LZ4-compressed payload.
/// Compresses "AAAA" (4 bytes of 0x41) using lz4_flex, wraps in
/// DEFLATED_DATA header, and feeds to the decoder.
#[test]
fn golden_lz4_decoder_hand_crafted_deflated_data() {
    let input = b"AAAA";
    let compressed = lz4_flex::block::compress(input);
    let compressed_len = compressed.len();

    // Build wire: DEFLATED_DATA header + compressed payload + END_FLAG
    let mut wire = Vec::new();
    wire.push(DEFLATED_DATA | ((compressed_len >> 8) as u8));
    wire.push((compressed_len & 0xFF) as u8);
    wire.extend_from_slice(&compressed);
    wire.push(END_FLAG);

    let (literals, blocks) = lz4_decode_all(&wire);
    assert_eq!(literals, input);
    assert!(blocks.is_empty());
}

/// Hand-crafted mixed stream: DEFLATED_DATA + TOKEN_REL + END_FLAG.
/// Verifies the decoder correctly interleaves literal and token parsing.
#[test]
fn golden_lz4_decoder_hand_crafted_mixed() {
    let input = b"hello";
    let compressed = lz4_flex::block::compress(input);
    let compressed_len = compressed.len();

    let mut wire = Vec::new();
    // DEFLATED_DATA header + compressed "hello"
    wire.push(DEFLATED_DATA | ((compressed_len >> 8) as u8));
    wire.push((compressed_len & 0xFF) as u8);
    wire.extend_from_slice(&compressed);
    // TOKEN_REL | 7 (block 7)
    wire.push(TOKEN_REL | 7);
    // END_FLAG
    wire.push(END_FLAG);

    let (literals, blocks) = lz4_decode_all(&wire);
    assert_eq!(literals, b"hello");
    assert_eq!(blocks, vec![7]);
}

// ===========================================================================
// Section 8: Encoder/decoder roundtrip with exact byte verification
// ===========================================================================

/// Verify that encoding then decoding a complex mixed stream preserves
/// all data and block indices exactly.
#[test]
fn golden_lz4_complex_roundtrip() {
    let tokens = vec![
        Lz4TestToken::Literal(b"header data".to_vec()),
        Lz4TestToken::BlockMatch(0),
        Lz4TestToken::BlockMatch(1),
        Lz4TestToken::BlockMatch(2),
        Lz4TestToken::Literal(b"middle data with more content".to_vec()),
        Lz4TestToken::BlockMatch(100),
        Lz4TestToken::Literal(b"trailing data".to_vec()),
    ];

    let encoded = lz4_encode(&tokens);
    let (literals, blocks) = lz4_decode_all(&encoded);

    assert_eq!(
        literals,
        b"header datamiddle data with more contenttrailing data"
    );
    assert_eq!(blocks, vec![0, 1, 2, 100]);
}

/// Verify encoder reset between files produces independent streams.
/// Each file's stream must be self-contained - no cross-file state leaks.
/// LZ4 is stateless per-chunk, but the token run state (last_token,
/// run_start, last_run_end) must reset between files.
///
/// upstream: token.c - new file resets token run tracking
#[test]
fn golden_lz4_reset_produces_independent_streams() {
    let mut encoder = CompressedTokenEncoder::new_lz4();

    for i in 0u8..3 {
        let mut output = Vec::new();
        let data = [b'A' + i; 32];
        encoder.send_literal(&mut output, &data).unwrap();
        encoder.send_block_match(&mut output, i as u32).unwrap();
        encoder.finish(&mut output).unwrap();

        // Each stream must be independently decodable
        let (literals, blocks) = lz4_decode_all(&output);
        assert_eq!(literals, data);
        assert_eq!(blocks, vec![i as u32]);

        // Verify wire structure
        let (sequence, _) = parse_wire_structure(&output);
        assert_eq!(sequence[0], "DEFLATED_DATA");
        assert_eq!(*sequence.last().unwrap(), "END");
    }
}

/// Large run count encoded correctly. 256 consecutive blocks from 0.
/// n = 255, which requires both bytes of the 16-bit LE run count.
#[test]
fn golden_lz4_large_run_count_encoding() {
    let tokens: Vec<Lz4TestToken> = (0..256u32).map(Lz4TestToken::BlockMatch).collect();

    let encoded = lz4_encode(&tokens);

    // Should use TOKENRUN_REL | 0 with n=255
    assert_eq!(encoded[0], TOKENRUN_REL | 0);
    assert_eq!(encoded[1], 255); // n & 0xFF = 255
    assert_eq!(encoded[2], 0); // n >> 8 = 0
    assert_eq!(encoded[3], END_FLAG);

    // Roundtrip
    let (_, blocks) = lz4_decode_all(&encoded);
    let expected: Vec<u32> = (0..256).collect();
    assert_eq!(blocks, expected);
}

/// Run count of 256 requires the high byte of the 16-bit LE count.
/// 257 consecutive blocks: n = 256 = 0x0100.
#[test]
fn golden_lz4_run_count_256_uses_high_byte() {
    let tokens: Vec<Lz4TestToken> = (0..257u32).map(Lz4TestToken::BlockMatch).collect();

    let encoded = lz4_encode(&tokens);

    assert_eq!(encoded[0], TOKENRUN_REL | 0);
    assert_eq!(encoded[1], 0); // n & 0xFF = 0
    assert_eq!(encoded[2], 1); // n >> 8 = 1
    assert_eq!(encoded[3], END_FLAG);

    let (_, blocks) = lz4_decode_all(&encoded);
    let expected: Vec<u32> = (0..257).collect();
    assert_eq!(blocks, expected);
}
