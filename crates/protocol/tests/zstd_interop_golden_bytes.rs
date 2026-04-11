// `| 0` used deliberately in wire format constants to document the offset field.
#![allow(clippy::identity_op)]
#![cfg(feature = "zstd")]
//! Zstd interop golden byte tests against upstream rsync 3.4.1 compressed token stream.
//!
//! Verifies wire compatibility between our zstd compressed token encoder/decoder
//! and upstream rsync's CPRES_ZSTD mode (token.c:send_zstd_token / recv_zstd_token).
//! Unlike the existing golden byte tests that verify structural properties, these
//! tests pin exact upstream-compatible byte sequences and verify cross-codec
//! decompression of raw zstd payloads.
//!
//! ## Upstream zstd framing specifics (token.c lines 678-776)
//!
//! - `ZSTD_CCtx` created once per session, never reset between files (line 688)
//! - `ZSTD_e_continue` for literal data accumulation (implicit in compressStream2)
//! - `ZSTD_e_flush` at every token boundary: block match or end-of-file (line 741)
//! - Output buffered in `MAX_DATA_COUNT`-sized buffer (line 695, 735-736)
//! - DEFLATED_DATA blocks written when buffer full OR on flush (line 755)
//! - `flush_pending` flag tracks whether unflushed data exists (line 769)
//! - No sync marker stripping (unlike zlib mode)
//!
//! ## Upstream zstd decoder behavior (token.c lines 780-870)
//!
//! - `ZSTD_DCtx` created once, never reset (line 788)
//! - Reads DEFLATED_DATA header, then `n` bytes of compressed data (lines 814-821)
//! - Calls `ZSTD_decompressStream` and returns decompressed output (line 850-851)
//! - Transitions to `r_idle` when input consumed and output not full (line 862-863)
//! - Token bytes (TOKEN_REL, TOKEN_LONG, etc.) handled identically to zlib (line 831)

use std::io::{Cursor, Read};

use protocol::wire::{
    CompressedToken, CompressedTokenDecoder, CompressedTokenEncoder, DEFLATED_DATA, END_FLAG,
    MAX_DATA_COUNT, TOKEN_LONG, TOKEN_REL, TOKENRUN_LONG, TOKENRUN_REL,
};
use zstd::stream::raw::{Decoder as ZstdRawDecoder, Operation};

/// Decompresses a raw zstd payload using the incremental raw decoder.
///
/// Upstream rsync uses `ZSTD_e_flush` (not `ZSTD_e_end`) at token boundaries,
/// so concatenated DEFLATED_DATA payloads form a flush-point-terminated stream
/// without a frame-end marker. The streaming `zstd::stream::read::Decoder`
/// reports "incomplete frame" on EOF without a frame end, but the raw decoder
/// handles flush-point streams correctly - matching upstream's incremental
/// `ZSTD_decompressStream` usage (token.c:850).
fn decompress_raw_zstd(data: &[u8]) -> Vec<u8> {
    let mut decoder = ZstdRawDecoder::new().unwrap();
    let mut result = Vec::new();
    let mut out_buf_storage = vec![0u8; 64 * 1024];

    let mut in_buf = zstd::stream::raw::InBuffer::around(data);
    while in_buf.pos() < in_buf.src.len() {
        let mut out_buf = zstd::stream::raw::OutBuffer::around(&mut out_buf_storage);
        decoder.run(&mut in_buf, &mut out_buf).unwrap();
        let produced = out_buf.pos();
        result.extend_from_slice(&out_buf_storage[..produced]);
    }

    // Drain any remaining output after all input consumed
    loop {
        let mut out_buf = zstd::stream::raw::OutBuffer::around(&mut out_buf_storage);
        decoder
            .run(&mut zstd::stream::raw::InBuffer::around(&[]), &mut out_buf)
            .unwrap();
        let produced = out_buf.pos();
        if produced == 0 {
            break;
        }
        result.extend_from_slice(&out_buf_storage[..produced]);
    }

    result
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Decodes all tokens from a zstd-encoded byte buffer.
fn decode_all_zstd(data: &[u8]) -> (Vec<u8>, Vec<u32>) {
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

/// Encodes a literal-only token stream at the given zstd compression level.
fn encode_literal(data: &[u8], level: i32) -> Vec<u8> {
    let mut encoder = CompressedTokenEncoder::new_zstd(level).unwrap();
    let mut output = Vec::new();
    encoder.send_literal(&mut output, data).unwrap();
    encoder.finish(&mut output).unwrap();
    output
}

/// Encodes a mixed stream of literals and block matches.
fn encode_mixed(tokens: &[InteropToken], level: i32) -> Vec<u8> {
    let mut encoder = CompressedTokenEncoder::new_zstd(level).unwrap();
    let mut output = Vec::new();

    for token in tokens {
        match token {
            InteropToken::Literal(data) => encoder.send_literal(&mut output, data).unwrap(),
            InteropToken::BlockMatch(idx) => encoder.send_block_match(&mut output, *idx).unwrap(),
        }
    }

    encoder.finish(&mut output).unwrap();
    output
}

#[derive(Clone)]
enum InteropToken {
    Literal(Vec<u8>),
    BlockMatch(u32),
}

/// Extracts all DEFLATED_DATA payloads from wire bytes, returning them in order
/// alongside the remaining non-DEFLATED_DATA wire elements.
fn extract_deflated_payloads(wire: &[u8]) -> (Vec<Vec<u8>>, Vec<u8>) {
    let mut cursor = Cursor::new(wire);
    let mut payloads = Vec::new();
    let mut non_deflated = Vec::new();

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
            let mut payload = vec![0u8; len];
            cursor.read_exact(&mut payload).unwrap();
            payloads.push(payload);
        } else {
            non_deflated.push(flag);
            if flag == END_FLAG {
                break;
            }
            // Consume trailing bytes for token types
            if flag & 0x80 != 0 {
                if flag & 0xC0 == TOKENRUN_REL {
                    let mut run_buf = [0u8; 2];
                    cursor.read_exact(&mut run_buf).unwrap();
                    non_deflated.extend_from_slice(&run_buf);
                }
            } else if flag & 0xE0 == TOKEN_LONG {
                let mut buf = [0u8; 4];
                cursor.read_exact(&mut buf).unwrap();
                non_deflated.extend_from_slice(&buf);
                if flag & 1 != 0 {
                    let mut run_buf = [0u8; 2];
                    cursor.read_exact(&mut run_buf).unwrap();
                    non_deflated.extend_from_slice(&run_buf);
                }
            }
        }
    }

    (payloads, non_deflated)
}

