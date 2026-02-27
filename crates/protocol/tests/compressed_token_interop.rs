// `| 0` used deliberately in wire format constants to document the offset field.
#![allow(clippy::identity_op)]
//! Compressed token interop tests against upstream rsync 3.4.1.
//!
//! These tests verify wire compatibility with upstream rsync's compressed token
//! format defined in `token.c`. The compressed token stream uses DEFLATED_DATA
//! headers for literal data, relative/absolute block-match tokens, and run-length
//! encoding for consecutive blocks.
//!
//! ## Wire Format Reference (upstream token.c lines 321-329)
//!
//! ```text
//! END_FLAG      = 0x00  — end of file marker
//! TOKEN_LONG    = 0x20  — followed by 32-bit LE token number
//! TOKENRUN_LONG = 0x21  — followed by 32-bit LE token + 16-bit LE run count
//! DEFLATED_DATA = 0x40  — + 6-bit high len, then low len byte, then compressed data
//! TOKEN_REL     = 0x80  — + 6-bit relative token number
//! TOKENRUN_REL  = 0xC0  — + 6-bit relative token + 16-bit LE run count
//! ```
//!
//! ## Testing Strategy
//!
//! 1. **Golden byte tests**: Construct raw byte sequences matching upstream wire
//!    format and verify our decoder parses them identically.
//! 2. **Encoder format tests**: Verify our encoder produces upstream-compatible
//!    byte patterns for each token type.
//! 3. **Round-trip tests**: Encode with our encoder, decode with our decoder,
//!    verify data integrity across diverse data patterns.
//! 4. **Edge case tests**: Empty files, single-block files, maximum run lengths,
//!    boundary conditions on DEFLATED_DATA headers.

use std::io::Cursor;

use compress::zlib::CompressionLevel;
use protocol::wire::{
    CompressedToken, CompressedTokenDecoder, CompressedTokenEncoder, DEFLATED_DATA, END_FLAG,
    MAX_DATA_COUNT, TOKEN_LONG, TOKEN_REL, TOKENRUN_LONG, TOKENRUN_REL,
};

// ---------------------------------------------------------------------------
// Helper: collect all tokens from a decoder until End
// ---------------------------------------------------------------------------

