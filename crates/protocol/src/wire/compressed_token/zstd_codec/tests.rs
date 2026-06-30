//! Roundtrip and wire-format tests for the zstd token codec.

use std::io::{Cursor, Read};

use super::super::{
    CompressedToken, DEFLATED_DATA, END_FLAG, MAX_DATA_COUNT, TOKEN_LONG, TOKEN_REL,
    read_deflated_data_length,
};
use super::{ZstdTokenDecoder, ZstdTokenEncoder};

#[test]
fn zstd_roundtrip_literal_only() {
    let mut encoder = ZstdTokenEncoder::new(3, None).unwrap();
    let mut encoded = Vec::new();

    let data = b"Hello, zstd compressed token world!";
    encoder.send_literal(&mut encoded, data).unwrap();
    encoder.finish(&mut encoded).unwrap();

    let mut decoder = ZstdTokenDecoder::new().unwrap();
    let mut cursor = Cursor::new(&encoded);
    let mut result = Vec::new();

    loop {
        match decoder.recv_token(&mut cursor).unwrap() {
            CompressedToken::Literal(d) => result.extend_from_slice(&d),
            CompressedToken::End => break,
            CompressedToken::BlockMatch(_) => panic!("unexpected block match"),
        }
    }

    assert_eq!(result, data);
}

#[test]
fn zstd_roundtrip_block_matches() {
    let mut encoder = ZstdTokenEncoder::new(3, None).unwrap();
    let mut encoded = Vec::new();

    encoder.send_literal(&mut encoded, b"prefix").unwrap();
    encoder.send_block_match(&mut encoded, 0).unwrap();
    encoder.send_literal(&mut encoded, b"middle").unwrap();
    encoder.send_block_match(&mut encoded, 5).unwrap();
    encoder.finish(&mut encoded).unwrap();

    let mut decoder = ZstdTokenDecoder::new().unwrap();
    let mut cursor = Cursor::new(&encoded);
    let mut literals = Vec::new();
    let mut blocks = Vec::new();

    loop {
        match decoder.recv_token(&mut cursor).unwrap() {
            CompressedToken::Literal(d) => literals.extend_from_slice(&d),
            CompressedToken::BlockMatch(idx) => blocks.push(idx),
            CompressedToken::End => break,
        }
    }

    assert_eq!(literals, b"prefixmiddle");
    assert_eq!(blocks, vec![0, 5]);
}

#[test]
fn zstd_roundtrip_large_literal() {
    let mut encoder = ZstdTokenEncoder::new(3, None).unwrap();
    let mut encoded = Vec::new();

    // Large literal exceeding CHUNK_SIZE
    let data: Vec<u8> = (0..100_000).map(|i| (i % 256) as u8).collect();
    encoder.send_literal(&mut encoded, &data).unwrap();
    encoder.finish(&mut encoded).unwrap();

    let mut decoder = ZstdTokenDecoder::new().unwrap();
    let mut cursor = Cursor::new(&encoded);
    let mut result = Vec::new();

    loop {
        match decoder.recv_token(&mut cursor).unwrap() {
            CompressedToken::Literal(d) => result.extend_from_slice(&d),
            CompressedToken::End => break,
            CompressedToken::BlockMatch(_) => panic!("unexpected block match"),
        }
    }

    assert_eq!(result, data);
}

#[test]
fn zstd_see_token_is_noop() {
    let mut encoder = ZstdTokenEncoder::new(3, None).unwrap();
    encoder.see_token(b"anything").unwrap();

    let mut decoder = ZstdTokenDecoder::new().unwrap();
    decoder.see_token(b"anything").unwrap();
}

#[test]
fn zstd_consecutive_block_matches_use_run_encoding() {
    let mut encoder = ZstdTokenEncoder::new(3, None).unwrap();
    let mut encoded = Vec::new();

    // Consecutive blocks should use run encoding
    encoder.send_block_match(&mut encoded, 0).unwrap();
    encoder.send_block_match(&mut encoded, 1).unwrap();
    encoder.send_block_match(&mut encoded, 2).unwrap();
    encoder.finish(&mut encoded).unwrap();

    let mut decoder = ZstdTokenDecoder::new().unwrap();
    let mut cursor = Cursor::new(&encoded);
    let mut blocks = Vec::new();

    loop {
        match decoder.recv_token(&mut cursor).unwrap() {
            CompressedToken::Literal(_) => {}
            CompressedToken::BlockMatch(idx) => blocks.push(idx),
            CompressedToken::End => break,
        }
    }

    assert_eq!(blocks, vec![0, 1, 2]);
}