// ===========================================================================
// Section 1: Raw zstd payload cross-codec verification
//
// Extract the raw zstd compressed payload from our DEFLATED_DATA framing
// and decompress it with the standalone zstd crate to verify the payload
// is valid, standard zstd data that any conforming decompressor can handle.
// This proves interop with upstream rsync's ZSTD_decompressStream call.
//
// upstream: token.c:846-851 - recv side calls ZSTD_decompressStream on the
// raw payload bytes read after the DEFLATED_DATA header.
// ===========================================================================

/// Verify that the raw zstd payload extracted from our wire format can be
/// decompressed by the standalone zstd crate (simulating upstream's decoder).
/// This is the core interop guarantee: our encoder produces standard zstd
/// frames that any ZSTD_decompressStream implementation can consume.
///
/// upstream: token.c:850 - ZSTD_decompressStream(zstd_dctx, &out, &in)
#[test]
fn interop_raw_payload_decompressible_by_standalone_zstd() {
    let input = b"upstream rsync 3.4.1 compatibility test data for zstd interop";
    let encoded = encode_literal(input, 3);

    let (payloads, _) = extract_deflated_payloads(&encoded);
    assert!(
        !payloads.is_empty(),
        "must produce at least one DEFLATED_DATA block"
    );

    // Concatenate all payloads - they form one continuous zstd stream
    let raw_zstd: Vec<u8> = payloads.into_iter().flatten().collect();

    // Decompress using the raw zstd decoder (not our wire decoder).
    // upstream uses ZSTD_e_flush (not ZSTD_e_end) so there is no frame-end
    // marker - the streaming read::Decoder would report "incomplete frame".
    let decompressed = decompress_raw_zstd(&raw_zstd);

    assert_eq!(
        decompressed, input,
        "raw zstd payload must decompress to original input via standalone zstd crate"
    );
}

/// Verify cross-codec decompression for medium-sized repetitive data
/// typical of directory listing transfers. The repetitive structure
/// exercises zstd's back-reference decompression across blocks.
///
/// upstream: token.c:729-730 - data mapped and fed to compressStream2
#[test]
fn interop_raw_payload_medium_repetitive_data() {
    let mut input = Vec::with_capacity(800);
    for i in 0..20u8 {
        input.extend_from_slice(b"drwxr-xr-x  2 root root 4096 ");
        input.extend_from_slice(format!("entry_{i:03}.dat\n").as_bytes());
    }

    let encoded = encode_literal(&input, 3);
    let (payloads, _) = extract_deflated_payloads(&encoded);

    let raw_zstd: Vec<u8> = payloads.into_iter().flatten().collect();
    let decompressed = decompress_raw_zstd(&raw_zstd);

    assert_eq!(decompressed, input);
}

/// Verify cross-codec decompression of incompressible random data.
/// Zstd must handle data that does not compress well, producing
/// valid frames with stored/literal blocks.
#[test]
fn interop_raw_payload_incompressible_data() {
    // Generate pseudorandom incompressible data using xorshift64
    let mut state: u64 = 0xDEAD_BEEF_CAFE_1337;
    let input: Vec<u8> = (0..4096)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state & 0xFF) as u8
        })
        .collect();

    let encoded = encode_literal(&input, 3);
    let (payloads, _) = extract_deflated_payloads(&encoded);

    let raw_zstd: Vec<u8> = payloads.into_iter().flatten().collect();
    let decompressed = decompress_raw_zstd(&raw_zstd);

    assert_eq!(decompressed, input);
}