/// Decodes all tokens from a byte buffer, returning (literals, block_matches).
fn decode_all(data: &[u8]) -> (Vec<u8>, Vec<u32>) {
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

/// Encodes literals and block matches, then returns the raw byte stream.
fn encode_stream(tokens: &[TestToken]) -> Vec<u8> {
    let mut encoder = CompressedTokenEncoder::new(CompressionLevel::Default, 31);
    let mut output = Vec::new();

    for token in tokens {
        match token {
            TestToken::Literal(data) => encoder.send_literal(&mut output, data).unwrap(),
            TestToken::BlockMatch(idx) => encoder.send_block_match(&mut output, *idx).unwrap(),
        }
    }

    encoder.finish(&mut output).unwrap();
    output
}

/// Test token representation for building test streams.
#[derive(Clone)]
enum TestToken {
    Literal(Vec<u8>),
    BlockMatch(u32),
}

// ===========================================================================
// Section 1: Golden byte tests — verify decoder handles upstream wire format
// ===========================================================================

/// Upstream rsync signals end-of-file with a single END_FLAG (0x00) byte.
/// Reference: token.c line 462 `write_byte(f, END_FLAG)`.
#[test]
fn golden_end_flag_is_single_zero_byte() {
    let data = [END_FLAG];
    let mut cursor = Cursor::new(&data[..]);
    let mut decoder = CompressedTokenDecoder::new();

    let token = decoder.recv_token(&mut cursor).unwrap();
    assert_eq!(token, CompressedToken::End);
}

/// TOKEN_LONG (0x20) encodes an absolute block index as a 4-byte LE integer.
/// Reference: token.c line 391 `write_byte(f, TOKEN_LONG); write_int(f, run_start)`.
///
/// For block index 42: [0x20, 42, 0, 0, 0, END_FLAG]
#[test]
fn golden_token_long_block_42() {
    let data = [
        TOKEN_LONG, 42, 0, 0, 0, // run_start=42 (LE)
        END_FLAG,
    ];
    let (_, blocks) = decode_all(&data);
    assert_eq!(blocks, vec![42]);
}

/// TOKEN_LONG with a large block index (0x00010203).
/// Verifies correct little-endian byte order interpretation.
#[test]
fn golden_token_long_large_index() {
    let data = [
        TOKEN_LONG, 0x03, 0x02, 0x01, 0x00, // run_start=0x00010203 (LE)
        END_FLAG,
    ];
    let (_, blocks) = decode_all(&data);
    assert_eq!(blocks, vec![0x00010203]);
}

/// TOKEN_REL (0x80 + offset) encodes a relative block index in the low 6 bits.
/// rx_token starts at 0, so TOKEN_REL + 5 means block 5.
/// Reference: token.c line 589 `rx_token += flag & 0x3f`.
#[test]
fn golden_token_rel_offset_5() {
    let data = [
        TOKEN_REL | 5, // rx_token = 0 + 5 = 5
        END_FLAG,
    ];
    let (_, blocks) = decode_all(&data);
    assert_eq!(blocks, vec![5]);
}

/// TOKEN_REL with offset 0 means the current rx_token value.
#[test]
fn golden_token_rel_offset_0() {
    let data = [
        TOKEN_REL | 0, // rx_token = 0 + 0 = 0
        END_FLAG,
    ];
    let (_, blocks) = decode_all(&data);
    assert_eq!(blocks, vec![0]);
}

/// TOKEN_REL maximum relative offset is 63 (6 bits).
#[test]
fn golden_token_rel_max_offset_63() {
    let data = [
        TOKEN_REL | 63, // rx_token = 0 + 63 = 63
        END_FLAG,
    ];
    let (_, blocks) = decode_all(&data);
    assert_eq!(blocks, vec![63]);
}

/// TOKENRUN_LONG (0x21) encodes an absolute block index (4 bytes LE) followed
/// by a 16-bit LE run count. The first token is run_start, then run_count
/// additional consecutive tokens follow.
///
/// Reference: token.c lines 391-397.
///
/// Upstream sends: write_byte(f, TOKENRUN_LONG); write_int(f, run_start);
/// write_byte(f, n); write_byte(f, n >> 8);
/// where n = last_token - run_start.
///
/// The decoder returns run_start first, then increments rx_token for each of
/// the n additional tokens (rx_run counts down from n).
#[test]
fn golden_tokenrun_long_3_consecutive_from_100() {
    // run_start=100, n=2 (run count), total = 3 tokens: 100, 101, 102
    let data = [
        TOKENRUN_LONG,
        100,
        0,
        0,
        0, // run_start=100 (LE)
        2,
        0, // n=2 (LE 16-bit)
        END_FLAG,
    ];
    let (_, blocks) = decode_all(&data);
    assert_eq!(blocks, vec![100, 101, 102]);
}

/// TOKENRUN_REL (0xC0 + offset) encodes a relative block index + 16-bit run count.
/// Reference: token.c lines 589-598.
///
/// rx_token starts at 0. TOKENRUN_REL + 10 means rx_token += 10 = 10,
/// then read 2 bytes for run count.
#[test]
fn golden_tokenrun_rel_from_10_count_2() {
    // rx_token=0+10=10, run_count=2, total = 3 tokens: 10, 11, 12
    let data = [
        TOKENRUN_REL | 10, // rx_token = 0 + 10 = 10
        2,
        0, // n=2 (LE 16-bit)
        END_FLAG,
    ];
    let (_, blocks) = decode_all(&data);
    assert_eq!(blocks, vec![10, 11, 12]);
}

/// Multiple TOKEN_REL tokens in sequence correctly accumulate rx_token.
/// After receiving block N, the decoder increments rx_token to N+1.
/// The next TOKEN_REL offset is relative to that new rx_token.
///
/// Reference: token.c line 589 `rx_token += flag & 0x3f` then line 599
/// `return -1 - rx_token` (and rx_token is incremented after).
#[test]
fn golden_two_token_rel_accumulating() {
    let data = [
        TOKEN_REL | 5,  // rx_token=0+5=5, returns 5, rx_token becomes 6
        TOKEN_REL | 10, // rx_token=6+10=16, returns 16
        END_FLAG,
    ];
    let (_, blocks) = decode_all(&data);
    assert_eq!(blocks, vec![5, 16]);
}

/// TOKEN_REL followed by TOKENRUN_REL to test mixed relative encodings.
#[test]
fn golden_token_rel_then_tokenrun_rel() {
    let data = [
        TOKEN_REL | 3,    // rx_token=0+3=3, returns 3, rx_token=4
        TOKENRUN_REL | 2, // rx_token=4+2=6, run_count=3, returns 6,7,8
        3,
        0, // n=3 (LE 16-bit)
        END_FLAG,
    ];
    let (_, blocks) = decode_all(&data);
    assert_eq!(blocks, vec![3, 6, 7, 8, 9]);
}

/// DEFLATED_DATA header format: first byte = 0x40 | (len >> 8),
/// second byte = len & 0xFF. Maximum length is 16383 (14 bits).
///
/// Reference: token.c lines 325, 451-453:
/// `obuf[0] = DEFLATED_DATA + (n >> 8); obuf[1] = n;`
#[test]
fn golden_deflated_data_header_length_encoding() {
    // Verify the 14-bit length encoding for various values
    for len in [0usize, 1, 255, 256, 1000, 8192, MAX_DATA_COUNT] {
        let header_byte0 = DEFLATED_DATA | ((len >> 8) as u8);
        let header_byte1 = (len & 0xFF) as u8;

        // Decode: high 6 bits from byte0, low 8 bits from byte1
        let decoded = ((header_byte0 & 0x3F) as usize) << 8 | header_byte1 as usize;
        assert_eq!(
            decoded, len,
            "Length {len} round-trips through header encoding"
        );
    }
}

/// Verify the DEFLATED_DATA flag byte is recognized correctly even when
/// the length bits vary. The flag check is `(flag & 0xC0) == DEFLATED_DATA`.
///
/// Reference: token.c line 535 `if ((flag & 0xC0) == DEFLATED_DATA)`.
#[test]
fn golden_deflated_data_flag_mask() {
    // DEFLATED_DATA = 0x40. Any byte with bits [7:6] == 01 is DEFLATED_DATA.
    for high_bits in 0..=0x3Fu8 {
        let flag = DEFLATED_DATA | high_bits;
        assert_eq!(flag & 0xC0, DEFLATED_DATA);
    }

    // Verify other flag ranges are NOT DEFLATED_DATA
    assert_ne!(END_FLAG & 0xC0, DEFLATED_DATA);
    assert_ne!(TOKEN_LONG & 0xC0, DEFLATED_DATA);
    assert_ne!(TOKEN_REL & 0xC0, DEFLATED_DATA);
    assert_ne!(TOKENRUN_REL & 0xC0, DEFLATED_DATA);
}

/// Construct a valid DEFLATED_DATA packet with actual zlib-compressed content
/// and verify the decoder inflates it correctly. This simulates what upstream
/// rsync sends: a DEFLATED_DATA header followed by raw deflate data (no zlib
/// header), with the 4-byte sync marker stripped.
///
/// Reference: token.c lines 440-455: compress with Z_SYNC_FLUSH, strip trailing
/// 0x00 0x00 0xFF 0xFF, write DEFLATED_DATA header + compressed bytes.
#[test]
fn golden_deflated_data_with_real_compressed_content() {
    use flate2::{Compress, Compression, FlushCompress};

    let input = b"Hello from upstream rsync compressed token stream!";

    // Compress with raw deflate (window bits = -15, matching upstream)
    let mut compressor = Compress::new(Compression::default(), false);
    let mut compressed = vec![0u8; input.len() * 2 + 64];

    compressor
        .compress(input, &mut compressed, FlushCompress::None)
        .unwrap();
    // Flush with Z_SYNC_FLUSH
    loop {
        let before = compressor.total_out();
        let status = compressor
            .compress(&[], &mut compressed[before as usize..], FlushCompress::Sync)
            .unwrap();
        if status == flate2::Status::Ok {
            break;
        }
    }
    let total = compressor.total_out() as usize;
    compressed.truncate(total);

    // Strip trailing sync marker (0x00 0x00 0xFF 0xFF)
    assert!(compressed.len() >= 4);
    assert_eq!(
        &compressed[compressed.len() - 4..],
        &[0x00, 0x00, 0xFF, 0xFF]
    );
    compressed.truncate(compressed.len() - 4);

    // Build wire packet: DEFLATED_DATA header + compressed bytes + END_FLAG
    let len = compressed.len();
    assert!(len <= MAX_DATA_COUNT);
    let mut wire = Vec::new();
    wire.push(DEFLATED_DATA | ((len >> 8) as u8));
    wire.push((len & 0xFF) as u8);
    wire.extend_from_slice(&compressed);
    wire.push(END_FLAG);

    // Decode and verify
    let (literals, blocks) = decode_all(&wire);
    assert!(blocks.is_empty());
    assert_eq!(literals, input);
}

/// Mixed token stream: DEFLATED_DATA (literal) + TOKEN_REL (block match) + END_FLAG.
/// This simulates a typical upstream rsync file transfer where literal data is
/// interleaved with block matches.
#[test]
fn golden_mixed_literal_and_block_match() {
    use flate2::{Compress, Compression, FlushCompress};

    let literal_data = b"literal bytes before a block match";

    // Compress literal data
    let mut compressor = Compress::new(Compression::default(), false);
    let mut compressed = vec![0u8; literal_data.len() * 2 + 64];

    compressor
        .compress(literal_data, &mut compressed, FlushCompress::None)
        .unwrap();
    loop {
        let before = compressor.total_out();
        let status = compressor
            .compress(&[], &mut compressed[before as usize..], FlushCompress::Sync)
            .unwrap();
        if status == flate2::Status::Ok {
            break;
        }
    }
    let total = compressor.total_out() as usize;
    compressed.truncate(total);

    // Strip sync marker
    compressed.truncate(compressed.len() - 4);

    // Build wire: DEFLATED_DATA + TOKEN_REL(block 0) + END_FLAG
    let len = compressed.len();
    let mut wire = Vec::new();
    wire.push(DEFLATED_DATA | ((len >> 8) as u8));
    wire.push((len & 0xFF) as u8);
    wire.extend_from_slice(&compressed);
    wire.push(TOKEN_REL | 0); // block 0
    wire.push(END_FLAG);

    let (literals, blocks) = decode_all(&wire);
    assert_eq!(literals, literal_data);
    assert_eq!(blocks, vec![0]);
}

// ===========================================================================
// Section 2: Encoder format tests — verify our encoder produces upstream format
// ===========================================================================

/// Verify the encoder writes END_FLAG (0x00) as the final byte.
/// Reference: token.c line 462 `write_byte(f, END_FLAG)`.
#[test]
fn encoder_end_marker_is_zero_byte() {
    let mut encoder = CompressedTokenEncoder::new(CompressionLevel::Default, 31);
    let mut output = Vec::new();
    encoder.finish(&mut output).unwrap();

    assert!(!output.is_empty());
    assert_eq!(
        *output.last().unwrap(),
        END_FLAG,
        "stream must end with END_FLAG (0x00)"
    );
}

/// Verify single block match at index 0 uses TOKEN_REL encoding.
/// When run_start=0 and last_run_end=0, relative offset r=0, which fits in 6 bits.
/// Reference: token.c line 389 `if (r >= 0 && r <= 63)`.
#[test]
fn encoder_single_block_uses_token_rel() {
    let mut encoder = CompressedTokenEncoder::new(CompressionLevel::Default, 31);
    let mut output = Vec::new();
    encoder.send_block_match(&mut output, 0).unwrap();
    encoder.finish(&mut output).unwrap();

    // Expected: TOKEN_REL | 0 (= 0x80), END_FLAG (= 0x00)
    assert_eq!(output.len(), 2);
    assert_eq!(output[0], TOKEN_REL); // 0x80 | 0 = 0x80
    assert_eq!(output[1], END_FLAG);
}

/// Verify consecutive blocks use TOKENRUN_REL encoding.
/// Blocks 0,1,2: run_start=0, last_token=2, n=2, r=0.
/// Reference: token.c lines 389-397.
#[test]
fn encoder_consecutive_blocks_use_tokenrun_rel() {
    let mut encoder = CompressedTokenEncoder::new(CompressionLevel::Default, 31);
    let mut output = Vec::new();
    encoder.send_block_match(&mut output, 0).unwrap();
    encoder.send_block_match(&mut output, 1).unwrap();
    encoder.send_block_match(&mut output, 2).unwrap();
    encoder.finish(&mut output).unwrap();

    // Expected: TOKENRUN_REL | 0 (= 0xC0), n_lo=2, n_hi=0, END_FLAG
    assert_eq!(output.len(), 4);
    assert_eq!(output[0], TOKENRUN_REL); // 0xC0 | 0 = 0xC0
    assert_eq!(output[1], 2); // n & 0xFF = 2
    assert_eq!(output[2], 0); // n >> 8 = 0
    assert_eq!(output[3], END_FLAG);
}

/// Verify block with index > 63 from initial position uses TOKEN_LONG.
/// When r = run_start - last_run_end > 63, absolute encoding is needed.
/// Reference: token.c line 391 `write_byte(f, TOKEN_LONG); write_int(f, run_start)`.
#[test]
fn encoder_large_offset_uses_token_long() {
    let mut encoder = CompressedTokenEncoder::new(CompressionLevel::Default, 31);
    let mut output = Vec::new();
    encoder.send_block_match(&mut output, 100).unwrap();
    encoder.finish(&mut output).unwrap();

    // Expected: TOKEN_LONG (0x20), run_start=100 as 4-byte LE, END_FLAG
    assert_eq!(output.len(), 6);
    assert_eq!(output[0], TOKEN_LONG);
    assert_eq!(output[1..5], 100i32.to_le_bytes());
    assert_eq!(output[5], END_FLAG);
}

/// Verify consecutive blocks with large initial index use TOKENRUN_LONG.
/// Blocks 100,101: run_start=100, n=1, r=100 > 63 -> absolute encoding.
#[test]
fn encoder_consecutive_large_offset_uses_tokenrun_long() {
    let mut encoder = CompressedTokenEncoder::new(CompressionLevel::Default, 31);
    let mut output = Vec::new();
    encoder.send_block_match(&mut output, 100).unwrap();
    encoder.send_block_match(&mut output, 101).unwrap();
    encoder.finish(&mut output).unwrap();

    // Expected: TOKENRUN_LONG (0x21), run_start=100 LE, n=1 LE16, END_FLAG
    assert_eq!(output.len(), 8);
    assert_eq!(output[0], TOKENRUN_LONG);
    assert_eq!(output[1..5], 100i32.to_le_bytes());
    assert_eq!(output[5], 1); // n & 0xFF
    assert_eq!(output[6], 0); // n >> 8
    assert_eq!(output[7], END_FLAG);
}

/// Non-consecutive blocks produce separate token encodings.
/// Blocks 0, then 5: first run is 0 (relative), second is 5 (relative to last_run_end=1).
/// After first run: last_run_end = 0 + 1 = 1 (last_token + 1).
/// Second run: r = 5 - 1 = 4 which fits in 6 bits.
#[test]
fn encoder_non_consecutive_blocks_separate_tokens() {
    let mut encoder = CompressedTokenEncoder::new(CompressionLevel::Default, 31);
    let mut output = Vec::new();
    encoder.send_block_match(&mut output, 0).unwrap();
    encoder.send_block_match(&mut output, 5).unwrap();
    encoder.finish(&mut output).unwrap();

    // First run: TOKEN_REL | 0 = 0x80
    assert_eq!(output[0], TOKEN_REL);
    // Second run: TOKEN_REL | 4 = 0x84 (r = 5 - 1 = 4)
    assert_eq!(output[1], TOKEN_REL | 4);
    assert_eq!(output[2], END_FLAG);
}

/// Verify DEFLATED_DATA header bytes are correctly formed by the encoder.
/// The first byte must have bits [7:6] = 01 (DEFLATED_DATA flag range).
#[test]
fn encoder_literal_produces_deflated_data_header() {
    let mut encoder = CompressedTokenEncoder::new(CompressionLevel::Default, 31);
    let mut output = Vec::new();

    encoder
        .send_literal(&mut output, b"test literal data for header verification")
        .unwrap();
    encoder.finish(&mut output).unwrap();

    // First byte should be in DEFLATED_DATA range
    assert_eq!(
        output[0] & 0xC0,
        DEFLATED_DATA,
        "first byte flag bits must be DEFLATED_DATA"
    );

    // Last byte is END_FLAG
    assert_eq!(*output.last().unwrap(), END_FLAG);
}

/// Verify the encoder correctly handles the run length limit of 65536.
/// Upstream: token.c line 384 `token >= run_start + 65536`.
/// A run of exactly 65536 consecutive blocks should fit in one TOKENRUN.
/// A run of 65537 would need to be split.
#[test]
fn encoder_run_length_boundary_65535() {
    let mut encoder = CompressedTokenEncoder::new(CompressionLevel::Default, 31);
    let mut output = Vec::new();

    // Send blocks 0..=65535 (65536 tokens, run count = 65535)
    for i in 0..=65535u32 {
        encoder.send_block_match(&mut output, i).unwrap();
    }
    encoder.finish(&mut output).unwrap();

    // Verify all blocks decode correctly
    let (_, blocks) = decode_all(&output);
    assert_eq!(blocks.len(), 65536);
    for (i, &b) in blocks.iter().enumerate() {
        assert_eq!(b, i as u32);
    }
}

// ===========================================================================
// Section 3: Round-trip tests — encode then decode with various data patterns
// ===========================================================================

/// Round-trip: empty file (no literals, no blocks, just END_FLAG).
#[test]
fn roundtrip_empty_file() {
    let encoded = encode_stream(&[]);
    let (literals, blocks) = decode_all(&encoded);
    assert!(literals.is_empty());
    assert!(blocks.is_empty());
}

/// Round-trip: single-byte literal.
#[test]
fn roundtrip_single_byte_literal() {
    let encoded = encode_stream(&[TestToken::Literal(vec![0x42])]);
    let (literals, blocks) = decode_all(&encoded);
    assert_eq!(literals, vec![0x42]);
    assert!(blocks.is_empty());
}

/// Round-trip: single block match only (no literal data).
#[test]
fn roundtrip_single_block_match() {
    let encoded = encode_stream(&[TestToken::BlockMatch(7)]);
    let (literals, blocks) = decode_all(&encoded);
    assert!(literals.is_empty());
    assert_eq!(blocks, vec![7]);
}

/// Round-trip: alternating literal and block match tokens.
#[test]
fn roundtrip_alternating_literal_block() {
    let tokens = vec![
        TestToken::Literal(b"chunk A".to_vec()),
        TestToken::BlockMatch(0),
        TestToken::Literal(b"chunk B".to_vec()),
        TestToken::BlockMatch(5),
        TestToken::Literal(b"chunk C".to_vec()),
        TestToken::BlockMatch(10),
    ];
    let encoded = encode_stream(&tokens);
    let (literals, blocks) = decode_all(&encoded);

    let expected_literals = b"chunk Achunk Bchunk C";
    assert_eq!(literals, expected_literals);
    assert_eq!(blocks, vec![0, 5, 10]);
}

/// Round-trip: mostly-zero data (tests compression efficiency).
#[test]
fn roundtrip_mostly_zeros() {
    let data = vec![0u8; 4096];
    let encoded = encode_stream(&[TestToken::Literal(data.clone())]);
    let (literals, blocks) = decode_all(&encoded);
    assert_eq!(literals, data);
    assert!(blocks.is_empty());

    // Compressed stream should be significantly smaller than raw data
    assert!(
        encoded.len() < data.len() / 2,
        "compression should reduce mostly-zero data significantly \
         (encoded: {} bytes, raw: {} bytes)",
        encoded.len(),
        data.len()
    );
}

/// Round-trip: repetitive data pattern (good compression).
#[test]
fn roundtrip_repetitive_pattern() {
    let pattern: Vec<u8> = b"The quick brown fox jumps over the lazy dog. "
        .repeat(100)
        .to_vec();
    let encoded = encode_stream(&[TestToken::Literal(pattern.clone())]);
    let (literals, _blocks) = decode_all(&encoded);
    assert_eq!(literals, pattern);
}

/// Round-trip: random-looking data (poor compression, tests correctness).
#[test]
fn roundtrip_pseudorandom_data() {
    // LCG pseudo-random to avoid needing rand dependency
    let mut state: u32 = 0xDEADBEEF;
    let data: Vec<u8> = (0..2048)
        .map(|_| {
            state = state.wrapping_mul(1103515245).wrapping_add(12345);
            (state >> 16) as u8
        })
        .collect();

    let encoded = encode_stream(&[TestToken::Literal(data.clone())]);
    let (literals, _blocks) = decode_all(&encoded);
    assert_eq!(literals, data);
}

/// Round-trip: data larger than CHUNK_SIZE (32 KiB) to exercise multi-chunk
/// compression path.
#[test]
fn roundtrip_multi_chunk_literal() {
    let data: Vec<u8> = (0..100_000).map(|i| (i % 251) as u8).collect();
    let encoded = encode_stream(&[TestToken::Literal(data.clone())]);
    let (literals, blocks) = decode_all(&encoded);
    assert_eq!(literals, data);
    assert!(blocks.is_empty());
}

/// Round-trip: many small literals interleaved with block matches.
/// Simulates a file with many small unmatched regions between blocks.
#[test]
fn roundtrip_many_small_literals_with_blocks() {
    let mut tokens = Vec::new();
    let mut expected_literals = Vec::new();

    for i in 0u32..50 {
        let literal = format!("segment_{i:03}|");
        expected_literals.extend_from_slice(literal.as_bytes());
        tokens.push(TestToken::Literal(literal.into_bytes()));
        tokens.push(TestToken::BlockMatch(i));
    }

    let encoded = encode_stream(&tokens);
    let (literals, blocks) = decode_all(&encoded);
    assert_eq!(literals, expected_literals);
    assert_eq!(blocks, (0..50).collect::<Vec<_>>());
}

/// Round-trip: consecutive block matches forming a long run.
/// Tests run-length encoding with a run of 1000 consecutive blocks.
#[test]
fn roundtrip_long_consecutive_run() {
    let tokens: Vec<TestToken> = (0..1000).map(TestToken::BlockMatch).collect();
    let encoded = encode_stream(&tokens);
    let (literals, blocks) = decode_all(&encoded);
    assert!(literals.is_empty());
    assert_eq!(blocks, (0..1000).collect::<Vec<_>>());
}

/// Round-trip: multiple separate runs of consecutive blocks.
#[test]
fn roundtrip_multiple_separate_runs() {
    let tokens: Vec<TestToken> = [
        // Run 1: blocks 0-4
        (0..5).map(TestToken::BlockMatch).collect::<Vec<_>>(),
        // Run 2: blocks 100-104
        (100..105).map(TestToken::BlockMatch).collect::<Vec<_>>(),
        // Run 3: blocks 200-204
        (200..205).map(TestToken::BlockMatch).collect::<Vec<_>>(),
    ]
    .concat();

    let encoded = encode_stream(&tokens);
    let (_, blocks) = decode_all(&encoded);

    let expected: Vec<u32> = (0..5).chain(100..105).chain(200..205).collect();
    assert_eq!(blocks, expected);
}

/// Round-trip: blocks only, no literals, then verify encoder/decoder agree
/// on the exact block indices when using different encoding strategies.
#[test]
fn roundtrip_mixed_relative_and_absolute_blocks() {
    let block_indices = vec![0, 1, 2, 3, 50, 51, 52, 200, 201];
    let tokens: Vec<TestToken> = block_indices
        .iter()
        .map(|&i| TestToken::BlockMatch(i))
        .collect();

    let encoded = encode_stream(&tokens);
    let (_, blocks) = decode_all(&encoded);
    assert_eq!(blocks, block_indices);
}

// ===========================================================================
// Section 4: Edge case and boundary tests
// ===========================================================================

/// Verify that encoder reuse across files produces valid streams.
/// Upstream rsync reuses the same compressor across files in a transfer,
/// calling deflateReset() between files.
/// Reference: token.c lines 363-378.
#[test]
fn encoder_reuse_across_files() {
    let mut encoder = CompressedTokenEncoder::new(CompressionLevel::Default, 31);

    for file_idx in 0..5 {
        let mut output = Vec::new();
        let data = format!("file {file_idx} content with some data to compress");
        encoder.send_literal(&mut output, data.as_bytes()).unwrap();
        encoder.send_block_match(&mut output, file_idx).unwrap();
        encoder.finish(&mut output).unwrap();

        let (literals, blocks) = decode_all(&output);
        assert_eq!(literals, data.as_bytes());
        assert_eq!(blocks, vec![file_idx]);

        encoder.reset();
    }
}

/// Verify that decoder reuse across files produces correct results.
#[test]
fn decoder_reuse_across_files() {
    let mut decoder = CompressedTokenDecoder::new();

    for file_idx in 0u32..5 {
        let mut encoder = CompressedTokenEncoder::new(CompressionLevel::Default, 31);
        let mut output = Vec::new();
        let data = format!("content for file {file_idx}");
        encoder.send_literal(&mut output, data.as_bytes()).unwrap();
        encoder.finish(&mut output).unwrap();

        let mut cursor = Cursor::new(&output);
        let mut literals = Vec::new();

        loop {
            match decoder.recv_token(&mut cursor).unwrap() {
                CompressedToken::Literal(chunk) => literals.extend_from_slice(&chunk),
                CompressedToken::End => break,
                CompressedToken::BlockMatch(_) => {}
            }
        }

        assert_eq!(literals, data.as_bytes());
        decoder.reset();
    }
}

/// Exactly CHUNK_SIZE (32 KiB) of literal data — the boundary where the
/// encoder flushes its first chunk.
#[test]
fn roundtrip_exactly_chunk_size() {
    let data = vec![0xABu8; 32 * 1024];
    let encoded = encode_stream(&[TestToken::Literal(data.clone())]);
    let (literals, blocks) = decode_all(&encoded);
    assert_eq!(literals, data);
    assert!(blocks.is_empty());
}

/// One byte less than CHUNK_SIZE — data stays in buffer until finish().
#[test]
fn roundtrip_chunk_size_minus_one() {
    let data = vec![0xCDu8; 32 * 1024 - 1];
    let encoded = encode_stream(&[TestToken::Literal(data.clone())]);
    let (literals, blocks) = decode_all(&encoded);
    assert_eq!(literals, data);
    assert!(blocks.is_empty());
}

/// One byte more than CHUNK_SIZE — triggers a flush mid-stream, with one byte
/// remaining for the next chunk.
#[test]
fn roundtrip_chunk_size_plus_one() {
    let data = vec![0xEFu8; 32 * 1024 + 1];
    let encoded = encode_stream(&[TestToken::Literal(data.clone())]);
    let (literals, blocks) = decode_all(&encoded);
    assert_eq!(literals, data);
    assert!(blocks.is_empty());
}

/// Exactly 2x CHUNK_SIZE to test two full-chunk flushes.
#[test]
fn roundtrip_two_full_chunks() {
    let data: Vec<u8> = (0..2 * 32 * 1024).map(|i| (i % 256) as u8).collect();
    let encoded = encode_stream(&[TestToken::Literal(data.clone())]);
    let (literals, blocks) = decode_all(&encoded);
    assert_eq!(literals, data);
    assert!(blocks.is_empty());
}

/// All compression levels produce decodable output for the same input.
/// Reference: token.c line 369 `deflateInit2(&tx_strm, per_file_default_level, ...)`.
#[test]
fn all_compression_levels_roundtrip() {
    use std::num::NonZeroU8;

    let data = b"Common test data used across all compression levels for verification.".repeat(10);
    let levels = [
        CompressionLevel::Fast,
        CompressionLevel::Default,
        CompressionLevel::Best,
        CompressionLevel::Precise(NonZeroU8::new(1).unwrap()),
        CompressionLevel::Precise(NonZeroU8::new(5).unwrap()),
        CompressionLevel::Precise(NonZeroU8::new(9).unwrap()),
    ];

    for level in levels {
        let mut encoder = CompressedTokenEncoder::new(level, 31);
        let mut output = Vec::new();
        encoder.send_literal(&mut output, &data).unwrap();
        encoder.finish(&mut output).unwrap();

        let (literals, _) = decode_all(&output);
        assert_eq!(
            literals, data,
            "round-trip failed for compression level {level:?}"
        );
    }
}

/// Protocol version 30 (pre-fix) vs 31 (post-fix) both produce decodable output.
/// The data-duplicating bug in protocol < 31 only affects dictionary sync
/// (`see_token`), not the wire format itself.
#[test]
fn protocol_version_30_and_31_both_produce_valid_wire_format() {
    let data = b"data that exercises both protocol paths";

    for version in [30, 31, 32] {
        let mut encoder = CompressedTokenEncoder::new(CompressionLevel::Default, version);
        let mut output = Vec::new();
        encoder.send_literal(&mut output, data).unwrap();
        encoder.send_block_match(&mut output, 0).unwrap();
        encoder.finish(&mut output).unwrap();

        let (literals, blocks) = decode_all(&output);
        assert_eq!(
            literals, data,
            "protocol version {version} literal mismatch"
        );
        assert_eq!(blocks, vec![0], "protocol version {version} block mismatch");
    }
}

/// Verify upstream's token accumulation rule: after returning block N,
/// rx_token becomes N+1. Multiple TOKEN_REL encodings in sequence must
/// each account for this increment.
///
/// Reference: token.c line 599 `return -1 - rx_token;` — the decoder
/// returns the token value and the caller (in upstream) knows that
/// rx_token has already been incremented.
#[test]
fn golden_rx_token_accumulation_chain() {
    // Build a chain of 5 relative tokens:
    // Initial rx_token = 0
    // Token 1: TOKEN_REL | 0  -> block 0, rx_token = 1
    // Token 2: TOKEN_REL | 0  -> block 1, rx_token = 2
    // Token 3: TOKEN_REL | 3  -> block 5, rx_token = 6
    // Token 4: TOKEN_REL | 0  -> block 6, rx_token = 7
    // Token 5: TOKEN_REL | 10 -> block 17, rx_token = 18
    let data = [
        TOKEN_REL | 0,  // block 0
        TOKEN_REL | 0,  // block 1
        TOKEN_REL | 3,  // block 5
        TOKEN_REL | 0,  // block 6
        TOKEN_REL | 10, // block 17
        END_FLAG,
    ];
    let (_, blocks) = decode_all(&data);
    assert_eq!(blocks, vec![0, 1, 5, 6, 17]);
}

/// Verify upstream's run count interpretation: when a TOKENRUN is received,
/// the run_count value is the number of ADDITIONAL tokens after the first.
///
/// Example: TOKENRUN_REL with rx_token=0 and n=4 produces blocks 0,1,2,3,4
/// (5 total = 1 initial + 4 from run).
///
/// Reference: token.c lines 594-598, 618-622.
#[test]
fn golden_run_count_is_additional_tokens() {
    let data = [
        TOKENRUN_REL | 0, // rx_token = 0, returns block 0
        4,
        0, // n=4, produces blocks 1,2,3,4
        END_FLAG,
    ];
    let (_, blocks) = decode_all(&data);
    assert_eq!(blocks, vec![0, 1, 2, 3, 4]);
}

/// Maximum 16-bit run count (65535 additional tokens).
#[test]
fn golden_max_run_count_65535() {
    let data = [
        TOKENRUN_REL | 0, // rx_token = 0, returns block 0
        0xFF,
        0xFF, // n=65535
        END_FLAG,
    ];
    let (_, blocks) = decode_all(&data);
    assert_eq!(blocks.len(), 65536);
    assert_eq!(blocks[0], 0);
    assert_eq!(blocks[65535], 65535);
}

/// Verify that TOKENRUN_LONG correctly reads a 32-bit LE token followed by
/// a 16-bit LE run count. This matches upstream's `write_int(f, run_start)`
/// followed by `write_byte(f, n); write_byte(f, n >> 8)`.
///
/// Reference: token.c lines 391-397.
#[test]
fn golden_tokenrun_long_wire_format() {
    // run_start = 0x00001000 = 4096, n = 0x0100 = 256
    let data = [
        TOKENRUN_LONG,
        0x00,
        0x10,
        0x00,
        0x00, // run_start=4096 (LE)
        0x00,
        0x01, // n=256 (LE 16-bit)
        END_FLAG,
    ];
    let (_, blocks) = decode_all(&data);
    // 257 total blocks: 4096 through 4352
    assert_eq!(blocks.len(), 257);
    assert_eq!(blocks[0], 4096);
    assert_eq!(blocks[256], 4352);
}

/// Upstream's decoder uses `flag >> 6` to check for the run bit after
/// extracting the relative offset. For TOKEN_REL (0x80-0xBF), flag >> 6 = 2,
/// and `(flag >> 6) & 1 = 0` means no run. For TOKENRUN_REL (0xC0-0xFF),
/// flag >> 6 = 3, and `(flag >> 6) & 1 = 1` means there IS a run.
///
/// Reference: token.c lines 589-598.
#[test]
fn golden_flag_bit_extraction() {
    // TOKEN_REL range: 0x80..=0xBF
    for offset in 0..=63u8 {
        let flag = TOKEN_REL | offset;
        assert_eq!(flag & 0xC0, 0x80, "TOKEN_REL flag check");
        assert_eq!((flag >> 6) & 1, 0, "TOKEN_REL has no run bit");
    }

    // TOKENRUN_REL range: 0xC0..=0xFF
    for offset in 0..=63u8 {
        let flag = TOKENRUN_REL | offset;
        assert_eq!(flag & 0xC0, 0xC0, "TOKENRUN_REL flag check");
        assert_eq!((flag >> 6) & 1, 1, "TOKENRUN_REL has run bit set");
    }
}

/// Verify the upstream distinction between TOKEN_LONG (0x20) and
/// TOKENRUN_LONG (0x21): they differ only in bit 0.
///
/// Reference: token.c lines 323-324.
#[test]
fn golden_token_long_vs_tokenrun_long_bit0() {
    assert_eq!(TOKEN_LONG & 0xFE, TOKENRUN_LONG & 0xFE);
    assert_eq!(TOKEN_LONG & 1, 0, "TOKEN_LONG has bit 0 clear");
    assert_eq!(TOKENRUN_LONG & 1, 1, "TOKENRUN_LONG has bit 0 set");
}

/// Verify that DEFLATED_DATA length of zero is valid (empty compressed block).
/// This can occur when sync-flush produces only the marker bytes.
#[test]
fn golden_deflated_data_zero_length() {
    // DEFLATED_DATA with length 0: just the header bytes, no compressed data.
    // This is immediately followed by END_FLAG. The decoder should handle this
    // gracefully (no literal output).
    let data = [
        DEFLATED_DATA, // 0x40 | 0 = 0x40
        0x00,          // low byte = 0
        END_FLAG,
    ];
    let mut cursor = Cursor::new(&data[..]);
    let mut decoder = CompressedTokenDecoder::new();

    // Should eventually reach END_FLAG
    loop {
        match decoder.recv_token(&mut cursor).unwrap() {
            CompressedToken::End => break,
            CompressedToken::Literal(d) => {
                assert!(
                    d.is_empty(),
                    "zero-length DEFLATED_DATA should produce no literal"
                );
            }
            CompressedToken::BlockMatch(_) => panic!("unexpected block match"),
        }
    }
}

/// Verify that the wire format is deterministic: encoding the same tokens
/// with the same settings produces identical byte streams.
#[test]
fn encoder_deterministic_output() {
    let make_stream = || {
        let mut encoder = CompressedTokenEncoder::new(CompressionLevel::Default, 31);
        let mut output = Vec::new();
        encoder
            .send_literal(&mut output, b"deterministic test data")
            .unwrap();
        encoder.send_block_match(&mut output, 0).unwrap();
        encoder.send_block_match(&mut output, 1).unwrap();
        encoder.send_literal(&mut output, b"more data").unwrap();
        encoder.send_block_match(&mut output, 10).unwrap();
        encoder.finish(&mut output).unwrap();
        output
    };

    let stream1 = make_stream();
    let stream2 = make_stream();
    assert_eq!(
        stream1, stream2,
        "encoder must produce deterministic output"
    );
}

/// Test that the decoder handles the upstream sequence where a TOKEN_LONG
/// immediately follows DEFLATED_DATA (no token run between them).
/// This is the common pattern in upstream rsync when a single block match
/// follows literal data.
#[test]
fn golden_deflated_data_then_token_long() {
    use flate2::{Compress, Compression, FlushCompress};

    let input = b"short literal";
    let mut compressor = Compress::new(Compression::default(), false);
    let mut compressed = vec![0u8; 256];

    compressor
        .compress(input, &mut compressed, FlushCompress::None)
        .unwrap();
    loop {
        let before = compressor.total_out();
        let status = compressor
            .compress(&[], &mut compressed[before as usize..], FlushCompress::Sync)
            .unwrap();
        if status == flate2::Status::Ok {
            break;
        }
    }
    let total = compressor.total_out() as usize;
    compressed.truncate(total);
    compressed.truncate(compressed.len() - 4); // strip sync marker

    let len = compressed.len();
    let mut wire = Vec::new();
    // DEFLATED_DATA header
    wire.push(DEFLATED_DATA | ((len >> 8) as u8));
    wire.push((len & 0xFF) as u8);
    wire.extend_from_slice(&compressed);
    // TOKEN_LONG for block 999
    wire.push(TOKEN_LONG);
    wire.extend_from_slice(&999i32.to_le_bytes());
    // END_FLAG
    wire.push(END_FLAG);

    let (literals, blocks) = decode_all(&wire);
    assert_eq!(literals, input);
    assert_eq!(blocks, vec![999]);
}