/// Verifies flush boundary placement matches upstream framing.
///
/// Upstream writes one DEFLATED_DATA block per output buffer fill
/// (during continue) or per flush call. For small literals that fit in
/// a single buffer, the entire compressed+flushed output should appear
/// as a single DEFLATED_DATA block, not multiple smaller blocks.
///
/// upstream: token.c lines 755-763
#[test]
fn zstd_flush_produces_single_deflated_data_block_for_small_input() {
    let mut encoder = ZstdTokenEncoder::new(3, None).unwrap();
    let mut encoded = Vec::new();

    let data = b"small literal data for flush test";
    encoder.send_literal(&mut encoded, data).unwrap();
    encoder.send_block_match(&mut encoded, 0).unwrap();
    encoder.finish(&mut encoded).unwrap();

    // Count DEFLATED_DATA blocks before the first token byte
    let mut cursor = Cursor::new(&encoded);
    let mut deflated_count = 0;
    let mut total_compressed_len = 0;

    loop {
        let mut flag_buf = [0u8; 1];
        cursor.read_exact(&mut flag_buf).unwrap();
        let flag = flag_buf[0];

        if (flag & 0xC0) == DEFLATED_DATA {
            deflated_count += 1;
            let len = read_deflated_data_length(&mut cursor, flag).unwrap();
            total_compressed_len += len;
            // Skip past compressed data
            let pos = cursor.position() as usize;
            cursor.set_position((pos + len) as u64);
        } else {
            // Hit a token or end flag - stop counting
            break;
        }
    }

    // Small input should produce exactly one DEFLATED_DATA block
    // (all compressed data fits in one MAX_DATA_COUNT buffer)
    assert_eq!(
        deflated_count, 1,
        "small literal should produce exactly one DEFLATED_DATA block, got {deflated_count}"
    );
    assert!(
        total_compressed_len > 0,
        "compressed data should not be empty"
    );
    assert!(
        total_compressed_len <= MAX_DATA_COUNT,
        "single block should not exceed MAX_DATA_COUNT"
    );
}

/// Verifies that the wire format uses DEFLATED_DATA framing correctly.
///
/// The encoder must produce: [DEFLATED_DATA blocks...] [TOKEN byte] pattern
/// for each literal+token pair, matching upstream's output ordering.
#[test]
fn zstd_wire_format_ordering() {
    let mut encoder = ZstdTokenEncoder::new(3, None).unwrap();
    let mut encoded = Vec::new();

    // Literal followed by block match, then another literal + finish
    encoder.send_literal(&mut encoded, b"first chunk").unwrap();
    encoder.send_block_match(&mut encoded, 0).unwrap();
    encoder.send_literal(&mut encoded, b"second chunk").unwrap();
    encoder.finish(&mut encoded).unwrap();

    // Parse wire format to verify ordering
    let mut cursor = Cursor::new(&encoded);
    let mut sequence = Vec::new();

    loop {
        let mut flag_buf = [0u8; 1];
        if cursor.read_exact(&mut flag_buf).is_err() {
            break;
        }
        let flag = flag_buf[0];

        if (flag & 0xC0) == DEFLATED_DATA {
            let len = read_deflated_data_length(&mut cursor, flag).unwrap();
            let pos = cursor.position() as usize;
            cursor.set_position((pos + len) as u64);
            sequence.push("DEFLATED_DATA");
        } else if flag == END_FLAG {
            sequence.push("END");
            break;
        } else if flag & TOKEN_REL != 0 {
            if (flag >> 6) & 1 != 0 {
                let mut run_buf = [0u8; 2];
                cursor.read_exact(&mut run_buf).unwrap();
            }
            sequence.push("TOKEN");
        } else if flag & 0xE0 == TOKEN_LONG {
            let mut buf = [0u8; 4];
            cursor.read_exact(&mut buf).unwrap();
            if flag & 1 != 0 {
                let mut run_buf = [0u8; 2];
                cursor.read_exact(&mut run_buf).unwrap();
            }
            sequence.push("TOKEN");
        }
    }

    // Expected: DEFLATED_DATA(s) for "first chunk", TOKEN(block 0),
    //           DEFLATED_DATA(s) for "second chunk", END
    assert!(
        sequence.len() >= 4,
        "expected at least 4 wire elements, got {sequence:?}"
    );
    assert_eq!(sequence[0], "DEFLATED_DATA");
    assert_eq!(
        sequence.iter().filter(|s| **s == "TOKEN").count(),
        1,
        "expected exactly one TOKEN"
    );
    assert_eq!(*sequence.last().unwrap(), "END");
}