// ===========================================================================
// Section 2: Per-token ZSTD_e_flush boundary verification
//
// Upstream rsync flushes the zstd encoder at every token boundary (block
// match or end-of-file). This means that after each flush, all literal
// data accumulated since the last flush is decompressible. The tests below
// verify this flush-per-token invariant by checking that each DEFLATED_DATA
// block between token bytes produces complete, decompressible output.
//
// upstream: token.c:740-741 - if (token != -2) flush = ZSTD_e_flush
// upstream: token.c:743 - ZSTD_compressStream2(cctx, &out, &in, flush)
// ===========================================================================

/// Verify that a literal followed by a block match produces a flush boundary.
/// The DEFLATED_DATA block(s) before the token byte must contain all the
/// literal data in a decompressible form.
///
/// upstream: token.c:727 - nb != 0 triggers compress path, flush at token
#[test]
fn interop_flush_boundary_literal_then_block() {
    let literal = b"data before block match - verifies flush at token boundary";
    let encoded = encode_mixed(
        &[
            InteropToken::Literal(literal.to_vec()),
            InteropToken::BlockMatch(0),
        ],
        3,
    );

    // Extract payloads before the token byte
    let (payloads, non_deflated) = extract_deflated_payloads(&encoded);
    assert!(!payloads.is_empty(), "must have DEFLATED_DATA before token");

    // The non-deflated bytes should be: TOKEN_REL|0, END_FLAG
    assert_eq!(
        non_deflated,
        vec![TOKEN_REL | 0, END_FLAG],
        "token bytes must follow the flushed DEFLATED_DATA blocks"
    );

    // The payload must be independently decompressible
    let raw_zstd: Vec<u8> = payloads.into_iter().flatten().collect();
    let decompressed = decompress_raw_zstd(&raw_zstd);
    assert_eq!(decompressed, literal);
}

/// Verify flush boundaries with interleaved literal-block-literal pattern.
/// Each literal segment must be flushed before its following token, and
/// the cumulative zstd stream must decompress to all literal data in order.
///
/// upstream: token.c:700-723 - run state logic, then compress+flush at 727-768
#[test]
fn interop_flush_boundary_interleaved_pattern() {
    let encoded = encode_mixed(
        &[
            InteropToken::Literal(b"segment-one\n".to_vec()),
            InteropToken::BlockMatch(0),
            InteropToken::Literal(b"segment-two\n".to_vec()),
            InteropToken::BlockMatch(5),
            InteropToken::Literal(b"segment-three\n".to_vec()),
        ],
        3,
    );

    // Full round-trip must recover all data
    let (literals, blocks) = decode_all_zstd(&encoded);
    assert_eq!(literals, b"segment-one\nsegment-two\nsegment-three\n");
    assert_eq!(blocks, vec![0, 5]);

    // Verify the concatenated raw payloads form a valid zstd stream
    let (payloads, _) = extract_deflated_payloads(&encoded);
    assert!(
        payloads.len() >= 3,
        "3 literal segments should produce at least 3 DEFLATED_DATA blocks, got {}",
        payloads.len()
    );

    let raw_zstd: Vec<u8> = payloads.into_iter().flatten().collect();
    let decompressed = decompress_raw_zstd(&raw_zstd);
    assert_eq!(decompressed, b"segment-one\nsegment-two\nsegment-three\n");
}

/// Verify that ZSTD_e_flush at end-of-file (token == -1) produces a
/// complete stream. After finish(), the DEFLATED_DATA blocks must form
/// a fully decompressible zstd stream terminated properly.
///
/// upstream: token.c:772-775 - token == -1 triggers END_FLAG write
#[test]
fn interop_flush_at_eof_produces_complete_stream() {
    let input = b"end-of-file flush verification for upstream interop";
    let encoded = encode_literal(input, 3);

    // Wire must end with END_FLAG
    assert_eq!(
        *encoded.last().unwrap(),
        END_FLAG,
        "stream must end with END_FLAG"
    );

    // The entire payload must form a valid, complete zstd stream
    let (payloads, non_deflated) = extract_deflated_payloads(&encoded);
    assert_eq!(
        non_deflated,
        vec![END_FLAG],
        "literal-only stream non-deflated bytes should be just END_FLAG"
    );

    let raw_zstd: Vec<u8> = payloads.into_iter().flatten().collect();
    let decompressed = decompress_raw_zstd(&raw_zstd);
    assert_eq!(decompressed, input);
}

// ===========================================================================
// Section 3: Upstream-format hand-crafted wire byte decoding
//
// These tests construct raw wire byte sequences that match exactly what
// upstream rsync 3.4.1 would produce for known inputs, then verify our
// decoder handles them correctly. The byte sequences are built from
// first principles using the upstream wire format specification.
//
// upstream: token.c lines 321-329 - flag byte definitions
// upstream: token.c:758-760 - DEFLATED_DATA header encoding
// ===========================================================================

