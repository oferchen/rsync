// `| 0` used deliberately in wire format constants to document the offset field.
#![allow(clippy::identity_op)]
#![cfg(feature = "zstd")]
//! Golden byte tests for zstd compressed token wire format.
//!
//! Verifies that our zstd encoder/decoder produces and consumes wire bytes
//! that match upstream rsync's CPRES_ZSTD mode (token.c:send_zstd_token /
//! recv_zstd_token). Unlike zlib, zstd does NOT strip the 4-byte sync marker
//! (0x00 0x00 0xFF 0xFF) and uses `ZSTD_e_flush` at token boundaries.
//!
//! ## Wire format (shared with zlib, upstream token.c lines 321-329)
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
//! ## Zstd-specific behavior (upstream token.c lines 678-776)
//!
//! - No sync marker stripping (zstd frames are self-delimiting)
//! - `ZSTD_e_flush` at every token boundary (block match or EOF)
//! - `ZSTD_e_continue` for literal data between boundaries
//! - Output buffered in MAX_DATA_COUNT-sized buffer, written as DEFLATED_DATA
//!   blocks when full or on flush

use std::io::{Cursor, Read};

use protocol::wire::{
    CompressedToken, CompressedTokenDecoder, CompressedTokenEncoder, DEFLATED_DATA, END_FLAG,
    MAX_DATA_COUNT, TOKEN_LONG, TOKEN_REL, TOKENRUN_LONG, TOKENRUN_REL,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Decodes all tokens from a zstd-encoded byte buffer.
fn zstd_decode_all(data: &[u8]) -> (Vec<u8>, Vec<u32>) {
    let mut cursor = Cursor::new(data);
    let mut decoder = CompressedTokenDecoder::new_zstd().unwrap();
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

/// Encodes tokens using zstd and returns the raw wire bytes.
fn zstd_encode(tokens: &[ZstdTestToken]) -> Vec<u8> {
    let mut encoder = CompressedTokenEncoder::new_zstd(3).unwrap();
    let mut output = Vec::new();

    for token in tokens {
        match token {
            ZstdTestToken::Literal(data) => encoder.send_literal(&mut output, data).unwrap(),
            ZstdTestToken::BlockMatch(idx) => encoder.send_block_match(&mut output, *idx).unwrap(),
        }
    }

    encoder.finish(&mut output).unwrap();
    output
}

#[derive(Clone)]
enum ZstdTestToken {
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

/// A literal-only zstd stream must produce: DEFLATED_DATA block(s) + END_FLAG.
/// The DEFLATED_DATA header uses the same 14-bit length encoding as zlib.
/// No sync marker (0x00 0x00 0xFF 0xFF) should appear in the output - zstd
/// frames are self-delimiting and don't use zlib sync flush markers.
///
/// upstream: token.c:send_zstd_token() lines 727-769
#[test]
fn golden_zstd_literal_only_wire_structure() {
    let encoded = zstd_encode(&[ZstdTestToken::Literal(
        b"Hello from zstd compressed token stream!".to_vec(),
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
    let (literals, blocks) = zstd_decode_all(&encoded);
    assert_eq!(literals, b"Hello from zstd compressed token stream!");
    assert!(blocks.is_empty());
}

/// Verify exact DEFLATED_DATA header bytes for a small zstd literal.
/// The header format is: byte0 = DEFLATED_DATA | (len >> 8), byte1 = len & 0xFF.
///
/// upstream: token.c lines 758-760
#[test]
fn golden_zstd_deflated_data_header_exact_bytes() {
    let encoded = zstd_encode(&[ZstdTestToken::Literal(b"test".to_vec())]);

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

/// Zstd compressed output must NOT contain the zlib sync marker bytes.
/// Upstream zlib mode strips trailing 0x00 0x00 0xFF 0xFF from each flush;
/// zstd does not produce these markers at all.
///
/// upstream: token.c line 685 - zstd uses ZSTD_e_flush, no marker stripping
#[test]
fn golden_zstd_no_sync_marker_in_output() {
    let data = b"Data to compress with zstd - should not contain zlib sync markers";
    let encoded = zstd_encode(&[ZstdTestToken::Literal(data.to_vec())]);

    // Search for the zlib sync marker pattern in the raw wire bytes
    let sync_marker = [0x00u8, 0x00, 0xFF, 0xFF];
    for window in encoded.windows(4) {
        assert_ne!(
            window, &sync_marker,
            "zstd output must not contain zlib sync marker 0x00 0x00 0xFF 0xFF"
        );
    }
}

// ===========================================================================
// Section 2: Block-match-only streams
// ===========================================================================

/// A single block match at index 0 produces TOKEN_REL | 0 + END_FLAG.
/// Token encoding is shared across all compression algorithms.
///
/// upstream: token.c lines 700-723 (zstd run encoding same as zlib)
#[test]
fn golden_zstd_single_block_match_token_rel() {
    let encoded = zstd_encode(&[ZstdTestToken::BlockMatch(0)]);

    // TOKEN_REL | 0 = 0x80, END_FLAG = 0x00
    assert_eq!(encoded.len(), 2, "single block match: TOKEN_REL + END_FLAG");
    assert_eq!(encoded[0], TOKEN_REL | 0);
    assert_eq!(encoded[1], END_FLAG);
}

/// Block match at index 42 (within relative range 0-63) uses TOKEN_REL.
/// r = run_start(42) - last_run_end(0) = 42, fits in 6 bits.
#[test]
fn golden_zstd_block_match_rel_42() {
    let encoded = zstd_encode(&[ZstdTestToken::BlockMatch(42)]);

    assert_eq!(encoded.len(), 2);
    assert_eq!(encoded[0], TOKEN_REL | 42);
    assert_eq!(encoded[1], END_FLAG);
}

/// Block match at index 100 (> 63) requires TOKEN_LONG absolute encoding.
/// r = 100 - 0 = 100 > 63, so TOKEN_LONG + 4-byte LE index.
///
/// upstream: token.c line 391 TOKEN_LONG path
#[test]
fn golden_zstd_block_match_token_long() {
    let encoded = zstd_encode(&[ZstdTestToken::BlockMatch(100)]);

    // TOKEN_LONG (0x20) + 4-byte LE index + END_FLAG
    assert_eq!(encoded.len(), 6);
    assert_eq!(encoded[0], TOKEN_LONG);
    assert_eq!(encoded[1..5], 100i32.to_le_bytes());
    assert_eq!(encoded[5], END_FLAG);
}

/// Non-consecutive block matches use separate TOKEN_REL encodings.
/// After block 0, last_run_end = 0. Block 5: r = 5 - 0 = 5 (fits in 6 bits).
#[test]
fn golden_zstd_non_consecutive_blocks_separate_tokens() {
    let encoded = zstd_encode(&[ZstdTestToken::BlockMatch(0), ZstdTestToken::BlockMatch(5)]);

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
/// upstream: token.c lines 700-723 (zstd uses same run detection as zlib)
#[test]
fn golden_zstd_consecutive_blocks_tokenrun_rel() {
    let encoded = zstd_encode(&[
        ZstdTestToken::BlockMatch(0),
        ZstdTestToken::BlockMatch(1),
        ZstdTestToken::BlockMatch(2),
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
fn golden_zstd_consecutive_blocks_tokenrun_long() {
    let encoded = zstd_encode(&[
        ZstdTestToken::BlockMatch(100),
        ZstdTestToken::BlockMatch(101),
    ]);

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
fn golden_zstd_four_consecutive_tokenrun_rel() {
    let encoded = zstd_encode(&[
        ZstdTestToken::BlockMatch(10),
        ZstdTestToken::BlockMatch(11),
        ZstdTestToken::BlockMatch(12),
        ZstdTestToken::BlockMatch(13),
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
fn golden_zstd_run_then_separate_block() {
    let encoded = zstd_encode(&[
        ZstdTestToken::BlockMatch(0),
        ZstdTestToken::BlockMatch(1),
        ZstdTestToken::BlockMatch(2),
        ZstdTestToken::BlockMatch(10),
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
/// The flush at the token boundary ensures decompressible output before the
/// token byte.
///
/// upstream: token.c lines 700-723 - compress_and_flush before writing token
#[test]
fn golden_zstd_mixed_literal_then_block() {
    let encoded = zstd_encode(&[
        ZstdTestToken::Literal(b"literal before block".to_vec()),
        ZstdTestToken::BlockMatch(0),
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
    let (literals, blocks) = zstd_decode_all(&encoded);
    assert_eq!(literals, b"literal before block");
    assert_eq!(blocks, vec![0]);
}

/// Block match followed by literal data: TOKEN_REL + DEFLATED_DATA + END_FLAG.
/// The block match with no preceding literals produces no DEFLATED_DATA.
/// The subsequent literal is flushed at finish().
#[test]
fn golden_zstd_mixed_block_then_literal() {
    let encoded = zstd_encode(&[
        ZstdTestToken::BlockMatch(0),
        ZstdTestToken::Literal(b"literal after block".to_vec()),
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
    let (literals, blocks) = zstd_decode_all(&encoded);
    assert_eq!(literals, b"literal after block");
    assert_eq!(blocks, vec![0]);
}

/// Interleaved pattern: lit, block, lit, block, lit, end.
/// Each literal is flushed before its following token.
///
/// upstream: token.c lines 700-723 - has_literals triggers compress_and_flush
#[test]
fn golden_zstd_interleaved_literal_block_literal() {
    let encoded = zstd_encode(&[
        ZstdTestToken::Literal(b"first".to_vec()),
        ZstdTestToken::BlockMatch(0),
        ZstdTestToken::Literal(b"second".to_vec()),
        ZstdTestToken::BlockMatch(5),
        ZstdTestToken::Literal(b"third".to_vec()),
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
    let (literals, blocks) = zstd_decode_all(&encoded);
    assert_eq!(literals, b"firstsecondthird");
    assert_eq!(blocks, vec![0, 5]);
}

/// Complex mixed stream with consecutive blocks (run encoding) interleaved
/// with literals.
#[test]
fn golden_zstd_mixed_with_run_encoding() {
    let encoded = zstd_encode(&[
        ZstdTestToken::Literal(b"prefix data".to_vec()),
        ZstdTestToken::BlockMatch(0),
        ZstdTestToken::BlockMatch(1),
        ZstdTestToken::BlockMatch(2),
        ZstdTestToken::Literal(b"suffix data".to_vec()),
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
    let (literals, blocks) = zstd_decode_all(&encoded);
    assert_eq!(literals, b"prefix datasuffix data");
    assert_eq!(blocks, vec![0, 1, 2]);
}

// ===========================================================================
// Section 5: DEFLATED_DATA framing and flush boundaries
// ===========================================================================

/// Small literals should produce exactly one DEFLATED_DATA block per flush.
/// Upstream accumulates output in a MAX_DATA_COUNT buffer and writes a single
/// DEFLATED_DATA block on flush (token.c lines 755-763).
///
/// upstream: token.c lines 740-743 - ZSTD_e_flush
#[test]
fn golden_zstd_small_literal_single_deflated_block() {
    let encoded = zstd_encode(&[
        ZstdTestToken::Literal(b"small input".to_vec()),
        ZstdTestToken::BlockMatch(0),
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
/// each at most MAX_DATA_COUNT (16383) bytes.
///
/// upstream: token.c line 755 - write when output buffer is full
#[test]
fn golden_zstd_large_literal_multiple_deflated_blocks() {
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

    let encoded = zstd_encode(&[ZstdTestToken::Literal(data.clone())]);
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
    let (literals, blocks) = zstd_decode_all(&encoded);
    assert_eq!(literals, data);
    assert!(blocks.is_empty());
}

/// Verify END_FLAG is always a single 0x00 byte at the end of the stream.
/// Shared across all compression algorithms.
///
/// upstream: token.c line 462 write_byte(f, END_FLAG)
#[test]
fn golden_zstd_end_flag_single_zero_byte() {
    // Empty stream (no literals, no blocks)
    let encoded = zstd_encode(&[]);
    assert_eq!(
        encoded.len(),
        1,
        "empty zstd stream should be just END_FLAG"
    );
    assert_eq!(encoded[0], END_FLAG);
}

/// The END_FLAG byte (0x00) is distinct from all other flag byte ranges.
/// This is critical for correct protocol parsing.
#[test]
fn golden_zstd_end_flag_not_confused_with_other_flags() {
    assert_eq!(END_FLAG, 0x00);
    assert_ne!(END_FLAG & 0xC0, DEFLATED_DATA);
    assert_ne!(END_FLAG, TOKEN_LONG);
    assert_ne!(END_FLAG, TOKENRUN_LONG);
    assert_ne!(END_FLAG & 0x80, TOKEN_REL);
    assert_ne!(END_FLAG & 0xC0, TOKENRUN_REL);
}

// ===========================================================================
// Section 6: Zstd-specific compressed payload verification
// ===========================================================================

/// Verify the zstd compressed payload starts with a valid zstd frame.
/// Zstd frames begin with the magic number 0xFD2FB528 (little-endian).
/// However, zstd streaming mode may produce raw blocks without the frame
/// header after initialization. The important thing is that the payload
/// is valid zstd data that our decoder can consume.
#[test]
fn golden_zstd_payload_is_valid_zstd_data() {
    let input = b"Verify this data produces valid zstd compressed bytes on the wire";
    let encoded = zstd_encode(&[ZstdTestToken::Literal(input.to_vec())]);

    // Extract compressed payload from first DEFLATED_DATA block
    assert_eq!(encoded[0] & 0xC0, DEFLATED_DATA);
    let high = (encoded[0] & 0x3F) as usize;
    let low = encoded[1] as usize;
    let compressed_len = (high << 8) | low;
    let payload = &encoded[2..2 + compressed_len];

    // Payload must not be empty
    assert!(
        !payload.is_empty(),
        "zstd compressed payload must not be empty"
    );

    // Verify the full stream decodes correctly (proves payload validity)
    let (literals, _) = zstd_decode_all(&encoded);
    assert_eq!(literals, input);
}

/// Zstd compressed output for the same input must differ from zlib output.
/// This verifies the encoder is actually using zstd, not accidentally
/// falling back to zlib.
#[test]
fn golden_zstd_output_differs_from_zlib() {
    use compress::zlib::CompressionLevel;

    let input = b"Compare zstd vs zlib compressed output bytes for this data";

    // Encode with zstd
    let zstd_encoded = zstd_encode(&[ZstdTestToken::Literal(input.to_vec())]);

    // Encode with zlib
    let mut zlib_encoder = CompressedTokenEncoder::new(CompressionLevel::Default, 31);
    let mut zlib_encoded = Vec::new();
    zlib_encoder.send_literal(&mut zlib_encoded, input).unwrap();
    zlib_encoder.finish(&mut zlib_encoded).unwrap();

    // Both should produce DEFLATED_DATA framing (same header structure)
    assert_eq!(zstd_encoded[0] & 0xC0, DEFLATED_DATA);
    assert_eq!(zlib_encoded[0] & 0xC0, DEFLATED_DATA);

    // But the compressed payloads must differ (different algorithms)
    assert_ne!(
        zstd_encoded, zlib_encoded,
        "zstd and zlib encodings must differ"
    );

    // Both must decode to the same input
    let (zstd_literals, _) = zstd_decode_all(&zstd_encoded);

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

    assert_eq!(zstd_literals, input.as_slice());
    assert_eq!(zlib_literals, input.as_slice());
}

// ===========================================================================
// Section 7: Decoder golden byte tests - hand-crafted wire bytes
// ===========================================================================

/// Verify the zstd decoder handles a hand-crafted wire stream with
/// only token bytes (no DEFLATED_DATA). Block-match-only streams have
/// identical wire encoding regardless of compression algorithm.
#[test]
fn golden_zstd_decoder_token_only_stream() {
    // TOKEN_REL | 5 (block 5), TOKEN_REL | 3 (block 5+3=8), END_FLAG
    let wire = [TOKEN_REL | 5, TOKEN_REL | 3, END_FLAG];

    let (literals, blocks) = zstd_decode_all(&wire);
    assert!(literals.is_empty());
    assert_eq!(blocks, vec![5, 8]);
}

/// Verify the zstd decoder handles TOKENRUN_REL in hand-crafted bytes.
/// TOKENRUN_REL | 0, run_count=4: blocks 0,1,2,3,4.
#[test]
fn golden_zstd_decoder_tokenrun_rel_hand_crafted() {
    let wire = [
        TOKENRUN_REL | 0, // rx_token = 0
        4,
        0, // n=4 (LE 16-bit) -> 4 additional tokens after first
        END_FLAG,
    ];

    let (_, blocks) = zstd_decode_all(&wire);
    assert_eq!(blocks, vec![0, 1, 2, 3, 4]);
}

/// Verify the zstd decoder handles TOKEN_LONG in hand-crafted bytes.
/// TOKEN_LONG, index=0x00001000 (4096), END_FLAG.
#[test]
fn golden_zstd_decoder_token_long_hand_crafted() {
    let wire = [
        TOKEN_LONG, 0x00, 0x10, 0x00, 0x00, // run_start=4096 (LE)
        END_FLAG,
    ];

    let (_, blocks) = zstd_decode_all(&wire);
    assert_eq!(blocks, vec![4096]);
}

/// Verify the zstd decoder handles TOKENRUN_LONG in hand-crafted bytes.
/// TOKENRUN_LONG, index=200 (LE32), n=3 (LE16): blocks 200,201,202,203.
#[test]
fn golden_zstd_decoder_tokenrun_long_hand_crafted() {
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

    let (_, blocks) = zstd_decode_all(&wire);
    assert_eq!(blocks, vec![200, 201, 202, 203]);
}

/// Verify the zstd decoder handles an empty stream (just END_FLAG).
#[test]
fn golden_zstd_decoder_empty_stream() {
    let wire = [END_FLAG];
    let (literals, blocks) = zstd_decode_all(&wire);
    assert!(literals.is_empty());
    assert!(blocks.is_empty());
}

// ===========================================================================
// Section 8: Encoder/decoder roundtrip with exact byte verification
// ===========================================================================

/// Verify that encoding then decoding a complex mixed stream preserves
/// all data and block indices exactly. This is the zstd equivalent of the
/// zlib interop roundtrip tests.
#[test]
fn golden_zstd_complex_roundtrip() {
    let tokens = vec![
        ZstdTestToken::Literal(b"header data".to_vec()),
        ZstdTestToken::BlockMatch(0),
        ZstdTestToken::BlockMatch(1),
        ZstdTestToken::BlockMatch(2),
        ZstdTestToken::Literal(b"middle data with more content".to_vec()),
        ZstdTestToken::BlockMatch(100),
        ZstdTestToken::Literal(b"trailing data".to_vec()),
    ];

    let encoded = zstd_encode(&tokens);
    let (literals, blocks) = zstd_decode_all(&encoded);

    assert_eq!(
        literals,
        b"header datamiddle data with more contenttrailing data"
    );
    assert_eq!(blocks, vec![0, 1, 2, 100]);
}

/// Verify encoder reset between files produces independent streams.
/// Each file's stream must be self-contained - no cross-file state leaks.
///
/// upstream: token.c:send_zstd_token() - ZSTD_CCtx_reset on new file
#[test]
fn golden_zstd_reset_produces_independent_streams() {
    let mut encoder = CompressedTokenEncoder::new_zstd(3).unwrap();

    for i in 0u8..3 {
        let mut output = Vec::new();
        let data = [b'A' + i; 32];
        encoder.send_literal(&mut output, &data).unwrap();
        encoder.send_block_match(&mut output, i as u32).unwrap();
        encoder.finish(&mut output).unwrap();

        // Each stream must be independently decodable
        let (literals, blocks) = zstd_decode_all(&output);
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
fn golden_zstd_large_run_count_encoding() {
    let tokens: Vec<ZstdTestToken> = (0..256u32).map(ZstdTestToken::BlockMatch).collect();

    let encoded = zstd_encode(&tokens);

    // Should use TOKENRUN_REL | 0 with n=255
    assert_eq!(encoded[0], TOKENRUN_REL | 0);
    assert_eq!(encoded[1], 255); // n & 0xFF = 255
    assert_eq!(encoded[2], 0); // n >> 8 = 0
    assert_eq!(encoded[3], END_FLAG);

    // Roundtrip
    let (_, blocks) = zstd_decode_all(&encoded);
    let expected: Vec<u32> = (0..256).collect();
    assert_eq!(blocks, expected);
}

/// Run count of 256 requires the high byte of the 16-bit LE count.
/// 257 consecutive blocks: n = 256 = 0x0100.
#[test]
fn golden_zstd_run_count_256_uses_high_byte() {
    let tokens: Vec<ZstdTestToken> = (0..257u32).map(ZstdTestToken::BlockMatch).collect();

    let encoded = zstd_encode(&tokens);

    assert_eq!(encoded[0], TOKENRUN_REL | 0);
    assert_eq!(encoded[1], 0); // n & 0xFF = 0
    assert_eq!(encoded[2], 1); // n >> 8 = 1
    assert_eq!(encoded[3], END_FLAG);

    let (_, blocks) = zstd_decode_all(&encoded);
    let expected: Vec<u32> = (0..257).collect();
    assert_eq!(blocks, expected);
}