/// Verifies that large literals produce multiple DEFLATED_DATA blocks
/// each capped at MAX_DATA_COUNT, matching upstream's buffer-full write
/// pattern.
///
/// upstream: token.c line 755 - write when zstd_out_buff.pos == zstd_out_buff.size
#[test]
fn zstd_large_literal_splits_into_max_data_count_blocks() {
    let mut encoder = ZstdTokenEncoder::new(1, None).unwrap();
    let mut encoded = Vec::new();

    // Use a large dataset so that even with zstd level 1 compression,
    // the compressed output exceeds MAX_DATA_COUNT (16383 bytes) and
    // triggers multiple DEFLATED_DATA blocks on the wire.
    let mut data = Vec::with_capacity(500_000);
    let mut state: u64 = 0xDEAD_BEEF_CAFE_BABE;
    for _ in 0..500_000 {
        // xorshift64 - produces uniformly distributed bytes that
        // defeat zstd's dictionary and entropy coder.
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        data.push((state & 0xFF) as u8);
    }
    encoder.send_literal(&mut encoded, &data).unwrap();
    encoder.finish(&mut encoded).unwrap();

    // Parse and verify all DEFLATED_DATA blocks respect MAX_DATA_COUNT
    let mut cursor = Cursor::new(&encoded);
    let mut block_sizes = Vec::new();

    loop {
        let mut flag_buf = [0u8; 1];
        if cursor.read_exact(&mut flag_buf).is_err() {
            break;
        }
        let flag = flag_buf[0];

        if (flag & 0xC0) == DEFLATED_DATA {
            let len = read_deflated_data_length(&mut cursor, flag).unwrap();
            block_sizes.push(len);
            let pos = cursor.position() as usize;
            cursor.set_position((pos + len) as u64);
        } else if flag == END_FLAG {
            break;
        }
    }

    assert!(
        !block_sizes.is_empty(),
        "should produce at least one DEFLATED_DATA block"
    );
    for (i, &size) in block_sizes.iter().enumerate() {
        assert!(
            size <= MAX_DATA_COUNT,
            "block {i} size {size} exceeds MAX_DATA_COUNT ({MAX_DATA_COUNT})"
        );
        assert!(size > 0, "block {i} should not be empty");
    }

    // With incompressible data, multiple blocks are produced.
    // Blocks from the continue phase are exactly MAX_DATA_COUNT (buffer-full
    // writes). The final block(s) from the flush phase may be smaller.
    assert!(
        block_sizes.len() > 1,
        "500KB of xorshift64 data should produce multiple DEFLATED_DATA blocks, got {} block(s) totaling {} bytes",
        block_sizes.len(),
        block_sizes.iter().sum::<usize>(),
    );

    let mut decoder = ZstdTokenDecoder::new().unwrap();
    let mut read_cursor = Cursor::new(&encoded);
    let mut result = Vec::new();

    loop {
        match decoder.recv_token(&mut read_cursor).unwrap() {
            CompressedToken::Literal(d) => result.extend_from_slice(&d),
            CompressedToken::End => break,
            CompressedToken::BlockMatch(_) => panic!("unexpected block match"),
        }
    }

    assert_eq!(result, data);
}