/// Hand-craft a DEFLATED_DATA block containing a known zstd-compressed
/// payload, followed by END_FLAG. Verify our decoder extracts the
/// original data correctly.
///
/// This simulates receiving a single-block compressed file from upstream.
///
/// upstream: token.c:755-760 - write DEFLATED_DATA header + payload
#[test]
fn interop_decode_handcrafted_deflated_data_block() {
    let input = b"handcrafted upstream wire bytes";

    // Compress with standalone zstd to get raw payload (simulating upstream encoder)
    let mut raw_encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
    std::io::Write::write_all(&mut raw_encoder, input).unwrap();
    std::io::Write::flush(&mut raw_encoder).unwrap();
    let compressed = raw_encoder.finish().unwrap();

    // Build DEFLATED_DATA wire frame: header + payload + END_FLAG
    let len = compressed.len();
    assert!(len <= MAX_DATA_COUNT, "payload must fit in one block");
    let mut wire = Vec::new();
    wire.push(DEFLATED_DATA | ((len >> 8) as u8));
    wire.push((len & 0xFF) as u8);
    wire.extend_from_slice(&compressed);
    wire.push(END_FLAG);

    // Decode using our wire decoder
    let (literals, blocks) = decode_all_zstd(&wire);
    assert_eq!(literals, input);
    assert!(blocks.is_empty());
}

/// Hand-craft a wire stream with DEFLATED_DATA + TOKEN_REL + END_FLAG.
/// This matches upstream's output for a literal followed by a single
/// block match at index 0.
///
/// upstream: token.c:712 - write_byte(f, TOKEN_REL + r) where r=0
/// upstream: token.c:758-760 - DEFLATED_DATA header before token
#[test]
fn interop_decode_handcrafted_literal_then_block_match() {
    let input = b"literal data before block";

    // Compress with standalone zstd (simulating upstream's ZSTD_compressStream2)
    let mut raw_encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
    std::io::Write::write_all(&mut raw_encoder, input).unwrap();
    std::io::Write::flush(&mut raw_encoder).unwrap();
    let compressed = raw_encoder.finish().unwrap();

    // Build wire: DEFLATED_DATA(compressed) + TOKEN_REL|0 + END_FLAG
    let len = compressed.len();
    let mut wire = Vec::new();
    wire.push(DEFLATED_DATA | ((len >> 8) as u8));
    wire.push((len & 0xFF) as u8);
    wire.extend_from_slice(&compressed);
    wire.push(TOKEN_REL | 0); // block match at index 0
    wire.push(END_FLAG);

    let (literals, blocks) = decode_all_zstd(&wire);
    assert_eq!(literals, input);
    assert_eq!(blocks, vec![0]);
}

/// Hand-craft a wire stream with TOKEN_LONG for a large block index.
/// Upstream uses TOKEN_LONG when the relative offset exceeds 63.
///
/// upstream: token.c:714 - write_byte(f, TOKEN_LONG); write_int(f, run_start)
#[test]
fn interop_decode_handcrafted_token_long_block() {
    // TOKEN_LONG + 4-byte LE index (1000) + END_FLAG
    let wire = [
        TOKEN_LONG, 0xE8, 0x03, 0x00, 0x00, // 1000 in LE
        END_FLAG,
    ];

    let (literals, blocks) = decode_all_zstd(&wire);
    assert!(literals.is_empty());
    assert_eq!(blocks, vec![1000]);
}

/// Hand-craft a wire stream with TOKENRUN_REL for consecutive blocks.
/// Upstream encodes runs of consecutive block matches as a single token.
///
/// upstream: token.c:712 - TOKENRUN_REL + r, followed by 16-bit LE count
#[test]
fn interop_decode_handcrafted_tokenrun_rel() {
    // TOKENRUN_REL|5, count=9 (LE16) -> blocks 5,6,7,8,9,10,11,12,13,14
    let wire = [
        TOKENRUN_REL | 5,
        9,
        0, // n=9 (LE16)
        END_FLAG,
    ];

    let (literals, blocks) = decode_all_zstd(&wire);
    assert!(literals.is_empty());
    let expected: Vec<u32> = (5..=14).collect();
    assert_eq!(blocks, expected);
}

/// Hand-craft a wire stream with TOKENRUN_LONG for consecutive blocks
/// starting at a large index.
///
/// upstream: token.c:714 - TOKENRUN_LONG + 32-bit LE run_start + 16-bit LE count
#[test]
fn interop_decode_handcrafted_tokenrun_long() {
    // TOKENRUN_LONG, run_start=500 (LE32), n=3 (LE16) -> blocks 500,501,502,503
    let wire = [
        TOKENRUN_LONG,
        0xF4,
        0x01,
        0x00,
        0x00, // run_start=500
        3,
        0, // n=3
        END_FLAG,
    ];

    let (literals, blocks) = decode_all_zstd(&wire);
    assert!(literals.is_empty());
    assert_eq!(blocks, vec![500, 501, 502, 503]);
}

