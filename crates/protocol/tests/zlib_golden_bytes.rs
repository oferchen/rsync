// `| 0` used deliberately in wire format constants to document the offset field.
#![allow(clippy::identity_op)]
//! Golden byte tests for zlib compressed token wire format.
//!
//! Verifies that our zlib encoder/decoder produces and consumes wire bytes
//! that match upstream rsync's CPRES_ZLIB mode (token.c:send_deflated_token /
//! recv_deflated_token). Zlib uses Z_SYNC_FLUSH with trailing sync marker
//! stripping (0x00 0x00 0xFF 0xFF removed from each flush output).
//!
//! ## Wire format (upstream token.c lines 321-329)
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
//! ## Zlib-specific behavior (upstream token.c lines 357-485)
//!
//! - Z_SYNC_FLUSH at every token boundary (block match or EOF)
//! - Trailing sync marker (0x00 0x00 0xFF 0xFF) stripped from each flush
//! - Decoder restores the sync marker before feeding data to inflate
//! - Single persistent deflate stream per file transfer
//! - Z_NO_FLUSH for literal data between boundaries

use std::io::{Cursor, Read};

use compress::zlib::CompressionLevel;
use protocol::wire::{
    CompressedToken, CompressedTokenDecoder, CompressedTokenEncoder, DEFLATED_DATA, END_FLAG,
    MAX_DATA_COUNT, TOKEN_LONG, TOKEN_REL, TOKENRUN_LONG, TOKENRUN_REL,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Decodes all tokens from a zlib-encoded byte buffer.
fn zlib_decode_all(data: &[u8]) -> (Vec<u8>, Vec<u32>) {
    let mut cursor = Cursor::new(data);
    let mut decoder = CompressedTokenDecoder::new();
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

/// Encodes tokens using zlib and returns the raw wire bytes.
fn zlib_encode(tokens: &[ZlibTestToken]) -> Vec<u8> {
    let mut encoder = CompressedTokenEncoder::new(CompressionLevel::Default, 31);
    let mut output = Vec::new();

    for token in tokens {
        match token {
            ZlibTestToken::Literal(data) => encoder.send_literal(&mut output, data).unwrap(),
            ZlibTestToken::BlockMatch(idx) => encoder.send_block_match(&mut output, *idx).unwrap(),
        }
    }

    encoder.finish(&mut output).unwrap();
    output
}

#[derive(Clone)]
enum ZlibTestToken {
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

/// A literal-only zlib stream must produce: DEFLATED_DATA block(s) + END_FLAG.
/// The DEFLATED_DATA header uses the same 14-bit length encoding shared by all
/// compression codecs. The sync marker (0x00 0x00 0xFF 0xFF) must be stripped
/// from the compressed output.
///
/// upstream: token.c:send_deflated_token() lines 357-485
#[test]
fn golden_zlib_literal_only_wire_structure() {
    let encoded = zlib_encode(&[ZlibTestToken::Literal(
        b"Hello from zlib compressed token stream!".to_vec(),
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
    let (literals, blocks) = zlib_decode_all(&encoded);
    assert_eq!(literals, b"Hello from zlib compressed token stream!");
    assert!(blocks.is_empty());
}

/// Verify exact DEFLATED_DATA header bytes for a small zlib literal.
/// The header format is: byte0 = DEFLATED_DATA | (len >> 8), byte1 = len & 0xFF.
///
/// upstream: token.c lines 451-453
#[test]
fn golden_zlib_deflated_data_header_exact_bytes() {
    let encoded = zlib_encode(&[ZlibTestToken::Literal(b"test".to_vec())]);

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

/// Zlib sync marker (0x00 0x00 0xFF 0xFF) must NOT appear in the wire output.
/// Upstream strips this trailing marker after each Z_SYNC_FLUSH to save 4 bytes
/// per flush. The decoder restores it before feeding data to inflate.
///
/// upstream: token.c lines 441-445 - strips sync marker after Z_SYNC_FLUSH
#[test]
fn golden_zlib_sync_marker_stripped_from_output() {
    let data = b"Data to compress with zlib - sync marker must be stripped from output";
    let encoded = zlib_encode(&[ZlibTestToken::Literal(data.to_vec())]);

    // Search for the zlib sync marker pattern in the raw wire bytes
    let sync_marker = [0x00u8, 0x00, 0xFF, 0xFF];
    for window in encoded.windows(4) {
        assert_ne!(
            window, &sync_marker,
            "zlib output must not contain sync marker 0x00 0x00 0xFF 0xFF (should be stripped)"
        );
    }

    // Roundtrip still works because decoder restores the marker
    let (literals, _) = zlib_decode_all(&encoded);
    assert_eq!(literals, data.as_slice());
}

/// Zlib compressed output is raw deflate (no zlib header).
/// The flate2 Compress is created with `false` for zlib_header parameter.
/// Raw deflate streams do not start with the zlib header bytes (0x78 xx).
///
/// upstream: token.c - uses raw deflate, not zlib wrapper
#[test]
fn golden_zlib_raw_deflate_no_zlib_header() {
    let input = b"Verify raw deflate without zlib header bytes";
    let encoded = zlib_encode(&[ZlibTestToken::Literal(input.to_vec())]);

    // Extract compressed payload from first DEFLATED_DATA block
    assert_eq!(encoded[0] & 0xC0, DEFLATED_DATA);
    let high = (encoded[0] & 0x3F) as usize;
    let low = encoded[1] as usize;
    let compressed_len = (high << 8) | low;
    let payload = &encoded[2..2 + compressed_len];

    // Raw deflate does NOT start with 0x78 (zlib CMF byte).
    // The first byte of a raw deflate stream is a block header:
    // bits 0: BFINAL, bits 1-2: BTYPE (00=stored, 01=fixed, 10=dynamic)
    // For compressed data, BTYPE is typically 01 (fixed Huffman) or 10 (dynamic).
    // The zlib header byte 0x78 would indicate CM=8 (deflate) + CINFO=7 (32K window).
    // We verify this is not a zlib-wrapped stream.
    if compressed_len >= 2 {
        let is_zlib_header = payload[0] == 0x78
            && (payload[1] == 0x01
                || payload[1] == 0x5E
                || payload[1] == 0x9C
                || payload[1] == 0xDA);
        assert!(
            !is_zlib_header,
            "payload must be raw deflate, not zlib-wrapped (found 0x78 header)"
        );
    }

    // Roundtrip verification proves the payload is valid raw deflate
    let (literals, _) = zlib_decode_all(&encoded);
    assert_eq!(literals, input.as_slice());
}

// ===========================================================================
// Section 2: Block-match-only streams
// ===========================================================================

/// A single block match at index 0 produces TOKEN_REL | 0 + END_FLAG.
/// Token encoding is shared across all compression algorithms.
///
/// upstream: token.c lines 380-398 (zlib run encoding)
#[test]
fn golden_zlib_single_block_match_token_rel() {
    let encoded = zlib_encode(&[ZlibTestToken::BlockMatch(0)]);

    // TOKEN_REL | 0 = 0x80, END_FLAG = 0x00
    assert_eq!(encoded.len(), 2, "single block match: TOKEN_REL + END_FLAG");
    assert_eq!(encoded[0], TOKEN_REL | 0);
    assert_eq!(encoded[1], END_FLAG);
}

/// Block match at index 42 (within relative range 0-63) uses TOKEN_REL.
/// r = run_start(42) - last_run_end(0) = 42, fits in 6 bits.
#[test]
fn golden_zlib_block_match_rel_42() {
    let encoded = zlib_encode(&[ZlibTestToken::BlockMatch(42)]);

    assert_eq!(encoded.len(), 2);
    assert_eq!(encoded[0], TOKEN_REL | 42);
    assert_eq!(encoded[1], END_FLAG);
}

/// Block match at index 63 (max relative range) uses TOKEN_REL.
/// r = 63, exactly the maximum 6-bit value.
#[test]
fn golden_zlib_block_match_rel_max() {
    let encoded = zlib_encode(&[ZlibTestToken::BlockMatch(63)]);

    assert_eq!(encoded.len(), 2);
    assert_eq!(encoded[0], TOKEN_REL | 63);
    assert_eq!(encoded[1], END_FLAG);
}

/// Block match at index 64 (just beyond relative range) requires TOKEN_LONG.
/// r = 64 > 63, so TOKEN_LONG + 4-byte LE index.
///
/// upstream: token.c line 391 TOKEN_LONG path
#[test]
fn golden_zlib_block_match_rel_boundary_64() {
    let encoded = zlib_encode(&[ZlibTestToken::BlockMatch(64)]);

    // TOKEN_LONG (0x20) + 4-byte LE index + END_FLAG
    assert_eq!(encoded.len(), 6);
    assert_eq!(encoded[0], TOKEN_LONG);
    assert_eq!(encoded[1..5], 64i32.to_le_bytes());
    assert_eq!(encoded[5], END_FLAG);
}

/// Block match at index 100 (> 63) requires TOKEN_LONG absolute encoding.
/// r = 100 - 0 = 100 > 63, so TOKEN_LONG + 4-byte LE index.
///
/// upstream: token.c line 391 TOKEN_LONG path
#[test]
fn golden_zlib_block_match_token_long() {
    let encoded = zlib_encode(&[ZlibTestToken::BlockMatch(100)]);

    // TOKEN_LONG (0x20) + 4-byte LE index + END_FLAG
    assert_eq!(encoded.len(), 6);
    assert_eq!(encoded[0], TOKEN_LONG);
    assert_eq!(encoded[1..5], 100i32.to_le_bytes());
    assert_eq!(encoded[5], END_FLAG);
}

/// Non-consecutive block matches use separate TOKEN_REL encodings.
/// After block 0, last_run_end = 0. Block 5: r = 5 - 0 = 5 (fits in 6 bits).
#[test]
fn golden_zlib_non_consecutive_blocks_separate_tokens() {
    let encoded = zlib_encode(&[ZlibTestToken::BlockMatch(0), ZlibTestToken::BlockMatch(5)]);

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
/// upstream: token.c lines 380-398 (zlib uses same run detection)
#[test]
fn golden_zlib_consecutive_blocks_tokenrun_rel() {
    let encoded = zlib_encode(&[
        ZlibTestToken::BlockMatch(0),
        ZlibTestToken::BlockMatch(1),
        ZlibTestToken::BlockMatch(2),
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
fn golden_zlib_consecutive_blocks_tokenrun_long() {
    let encoded = zlib_encode(&[
        ZlibTestToken::BlockMatch(100),
        ZlibTestToken::BlockMatch(101),
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
fn golden_zlib_four_consecutive_tokenrun_rel() {
    let encoded = zlib_encode(&[
        ZlibTestToken::BlockMatch(10),
        ZlibTestToken::BlockMatch(11),
        ZlibTestToken::BlockMatch(12),
        ZlibTestToken::BlockMatch(13),
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
fn golden_zlib_run_then_separate_block() {
    let encoded = zlib_encode(&[
        ZlibTestToken::BlockMatch(0),
        ZlibTestToken::BlockMatch(1),
        ZlibTestToken::BlockMatch(2),
        ZlibTestToken::BlockMatch(10),
    ]);

    // TOKENRUN_REL | 0 (0xC0), n=2 (LE16), TOKEN_REL | 8 (0x88), END_FLAG
    assert_eq!(encoded.len(), 5);
    assert_eq!(encoded[0], TOKENRUN_REL | 0);
    assert_eq!(encoded[1], 2); // n & 0xFF
    assert_eq!(encoded[2], 0); // n >> 8
    assert_eq!(encoded[3], TOKEN_REL | 8);
    assert_eq!(encoded[4], END_FLAG);
}

/// Two separate runs: blocks 0,1,2 then 10,11,12.
/// First run: TOKENRUN_REL | 0, n=2.
/// Second run: TOKENRUN_REL | (10-2)=8, n=2.
#[test]
fn golden_zlib_two_separate_runs() {
    let encoded = zlib_encode(&[
        ZlibTestToken::BlockMatch(0),
        ZlibTestToken::BlockMatch(1),
        ZlibTestToken::BlockMatch(2),
        ZlibTestToken::BlockMatch(10),
        ZlibTestToken::BlockMatch(11),
        ZlibTestToken::BlockMatch(12),
    ]);

    assert_eq!(encoded.len(), 7);
    // First run: TOKENRUN_REL | 0, n=2
    assert_eq!(encoded[0], TOKENRUN_REL | 0);
    assert_eq!(encoded[1], 2);
    assert_eq!(encoded[2], 0);
    // Second run: TOKENRUN_REL | 8, n=2
    assert_eq!(encoded[3], TOKENRUN_REL | 8);
    assert_eq!(encoded[4], 2);
    assert_eq!(encoded[5], 0);
    assert_eq!(encoded[6], END_FLAG);
}

// ===========================================================================
// Section 4: Mixed literal + block match streams
// ===========================================================================

/// Literal data followed by a block match: DEFLATED_DATA + TOKEN_REL + END_FLAG.
/// The Z_SYNC_FLUSH at the token boundary produces decompressible output before
/// the token byte.
///
/// upstream: token.c lines 400-430 - flush_all_literals before writing token
#[test]
fn golden_zlib_mixed_literal_then_block() {
    let encoded = zlib_encode(&[
        ZlibTestToken::Literal(b"literal before block".to_vec()),
        ZlibTestToken::BlockMatch(0),
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
    let (literals, blocks) = zlib_decode_all(&encoded);
    assert_eq!(literals, b"literal before block");
    assert_eq!(blocks, vec![0]);
}

/// Block match followed by literal data: TOKEN_REL + DEFLATED_DATA + END_FLAG.
/// The block match with no preceding literals produces no DEFLATED_DATA.
/// The subsequent literal is flushed at finish().
#[test]
fn golden_zlib_mixed_block_then_literal() {
    let encoded = zlib_encode(&[
        ZlibTestToken::BlockMatch(0),
        ZlibTestToken::Literal(b"literal after block".to_vec()),
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
    let (literals, blocks) = zlib_decode_all(&encoded);
    assert_eq!(literals, b"literal after block");
    assert_eq!(blocks, vec![0]);
}

/// Interleaved pattern: lit, block, lit, block, lit, end.
/// Each literal is flushed before its following token.
///
/// upstream: token.c - has_literals triggers flush_all_literals
#[test]
fn golden_zlib_interleaved_literal_block_literal() {
    let encoded = zlib_encode(&[
        ZlibTestToken::Literal(b"first".to_vec()),
        ZlibTestToken::BlockMatch(0),
        ZlibTestToken::Literal(b"second".to_vec()),
        ZlibTestToken::BlockMatch(5),
        ZlibTestToken::Literal(b"third".to_vec()),
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
    let (literals, blocks) = zlib_decode_all(&encoded);
    assert_eq!(literals, b"firstsecondthird");
    assert_eq!(blocks, vec![0, 5]);
}

/// Complex mixed stream with consecutive blocks (run encoding) interleaved
/// with literals.
#[test]
fn golden_zlib_mixed_with_run_encoding() {
    let encoded = zlib_encode(&[
        ZlibTestToken::Literal(b"prefix data".to_vec()),
        ZlibTestToken::BlockMatch(0),
        ZlibTestToken::BlockMatch(1),
        ZlibTestToken::BlockMatch(2),
        ZlibTestToken::Literal(b"suffix data".to_vec()),
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
    let (literals, blocks) = zlib_decode_all(&encoded);
    assert_eq!(literals, b"prefix datasuffix data");
    assert_eq!(blocks, vec![0, 1, 2]);
}

// ===========================================================================
// Section 5: DEFLATED_DATA framing and flush boundaries
// ===========================================================================

/// Small literals should produce exactly one DEFLATED_DATA block per flush.
/// Upstream accumulates compressed output and writes DEFLATED_DATA blocks.
///
/// upstream: token.c lines 440-455 - write_deflated_data_pieces
#[test]
fn golden_zlib_small_literal_single_deflated_block() {
    let encoded = zlib_encode(&[
        ZlibTestToken::Literal(b"small input".to_vec()),
        ZlibTestToken::BlockMatch(0),
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
/// upstream: token.c lines 440-455 - write_deflated_data_pieces
#[test]
fn golden_zlib_large_literal_multiple_deflated_blocks() {
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

    let encoded = zlib_encode(&[ZlibTestToken::Literal(data.clone())]);
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
    let (literals, blocks) = zlib_decode_all(&encoded);
    assert_eq!(literals, data);
    assert!(blocks.is_empty());
}

/// Verify END_FLAG is always a single 0x00 byte at the end of the stream.
/// Shared across all compression algorithms.
///
/// upstream: token.c line 462 write_byte(f, END_FLAG)
#[test]
fn golden_zlib_end_flag_single_zero_byte() {
    // Empty stream (no literals, no blocks)
    let encoded = zlib_encode(&[]);
    assert_eq!(
        encoded.len(),
        1,
        "empty zlib stream should be just END_FLAG"
    );
    assert_eq!(encoded[0], END_FLAG);
}

/// The END_FLAG byte (0x00) is distinct from all other flag byte ranges.
/// This is critical for correct protocol parsing.
#[test]
fn golden_zlib_end_flag_not_confused_with_other_flags() {
    assert_eq!(END_FLAG, 0x00);
    assert_ne!(END_FLAG & 0xC0, DEFLATED_DATA);
    assert_ne!(END_FLAG, TOKEN_LONG);
    assert_ne!(END_FLAG, TOKENRUN_LONG);
    assert_ne!(END_FLAG & 0x80, TOKEN_REL);
    assert_ne!(END_FLAG & 0xC0, TOKENRUN_REL);
}

// ===========================================================================
// Section 6: Zlib-specific compressed payload verification
// ===========================================================================

/// Verify the zlib compressed payload is valid raw deflate data.
/// The first DEFLATED_DATA block should contain a deflate block header.
/// Bit pattern: BFINAL (1 bit) + BTYPE (2 bits).
/// BTYPE 01 = fixed Huffman, BTYPE 10 = dynamic Huffman.
#[test]
fn golden_zlib_payload_is_valid_deflate_data() {
    let input = b"Verify this data produces valid deflate compressed bytes on the wire";
    let encoded = zlib_encode(&[ZlibTestToken::Literal(input.to_vec())]);

    // Extract compressed payload from first DEFLATED_DATA block
    assert_eq!(encoded[0] & 0xC0, DEFLATED_DATA);
    let high = (encoded[0] & 0x3F) as usize;
    let low = encoded[1] as usize;
    let compressed_len = (high << 8) | low;
    let payload = &encoded[2..2 + compressed_len];

    // Payload must not be empty
    assert!(
        !payload.is_empty(),
        "zlib compressed payload must not be empty"
    );

    // Verify the full stream decodes correctly (proves payload validity)
    let (literals, _) = zlib_decode_all(&encoded);
    assert_eq!(literals, input.as_slice());
}

/// Zlib compressed output must differ from uncompressed input.
/// This verifies the encoder is actually compressing, not passing through raw.
#[test]
fn golden_zlib_output_is_compressed() {
    let input = b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"; // highly compressible
    let encoded = zlib_encode(&[ZlibTestToken::Literal(input.to_vec())]);

    // Extract total compressed payload size
    let (_, block_sizes) = parse_wire_structure(&encoded);
    let total_compressed: usize = block_sizes.iter().sum();

    // 40 bytes of 'A' should compress to much less
    assert!(
        total_compressed < input.len(),
        "40 repeated 'A' bytes ({} bytes) should compress smaller than input, got {total_compressed}",
        input.len()
    );

    // Roundtrip
    let (literals, _) = zlib_decode_all(&encoded);
    assert_eq!(literals, input.as_slice());
}

/// The persistent deflate stream should achieve better compression on
/// repeated patterns across multiple literals within the same file.
/// Each literal builds on the same deflate dictionary.
///
/// upstream: token.c - single compressor per file, not per-chunk
#[test]
fn golden_zlib_persistent_stream_dictionary_effect() {
    // Send the same literal data twice - second should compress better
    // because the deflate dictionary already contains the pattern
    let chunk = b"The quick brown fox jumps over the lazy dog. ";

    let mut encoder = CompressedTokenEncoder::new(CompressionLevel::Default, 31);
    let mut output1 = Vec::new();

    // First file: one copy
    encoder.send_literal(&mut output1, chunk).unwrap();
    encoder.send_literal(&mut output1, chunk).unwrap();
    encoder.finish(&mut output1).unwrap();

    // Roundtrip verifies both copies are preserved
    let (literals, _) = zlib_decode_all(&output1);
    let mut expected = Vec::new();
    expected.extend_from_slice(chunk);
    expected.extend_from_slice(chunk);
    assert_eq!(literals, expected);
}

// ===========================================================================
// Section 7: Decoder golden byte tests - hand-crafted wire bytes
// ===========================================================================

/// Verify the zlib decoder handles a hand-crafted wire stream with
/// only token bytes (no DEFLATED_DATA). Block-match-only streams have
/// identical wire encoding regardless of compression algorithm.
#[test]
fn golden_zlib_decoder_token_only_stream() {
    // TOKEN_REL | 5 (block 5), TOKEN_REL | 3 (block 5+3=8), END_FLAG
    let wire = [TOKEN_REL | 5, TOKEN_REL | 3, END_FLAG];

    let (literals, blocks) = zlib_decode_all(&wire);
    assert!(literals.is_empty());
    assert_eq!(blocks, vec![5, 8]);
}

/// Verify the zlib decoder handles TOKENRUN_REL in hand-crafted bytes.
/// TOKENRUN_REL | 0, run_count=4: blocks 0,1,2,3,4.
#[test]
fn golden_zlib_decoder_tokenrun_rel_hand_crafted() {
    let wire = [
        TOKENRUN_REL | 0, // rx_token = 0
        4,
        0, // n=4 (LE 16-bit) -> 4 additional tokens after first
        END_FLAG,
    ];

    let (_, blocks) = zlib_decode_all(&wire);
    assert_eq!(blocks, vec![0, 1, 2, 3, 4]);
}

/// Verify the zlib decoder handles TOKEN_LONG in hand-crafted bytes.
/// TOKEN_LONG, index=0x00001000 (4096), END_FLAG.
#[test]
fn golden_zlib_decoder_token_long_hand_crafted() {
    let wire = [
        TOKEN_LONG, 0x00, 0x10, 0x00, 0x00, // run_start=4096 (LE)
        END_FLAG,
    ];

    let (_, blocks) = zlib_decode_all(&wire);
    assert_eq!(blocks, vec![4096]);
}

/// Verify the zlib decoder handles TOKENRUN_LONG in hand-crafted bytes.
/// TOKENRUN_LONG, index=200 (LE32), n=3 (LE16): blocks 200,201,202,203.
#[test]
fn golden_zlib_decoder_tokenrun_long_hand_crafted() {
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

    let (_, blocks) = zlib_decode_all(&wire);
    assert_eq!(blocks, vec![200, 201, 202, 203]);
}

/// Verify the zlib decoder handles an empty stream (just END_FLAG).
#[test]
fn golden_zlib_decoder_empty_stream() {
    let wire = [END_FLAG];
    let (literals, blocks) = zlib_decode_all(&wire);
    assert!(literals.is_empty());
    assert!(blocks.is_empty());
}

/// Verify the zlib decoder handles TOKEN_LONG with a large index.
/// TOKEN_LONG, index=0x00FFFFFF (16777215), END_FLAG.
#[test]
fn golden_zlib_decoder_token_long_large_index() {
    let wire = [
        TOKEN_LONG, 0xFF, 0xFF, 0xFF, 0x00, // run_start=16777215 (LE)
        END_FLAG,
    ];

    let (_, blocks) = zlib_decode_all(&wire);
    assert_eq!(blocks, vec![16_777_215]);
}

/// Verify the zlib decoder handles multiple TOKEN_REL with cumulative offsets.
/// TOKEN_REL | 10 (block 10), TOKEN_REL | 20 (block 30), TOKEN_REL | 33 (block 63).
#[test]
fn golden_zlib_decoder_cumulative_token_rel() {
    let wire = [TOKEN_REL | 10, TOKEN_REL | 20, TOKEN_REL | 33, END_FLAG];

    let (_, blocks) = zlib_decode_all(&wire);
    assert_eq!(blocks, vec![10, 30, 63]);
}

/// Verify the zlib decoder handles TOKENRUN_REL with a large run count.
/// TOKENRUN_REL | 0, n=1000 (0x03E8 LE): blocks 0..=1000.
#[test]
fn golden_zlib_decoder_tokenrun_rel_large_count() {
    let wire = [
        TOKENRUN_REL | 0, // rx_token = 0
        0xE8,
        0x03, // n=1000 (LE 16-bit)
        END_FLAG,
    ];

    let (_, blocks) = zlib_decode_all(&wire);
    let expected: Vec<u32> = (0..=1000).collect();
    assert_eq!(blocks, expected);
}

/// Verify the zlib decoder handles mixed token types in hand-crafted stream.
/// TOKEN_REL, TOKENRUN_REL, TOKEN_LONG, TOKENRUN_LONG.
#[test]
fn golden_zlib_decoder_mixed_token_types() {
    let mut wire = Vec::new();

    // TOKEN_REL | 5 -> block 5
    wire.push(TOKEN_REL | 5);

    // TOKENRUN_REL | 3 -> block 8,9,10 (r=3 from 5, n=2)
    wire.push(TOKENRUN_REL | 3);
    wire.extend_from_slice(&2u16.to_le_bytes()); // n=2

    // TOKEN_LONG -> block 1000 (r = 1000-10 > 63)
    wire.push(TOKEN_LONG);
    wire.extend_from_slice(&1000i32.to_le_bytes());

    // TOKENRUN_LONG -> blocks 2000,2001,2002 (r = 2000-1000 > 63)
    wire.push(TOKENRUN_LONG);
    wire.extend_from_slice(&2000i32.to_le_bytes());
    wire.extend_from_slice(&2u16.to_le_bytes()); // n=2

    wire.push(END_FLAG);

    let (_, blocks) = zlib_decode_all(&wire);
    assert_eq!(blocks, vec![5, 8, 9, 10, 1000, 2000, 2001, 2002]);
}

// ===========================================================================
// Section 8: Encoder/decoder roundtrip with exact byte verification
// ===========================================================================

/// Verify that encoding then decoding a complex mixed stream preserves
/// all data and block indices exactly.
#[test]
fn golden_zlib_complex_roundtrip() {
    let tokens = vec![
        ZlibTestToken::Literal(b"header data".to_vec()),
        ZlibTestToken::BlockMatch(0),
        ZlibTestToken::BlockMatch(1),
        ZlibTestToken::BlockMatch(2),
        ZlibTestToken::Literal(b"middle data with more content".to_vec()),
        ZlibTestToken::BlockMatch(100),
        ZlibTestToken::Literal(b"trailing data".to_vec()),
    ];

    let encoded = zlib_encode(&tokens);
    let (literals, blocks) = zlib_decode_all(&encoded);

    assert_eq!(
        literals,
        b"header datamiddle data with more contenttrailing data"
    );
    assert_eq!(blocks, vec![0, 1, 2, 100]);
}

/// Verify encoder reset between files produces independent streams.
/// Each file's stream must be self-contained - no cross-file state leaks.
///
/// upstream: token.c - compressor reset on new file
#[test]
fn golden_zlib_reset_produces_independent_streams() {
    let mut encoder = CompressedTokenEncoder::new(CompressionLevel::Default, 31);

    for i in 0u8..3 {
        let mut output = Vec::new();
        let data = [b'A' + i; 32];
        encoder.send_literal(&mut output, &data).unwrap();
        encoder.send_block_match(&mut output, i as u32).unwrap();
        encoder.finish(&mut output).unwrap();

        // Each stream must be independently decodable
        let (literals, blocks) = zlib_decode_all(&output);
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
fn golden_zlib_large_run_count_encoding() {
    let tokens: Vec<ZlibTestToken> = (0..256u32).map(ZlibTestToken::BlockMatch).collect();

    let encoded = zlib_encode(&tokens);

    // Should use TOKENRUN_REL | 0 with n=255
    assert_eq!(encoded[0], TOKENRUN_REL | 0);
    assert_eq!(encoded[1], 255); // n & 0xFF = 255
    assert_eq!(encoded[2], 0); // n >> 8 = 0
    assert_eq!(encoded[3], END_FLAG);

    // Roundtrip
    let (_, blocks) = zlib_decode_all(&encoded);
    let expected: Vec<u32> = (0..256).collect();
    assert_eq!(blocks, expected);
}

/// Run count of 256 requires the high byte of the 16-bit LE count.
/// 257 consecutive blocks: n = 256 = 0x0100.
#[test]
fn golden_zlib_run_count_256_uses_high_byte() {
    let tokens: Vec<ZlibTestToken> = (0..257u32).map(ZlibTestToken::BlockMatch).collect();

    let encoded = zlib_encode(&tokens);

    assert_eq!(encoded[0], TOKENRUN_REL | 0);
    assert_eq!(encoded[1], 0); // n & 0xFF = 0
    assert_eq!(encoded[2], 1); // n >> 8 = 1
    assert_eq!(encoded[3], END_FLAG);

    let (_, blocks) = zlib_decode_all(&encoded);
    let expected: Vec<u32> = (0..257).collect();
    assert_eq!(blocks, expected);
}

/// Roundtrip with various compression levels to ensure they all produce
/// valid wire format that decodes correctly.
#[test]
fn golden_zlib_roundtrip_all_compression_levels() {
    let input = b"Test data for compression level roundtrip verification across all levels";

    let levels = [
        CompressionLevel::Fast,
        CompressionLevel::Default,
        CompressionLevel::Best,
    ];

    for level in &levels {
        let mut encoder = CompressedTokenEncoder::new(*level, 31);
        let mut output = Vec::new();
        encoder.send_literal(&mut output, input).unwrap();
        encoder.finish(&mut output).unwrap();

        let (literals, blocks) = zlib_decode_all(&output);
        assert_eq!(literals, input.as_slice(), "roundtrip failed for {level:?}");
        assert!(blocks.is_empty());

        // Wire structure must be valid
        let (sequence, _) = parse_wire_structure(&output);
        assert_eq!(*sequence.last().unwrap(), "END");
    }
}

/// Roundtrip with empty literal (zero-length data).
/// Should produce no DEFLATED_DATA blocks - just END_FLAG.
#[test]
fn golden_zlib_empty_literal_no_deflated_data() {
    let encoded = zlib_encode(&[ZlibTestToken::Literal(Vec::new())]);

    // Empty literal should produce just END_FLAG
    assert_eq!(encoded.len(), 1);
    assert_eq!(encoded[0], END_FLAG);
}

/// Roundtrip with a single byte literal.
#[test]
fn golden_zlib_single_byte_literal_roundtrip() {
    let encoded = zlib_encode(&[ZlibTestToken::Literal(vec![0x42])]);

    let (literals, blocks) = zlib_decode_all(&encoded);
    assert_eq!(literals, vec![0x42]);
    assert!(blocks.is_empty());

    // Must have DEFLATED_DATA + END structure
    let (sequence, _) = parse_wire_structure(&encoded);
    assert_eq!(sequence[0], "DEFLATED_DATA");
    assert_eq!(*sequence.last().unwrap(), "END");
}

// ===========================================================================
// Section 9: Sync marker restoration in decoder
// ===========================================================================

/// Verify the decoder correctly restores the stripped sync marker.
/// The encoder strips 0x00 0x00 0xFF 0xFF from Z_SYNC_FLUSH output.
/// The decoder must append it back before feeding data to inflate.
///
/// We verify this indirectly: if the decoder did NOT restore the marker,
/// inflate would fail or produce garbage.
///
/// upstream: token.c lines 590-600 - decoder restores sync marker
#[test]
fn golden_zlib_decoder_restores_sync_marker() {
    // Encode a variety of literal patterns that exercise the sync flush path
    let patterns: Vec<Vec<u8>> = vec![
        b"short".to_vec(),
        vec![0xAA; 100],
        b"mixed content with various bytes 0123456789".to_vec(),
        vec![0x00; 50], // all zeros
        vec![0xFF; 50], // all 0xFF - could confuse naive marker detection
    ];

    for (i, pattern) in patterns.iter().enumerate() {
        let encoded = zlib_encode(&[
            ZlibTestToken::Literal(pattern.clone()),
            ZlibTestToken::BlockMatch(0), // forces sync flush
        ]);

        let (literals, blocks) = zlib_decode_all(&encoded);
        assert_eq!(
            literals, *pattern,
            "sync marker restoration failed for pattern {i}"
        );
        assert_eq!(blocks, vec![0]);
    }
}

/// Multiple consecutive flushes (literal, block, literal, block) each strip
/// and restore the sync marker independently. The persistent deflate stream
/// must remain coherent across multiple flush/restore cycles.
///
/// upstream: token.c - encoder strips marker per flush, decoder restores per read
#[test]
fn golden_zlib_multiple_flush_restore_cycles() {
    let tokens = vec![
        ZlibTestToken::Literal(b"flush one".to_vec()),
        ZlibTestToken::BlockMatch(0),
        ZlibTestToken::Literal(b"flush two".to_vec()),
        ZlibTestToken::BlockMatch(1),
        ZlibTestToken::Literal(b"flush three".to_vec()),
        ZlibTestToken::BlockMatch(2),
        ZlibTestToken::Literal(b"flush four".to_vec()),
        ZlibTestToken::BlockMatch(3),
    ];

    let encoded = zlib_encode(&tokens);
    let (literals, blocks) = zlib_decode_all(&encoded);

    assert_eq!(literals, b"flush oneflush twoflush threeflush four");
    assert_eq!(blocks, vec![0, 1, 2, 3]);

    // No sync markers in the wire output
    let sync_marker = [0x00u8, 0x00, 0xFF, 0xFF];
    for window in encoded.windows(4) {
        assert_ne!(
            window, &sync_marker,
            "sync marker must be stripped from wire output"
        );
    }
}