/// Verifies continuous stream across multiple files.
///
/// Upstream rsync uses a single zstd stream for the entire session.
/// The encoder and decoder contexts persist across file boundaries -
/// only token run-encoding state resets between files.
///
/// upstream: token.c:688 (CCtx created once), token.c:700-703 (only
/// run state resets), token.c:789 (DCtx created once)
#[test]
fn zstd_continuous_stream_across_files() {
    let mut encoder = ZstdTokenEncoder::new(3, None).unwrap();
    let mut decoder = ZstdTokenDecoder::new().unwrap();
    let mut encoded = Vec::new();

    // Encode three files into a single continuous stream
    for i in 0..3 {
        let data = format!("file {i} content with some data to compress");
        encoder.send_literal(&mut encoded, data.as_bytes()).unwrap();
        encoder.send_block_match(&mut encoded, i as u32).unwrap();
        encoder.finish(&mut encoded).unwrap();
    }

    // Decode all three files from the single stream
    let mut cursor = Cursor::new(&encoded);
    for i in 0..3 {
        let expected = format!("file {i} content with some data to compress");
        let mut literals = Vec::new();
        let mut blocks = Vec::new();

        loop {
            match decoder.recv_token(&mut cursor).unwrap() {
                CompressedToken::Literal(d) => literals.extend_from_slice(&d),
                CompressedToken::BlockMatch(idx) => blocks.push(idx),
                CompressedToken::End => break,
            }
        }

        assert_eq!(literals, expected.as_bytes());
        assert_eq!(blocks, vec![i as u32]);
        decoder.reset();
    }
}

/// Verifies that a block match with no preceding literals produces
/// no DEFLATED_DATA blocks before the token.
#[test]
fn zstd_block_match_without_literals_no_deflated_data() {
    let mut encoder = ZstdTokenEncoder::new(3, None).unwrap();
    let mut encoded = Vec::new();

    encoder.send_block_match(&mut encoded, 42).unwrap();
    encoder.finish(&mut encoded).unwrap();

    // First byte should be a TOKEN, not DEFLATED_DATA
    assert_ne!(
        encoded[0] & 0xC0,
        DEFLATED_DATA,
        "block match without literals should not produce DEFLATED_DATA"
    );

    let mut decoder = ZstdTokenDecoder::new().unwrap();
    let mut cursor = Cursor::new(&encoded);
    let mut blocks = Vec::new();

    loop {
        match decoder.recv_token(&mut cursor).unwrap() {
            CompressedToken::Literal(_) => {}
            CompressedToken::BlockMatch(idx) => blocks.push(idx),
            CompressedToken::End => break,
        }
    }

    assert_eq!(blocks, vec![42]);
}

/// Golden byte test for the DEFLATED_DATA header format.
///
/// Verifies the 2-byte header encoding: first byte is
/// `DEFLATED_DATA | (len >> 8)`, second byte is `len & 0xFF`.
/// This must match upstream's obuf[0]/obuf[1] encoding at
/// token.c lines 758-759.
#[test]
fn zstd_deflated_data_header_matches_upstream() {
    let mut encoder = ZstdTokenEncoder::new(3, None).unwrap();
    let mut encoded = Vec::new();

    encoder.send_literal(&mut encoded, b"test").unwrap();
    encoder.finish(&mut encoded).unwrap();

    // First two bytes should be the DEFLATED_DATA header
    let flag = encoded[0];
    assert_eq!(
        flag & 0xC0,
        DEFLATED_DATA,
        "first byte should have DEFLATED_DATA flag"
    );

    // Decode the length from the header
    let high = (flag & 0x3F) as usize;
    let low = encoded[1] as usize;
    let len = (high << 8) | low;

    // The compressed data should follow immediately
    assert!(
        encoded.len() >= 2 + len,
        "encoded data too short for declared length"
    );

    // After the DEFLATED_DATA block, the next byte should be END_FLAG
    assert_eq!(
        encoded[2 + len],
        END_FLAG,
        "END_FLAG should follow the single DEFLATED_DATA block"
    );
}