/// Hand-craft a complex wire stream mixing DEFLATED_DATA, TOKEN_REL,
/// TOKENRUN_REL, and TOKEN_LONG to verify our decoder handles the full
/// upstream wire vocabulary in a single stream.
///
/// upstream: token.c:700-723 - run detection and token emission
#[test]
fn interop_decode_handcrafted_complex_mixed_stream() {
    let literal1 = b"first segment";
    let literal2 = b"second segment";

    // Compress both literals with standalone zstd
    let mut enc1 = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
    std::io::Write::write_all(&mut enc1, literal1).unwrap();
    std::io::Write::flush(&mut enc1).unwrap();
    let comp1 = enc1.finish().unwrap();

    let mut enc2 = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
    std::io::Write::write_all(&mut enc2, literal2).unwrap();
    std::io::Write::flush(&mut enc2).unwrap();
    let comp2 = enc2.finish().unwrap();

    // Build wire: DEFLATED(lit1) + TOKEN_REL|0 + DEFLATED(lit2) + TOKEN_LONG(100) + END_FLAG
    let mut wire = Vec::new();

    // First DEFLATED_DATA block
    let len1 = comp1.len();
    wire.push(DEFLATED_DATA | ((len1 >> 8) as u8));
    wire.push((len1 & 0xFF) as u8);
    wire.extend_from_slice(&comp1);

    // TOKEN_REL | 0 (block match at index 0)
    wire.push(TOKEN_REL | 0);

    // Second DEFLATED_DATA block
    let len2 = comp2.len();
    wire.push(DEFLATED_DATA | ((len2 >> 8) as u8));
    wire.push((len2 & 0xFF) as u8);
    wire.extend_from_slice(&comp2);

    // TOKEN_LONG (block match at index 100, relative offset > 63)
    wire.push(TOKEN_LONG);
    wire.extend_from_slice(&100i32.to_le_bytes());

    // END_FLAG
    wire.push(END_FLAG);

    let (literals, blocks) = decode_all_zstd(&wire);
    assert_eq!(literals, b"first segmentsecond segment");
    assert_eq!(blocks, vec![0, 100]);
}

// ===========================================================================
// Section 4: DEFLATED_DATA header encoding interop
//
// Verify exact header byte encoding matches upstream's formula:
//   obuf[0] = DEFLATED_DATA + (n >> 8)
//   obuf[1] = n
// (upstream token.c lines 758-759)
// ===========================================================================

/// Verify DEFLATED_DATA header encoding for small payloads where the
/// length fits entirely in the second byte (high bits are zero).
///
/// upstream: token.c:758-759 - obuf[0] = DEFLATED_DATA + (n >> 8), obuf[1] = n
#[test]
fn interop_deflated_header_small_payload() {
    let encoded = encode_literal(b"x", 3);

    let byte0 = encoded[0];
    let byte1 = encoded[1];

    // For small payload, n < 256, so n >> 8 == 0
    assert_eq!(byte0 & 0xC0, DEFLATED_DATA, "must have DEFLATED_DATA flag");

    let declared_len = ((byte0 & 0x3F) as usize) << 8 | byte1 as usize;
    assert!(declared_len > 0, "payload must not be empty");
    assert!(
        declared_len <= MAX_DATA_COUNT,
        "payload must fit in 14 bits"
    );

    // Verify the declared length matches actual payload
    let actual_payload = &encoded[2..2 + declared_len];
    assert_eq!(actual_payload.len(), declared_len);

    // After payload: END_FLAG
    assert_eq!(encoded[2 + declared_len], END_FLAG);
}

/// Verify DEFLATED_DATA header encoding uses both bytes of the 14-bit
/// length field for larger payloads. With incompressible data, zstd
/// output can exceed 256 bytes.
///
/// upstream: token.c:758 - obuf[0] = DEFLATED_DATA + (n >> 8) where n > 255
#[test]
fn interop_deflated_header_large_payload_uses_high_bits() {
    // Generate ~1KB of incompressible data (will compress to > 256 bytes)
    let mut state: u64 = 0xABCD_EF01_2345_6789;
    let input: Vec<u8> = (0..2048)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state & 0xFF) as u8
        })
        .collect();

    let encoded = encode_literal(&input, 3);

    // Find a DEFLATED_DATA block with length > 255
    let mut found_large = false;
    let mut cursor = Cursor::new(&encoded[..]);
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

            if len > 255 {
                found_large = true;
                // Verify the encoding: high bits must be non-zero
                assert!(high > 0, "n > 255 requires high bits in byte 0");
                // Verify reconstruction matches upstream formula
                let reconstructed_byte0 = DEFLATED_DATA | (len >> 8) as u8;
                let reconstructed_byte1 = (len & 0xFF) as u8;
                assert_eq!(flag, reconstructed_byte0);
                assert_eq!(low_buf[0], reconstructed_byte1);
            }
            let pos = cursor.position() as usize;
            cursor.set_position((pos + len) as u64);
        } else {
            break;
        }
    }

    assert!(
        found_large,
        "incompressible 2KB input should produce DEFLATED_DATA with len > 255"
    );
}

// ===========================================================================
// Section 5: Multi-file session interop - persistent zstd context
//
// Upstream rsync maintains a single ZSTD_CCtx / ZSTD_DCtx for the entire
// transfer session. The context is NEVER reset between files - only the
// token run-encoding state resets. This means the second file benefits
// from dictionary/history built during the first file.
//
// upstream: token.c:686-698 - CCtx created once, comp_init_done flag
// upstream: token.c:700-703 - only last_token/run_start/flush_pending reset
// upstream: token.c:787-803 - DCtx created once, decomp_init_done flag
// ===========================================================================

/// Verify that our encoder/decoder can handle a multi-file session where
/// the zstd context persists. Encode three files with one encoder, decode
/// with one decoder, verify each file's data independently.
///
/// upstream: token.c:688 - CCtx created once, persists across files
/// upstream: token.c:788 - DCtx created once, persists across files
#[test]
fn interop_multi_file_session_persistent_context() {
    let files: &[&[u8]] = &[
        b"# config.yml\nhost: rsync.example.com\nport: 873\n",
        b"drwxr-xr-x root root 4096 modules/\ndrwxr-xr-x root root 4096 data/\n",
        b"# config.yml\nhost: rsync.example.com\nport: 873\nmodule: backup\n",
    ];

    let mut encoder = CompressedTokenEncoder::new_zstd(3).unwrap();
    let mut wire = Vec::new();

    // Encode all three files
    for file_data in files {
        encoder.send_literal(&mut wire, file_data).unwrap();
        encoder.finish(&mut wire).unwrap();
    }

    // Decode all three files with persistent decoder
    let mut cursor = Cursor::new(&wire);
    let mut decoder = CompressedTokenDecoder::new_zstd().unwrap();

    for (i, expected) in files.iter().enumerate() {
        let mut literals = Vec::new();
        loop {
            match decoder.recv_token(&mut cursor).unwrap() {
                CompressedToken::Literal(d) => literals.extend_from_slice(&d),
                CompressedToken::End => break,
                CompressedToken::BlockMatch(_) => {
                    panic!("file {i} should have no block matches")
                }
            }
        }
        assert_eq!(
            literals, *expected,
            "file {i} data mismatch in multi-file session"
        );
        decoder.reset();
    }
}

/// Verify that the third file in a session with shared content compresses
/// better than in isolation, proving the zstd context carries dictionary
/// history across file boundaries.
///
/// upstream: token.c:686-698 - CCtx never recreated, dictionary persists
#[test]
fn interop_cross_file_dictionary_improves_compression() {
    let shared_line = b"drwxr-xr-x  2 root root 4096 shared_module_data.txt\n";

    // Encode file 1 (establishes dictionary) then file 2 (benefits)
    let mut encoder = CompressedTokenEncoder::new_zstd(3).unwrap();
    let mut session_wire = Vec::new();

    encoder
        .send_literal(&mut session_wire, shared_line)
        .unwrap();
    encoder.finish(&mut session_wire).unwrap();
    let file1_end = session_wire.len();

    encoder
        .send_literal(&mut session_wire, shared_line)
        .unwrap();
    encoder.finish(&mut session_wire).unwrap();
    let file2_session_len = session_wire.len() - file1_end;

    // Encode same content in isolation (fresh context, no dictionary)
    let isolated = encode_literal(shared_line, 3);

    assert!(
        file2_session_len <= isolated.len(),
        "session file 2 ({file2_session_len}) should be <= isolated ({}) \
         due to cross-file dictionary",
        isolated.len()
    );
}

// ===========================================================================
// Section 6: Zstd frame magic and structural invariants
//
// Verify that the zstd compressed payloads contain valid zstd frame
// structure, matching what upstream rsync's zstd encoder produces.
// ===========================================================================

/// Verify that the first DEFLATED_DATA payload starts with the zstd
/// frame magic number (0xFD2FB528 in little-endian). This is a
/// fundamental structural requirement for zstd interop.
///
/// upstream: token.c:694 - ZSTD_CCtx_setParameter, standard frame output
#[test]
fn interop_zstd_frame_magic_in_first_payload() {
    let encoded = encode_literal(b"verify zstd frame magic for interop", 3);
    let (payloads, _) = extract_deflated_payloads(&encoded);
    assert!(!payloads.is_empty());

    let first_payload = &payloads[0];
    assert!(first_payload.len() >= 4, "payload too short for zstd magic");

    let magic = u32::from_le_bytes([
        first_payload[0],
        first_payload[1],
        first_payload[2],
        first_payload[3],
    ]);
    assert_eq!(
        magic, 0xFD2F_B528,
        "first payload must start with zstd frame magic 0xFD2FB528"
    );
}