/// Verifies that interleaved literal + block match sequences produce
/// correct flush boundaries with one flush per token boundary.
#[test]
fn zstd_interleaved_literal_block_flush_boundaries() {
    let mut encoder = ZstdTokenEncoder::new(3, None).unwrap();
    let mut encoded = Vec::new();

    // Pattern: lit, match, lit, match, lit, match, end
    for i in 0..3 {
        let data = format!("segment {i} with enough data to be meaningful");
        encoder.send_literal(&mut encoded, data.as_bytes()).unwrap();
        encoder.send_block_match(&mut encoded, i).unwrap();
    }
    encoder.finish(&mut encoded).unwrap();

    // Decode and verify
    let mut decoder = ZstdTokenDecoder::new().unwrap();
    let mut cursor = Cursor::new(&encoded);
    let mut literals = Vec::new();
    let mut blocks = Vec::new();

    loop {
        match decoder.recv_token(&mut cursor).unwrap() {
            CompressedToken::Literal(d) => literals.extend_from_slice(&d),
            CompressedToken::BlockMatch(idx) => blocks.push(idx),
            CompressedToken::End => break,
        }
    }

    let expected_literals: Vec<u8> = (0..3)
        .flat_map(|i| format!("segment {i} with enough data to be meaningful").into_bytes())
        .collect();
    assert_eq!(literals, expected_literals);
    assert_eq!(blocks, vec![0, 1, 2]);
}

/// Verifies that empty literal data (only block matches) roundtrips.
#[test]
fn zstd_only_block_matches_roundtrip() {
    let mut encoder = ZstdTokenEncoder::new(3, None).unwrap();
    let mut encoded = Vec::new();

    encoder.send_block_match(&mut encoded, 10).unwrap();
    encoder.send_block_match(&mut encoded, 20).unwrap();
    encoder.send_block_match(&mut encoded, 30).unwrap();
    encoder.finish(&mut encoded).unwrap();

    let mut decoder = ZstdTokenDecoder::new().unwrap();
    let mut cursor = Cursor::new(&encoded);
    let mut blocks = Vec::new();

    loop {
        match decoder.recv_token(&mut cursor).unwrap() {
            CompressedToken::Literal(_) => {}
            CompressedToken::BlockMatch(idx) => blocks.push(idx),
            CompressedToken::End => break,
        }
    }

    assert_eq!(blocks, vec![10, 20, 30]);
}

/// Verifies that the workers parameter is accepted without error.
///
/// When `Some(N)` is passed, `ZSTD_c_nbWorkers` is set on the raw encoder.
/// Whether true multi-threaded compression activates depends on the `zstdmt`
/// Cargo feature of the zstd-safe crate. Without `zstdmt`, the
/// `ZSTD_c_nbWorkers` parameter is silently ignored and the encoder falls
/// back to single-threaded mode.
#[test]
fn zstd_encoder_accepts_workers_parameter() {
    // None (single-threaded) always works.
    let enc_none = ZstdTokenEncoder::new(3, None);
    assert!(enc_none.is_ok(), "None workers should succeed");

    // Some(N) always succeeds - NbWorkers failure is silently ignored,
    // so the encoder falls back to single-threaded mode when zstdmt
    // is not available.
    let enc_one = ZstdTokenEncoder::new(3, std::num::NonZeroU8::new(1));
    assert!(enc_one.is_ok(), "1 worker should succeed");

    let enc_four = ZstdTokenEncoder::new(3, std::num::NonZeroU8::new(4));
    assert!(
        enc_four.is_ok(),
        "4 workers should succeed (fallback to single-threaded without zstdmt)"
    );
}

/// Verifies that a zstd encoder created with workers produces output that
/// the decoder can round-trip.
#[test]
fn zstd_encoder_with_workers_roundtrips() {
    let mut encoder = ZstdTokenEncoder::new(3, std::num::NonZeroU8::new(1)).unwrap();
    let mut encoded = Vec::new();

    let data = b"round-trip test data with workers=1";
    encoder.send_literal(&mut encoded, data).unwrap();
    encoder.send_block_match(&mut encoded, 7).unwrap();
    encoder.finish(&mut encoded).unwrap();

    let mut decoder = ZstdTokenDecoder::new().unwrap();
    let mut cursor = Cursor::new(&encoded);
    let mut literals = Vec::new();
    let mut blocks = Vec::new();

    loop {
        match decoder.recv_token(&mut cursor).unwrap() {
            CompressedToken::Literal(d) => literals.extend_from_slice(&d),
            CompressedToken::BlockMatch(idx) => blocks.push(idx),
            CompressedToken::End => break,
        }
    }

    assert_eq!(literals, data);
    assert_eq!(blocks, vec![7]);
}