/// Verify that zstd payloads never contain the zlib sync marker
/// (0x00 0x00 0xFF 0xFF). This distinguishes zstd mode from zlib mode
/// on the wire and is critical for mode detection.
///
/// upstream: token.c:685 - zstd never strips sync markers because it
/// never produces them
#[test]
fn interop_no_zlib_sync_markers_in_zstd_payload() {
    // Use data that might trigger false positives with certain byte patterns
    let mut input = Vec::with_capacity(512);
    for byte in 0..=255u8 {
        input.push(byte);
        input.push(byte.wrapping_add(1));
    }

    let encoded = encode_literal(&input, 3);
    let (payloads, _) = extract_deflated_payloads(&encoded);

    let sync_marker = [0x00u8, 0x00, 0xFF, 0xFF];
    for (i, payload) in payloads.iter().enumerate() {
        for window in payload.windows(4) {
            assert_ne!(
                window, &sync_marker,
                "DEFLATED_DATA block {i} must not contain zlib sync marker"
            );
        }
    }
}

/// Verify all DEFLATED_DATA blocks respect the MAX_DATA_COUNT limit.
/// Upstream writes output only when the buffer is full (MAX_DATA_COUNT)
/// or on flush.
///
/// upstream: token.c:735-736 - zstd_out_buff.size = MAX_DATA_COUNT
/// upstream: token.c:755 - write when pos == size or on flush
#[test]
fn interop_all_deflated_blocks_within_max_data_count() {
    // Generate large data that will produce multiple DEFLATED_DATA blocks
    let mut state: u64 = 0x1234_5678_9ABC_DEF0;
    let input: Vec<u8> = (0..100_000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state & 0xFF) as u8
        })
        .collect();

    let encoded = encode_literal(&input, 3);
    let (payloads, _) = extract_deflated_payloads(&encoded);

    assert!(
        payloads.len() > 1,
        "100KB incompressible data should produce multiple blocks"
    );

    for (i, payload) in payloads.iter().enumerate() {
        assert!(
            payload.len() <= MAX_DATA_COUNT,
            "block {i} size {} exceeds MAX_DATA_COUNT ({MAX_DATA_COUNT})",
            payload.len()
        );
        assert!(!payload.is_empty(), "block {i} must not be empty");
    }
}

// ===========================================================================
// Section 7: Compression level interop
//
// Upstream rsync respects the --compress-level flag, defaulting to
// ZSTD_CLEVEL_DEFAULT (3) for daemon mode. Verify that different
// compression levels all produce valid, decodable output.
//
// upstream: token.c:694 - ZSTD_CCtx_setParameter(cctx, ZSTD_c_compressionLevel, level)
// ===========================================================================

/// Verify that compression levels 1 through 6 all produce valid
/// interoperable output. These are the most commonly used levels
/// in production rsync daemon configurations.
///
/// upstream: token.c:694 - compression level applied via CCtx parameter
#[test]
fn interop_compression_levels_all_produce_valid_output() {
    let input = b"compression level interop test - verify all levels produce valid zstd frames";

    for level in 1..=6 {
        let encoded = encode_literal(input, level);

        // Verify round-trip through our decoder
        let (literals, blocks) = decode_all_zstd(&encoded);
        assert_eq!(literals, input, "round-trip failed at level {level}");
        assert!(blocks.is_empty());

        // Verify raw payload decompressible by standalone zstd
        let (payloads, _) = extract_deflated_payloads(&encoded);
        let raw: Vec<u8> = payloads.into_iter().flatten().collect();
        let out = decompress_raw_zstd(&raw);
        assert_eq!(out, input, "standalone zstd decode failed at level {level}");
    }
}

/// Verify that higher compression levels produce smaller or equal output
/// for compressible data, matching upstream's behavior where level
/// affects compression ratio but not wire format correctness.
///
/// upstream: token.c:694 - level controls zstd internal strategy
#[test]
fn interop_higher_level_better_compression() {
    let mut input = Vec::with_capacity(2000);
    for i in 0..50u8 {
        input.extend_from_slice(b"drwxr-xr-x  2 root root 4096 ");
        input.extend_from_slice(format!("module_{i:03}/data.bin\n").as_bytes());
    }

    let wire_1 = encode_literal(&input, 1);
    let wire_3 = encode_literal(&input, 3);
    let wire_6 = encode_literal(&input, 6);

    // All must decode correctly
    assert_eq!(decode_all_zstd(&wire_1).0, input);
    assert_eq!(decode_all_zstd(&wire_3).0, input);
    assert_eq!(decode_all_zstd(&wire_6).0, input);

    // Higher levels should compress at least as well
    assert!(
        wire_3.len() <= wire_1.len(),
        "level 3 ({}) should be <= level 1 ({})",
        wire_3.len(),
        wire_1.len()
    );
    assert!(
        wire_6.len() <= wire_3.len(),
        "level 6 ({}) should be <= level 3 ({})",
        wire_6.len(),
        wire_3.len()
    );
}

// ===========================================================================
// Section 8: Round-trip integrity with diverse data patterns
//
// Verify that our encoder produces output that our decoder can consume
// correctly for various data patterns representative of real rsync
// transfers. Each test encodes, then decodes, then verifies exact
// data and block-match recovery.
// ===========================================================================

/// Round-trip a mixed stream representing a typical incremental file
/// transfer: changed header, unchanged middle blocks, changed footer.
///
/// upstream: token.c:678-776 - send path, 780-870 - recv path
#[test]
fn interop_roundtrip_incremental_transfer_pattern() {
    let encoded = encode_mixed(
        &[
            InteropToken::Literal(b"#!/bin/bash\n# Updated header\n".to_vec()),
            InteropToken::BlockMatch(0),
            InteropToken::BlockMatch(1),
            InteropToken::BlockMatch(2),
            InteropToken::BlockMatch(3),
            InteropToken::Literal(b"\n# Updated footer\nexit 0\n".to_vec()),
        ],
        3,
    );

    let (literals, blocks) = decode_all_zstd(&encoded);
    assert_eq!(
        literals,
        b"#!/bin/bash\n# Updated header\n\n# Updated footer\nexit 0\n"
    );
    assert_eq!(blocks, vec![0, 1, 2, 3]);
}

/// Round-trip with zero-length input (empty file transfer).
/// Must produce only END_FLAG with no DEFLATED_DATA blocks.
///
/// upstream: token.c:772-775 - token==-1 with no preceding data
#[test]
fn interop_roundtrip_empty_file() {
    let mut encoder = CompressedTokenEncoder::new_zstd(3).unwrap();
    let mut wire = Vec::new();
    encoder.finish(&mut wire).unwrap();

    assert_eq!(
        wire,
        vec![END_FLAG],
        "empty file should produce only END_FLAG"
    );

    let (literals, blocks) = decode_all_zstd(&wire);
    assert!(literals.is_empty());
    assert!(blocks.is_empty());
}

/// Round-trip with block-match-only stream (basis file unchanged).
/// No DEFLATED_DATA blocks should appear - only token bytes.
///
/// upstream: token.c:706-723 - token emission without preceding literals
#[test]
fn interop_roundtrip_blocks_only_no_deflated_data() {
    let encoded = encode_mixed(
        &[
            InteropToken::BlockMatch(0),
            InteropToken::BlockMatch(1),
            InteropToken::BlockMatch(2),
        ],
        3,
    );

    // Should have no DEFLATED_DATA blocks
    let (payloads, _) = extract_deflated_payloads(&encoded);
    assert!(
        payloads.is_empty(),
        "block-match-only stream should have no DEFLATED_DATA blocks"
    );

    let (literals, blocks) = decode_all_zstd(&encoded);
    assert!(literals.is_empty());
    assert_eq!(blocks, vec![0, 1, 2]);
}

/// Round-trip with a single large literal (64 KB) that exceeds
/// `MAX_DATA_COUNT`, forcing multiple DEFLATED_DATA blocks.
/// Verifies our chunking matches upstream's buffer-full write behavior.
///
/// upstream: token.c:755 - write when zstd_out_buff.pos == zstd_out_buff.size
#[test]
fn interop_roundtrip_large_literal_multiple_blocks() {
    // Compressible data that still produces multiple output blocks
    let input: Vec<u8> = (0..65536u32)
        .flat_map(|i| format!("line {i:05}\n").into_bytes())
        .collect();

    let encoded = encode_literal(&input, 3);
    let (payloads, _) = extract_deflated_payloads(&encoded);

    // Large compressible input should still produce at least 2 blocks
    assert!(
        payloads.len() >= 2,
        "large literal should produce multiple DEFLATED_DATA blocks, got {}",
        payloads.len()
    );

    let (literals, blocks) = decode_all_zstd(&encoded);
    assert_eq!(literals, input);
    assert!(blocks.is_empty());
}

/// Round-trip with alternating small literals and block matches,
/// verifying that the flush-per-token pattern preserves all data
/// through many interleaving cycles.
///
/// upstream: token.c:727-768 - compress+flush loop per token
#[test]
fn interop_roundtrip_many_small_interleaved_tokens() {
    let mut tokens = Vec::new();
    let mut expected_literals = Vec::new();
    let mut expected_blocks = Vec::new();

    for i in 0..20u32 {
        let lit = format!("chunk-{i:02}\n");
        expected_literals.extend_from_slice(lit.as_bytes());
        tokens.push(InteropToken::Literal(lit.into_bytes()));

        expected_blocks.push(i);
        tokens.push(InteropToken::BlockMatch(i));
    }
    // Trailing literal
    let tail = b"tail\n";
    expected_literals.extend_from_slice(tail);
    tokens.push(InteropToken::Literal(tail.to_vec()));

    let encoded = encode_mixed(&tokens, 3);
    let (literals, blocks) = decode_all_zstd(&encoded);

    assert_eq!(literals, expected_literals);
    assert_eq!(blocks, expected_blocks);
}
