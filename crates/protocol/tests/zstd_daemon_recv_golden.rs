// `| 0` used deliberately in wire format constants to document the offset field.
#![allow(clippy::identity_op)]
#![cfg(feature = "zstd")]
//! Golden byte test for zstd daemon-mode receive codec.
//!
//! Verifies that the zstd compressed token encoder produces deterministic,
//! wire-compatible output for known inputs at daemon default compression
//! level (3). The tests pin exact wire bytes for the framing layer and verify
//! structural invariants of the compressed payload.
//!
//! This test specifically covers the daemon-mode receive path where the
//! receiver decompresses zstd-compressed token data sent by the daemon sender.
//! In daemon mode, compression level defaults to 3 (ZSTD_CLEVEL_DEFAULT) and
//! the stream uses ZSTD_e_flush at every token boundary.
//!
//! ## Approach
//!
//! 1. **Determinism**: Encoding the same input twice must produce identical
//!    wire bytes. This ensures wire stability across builds.
//! 2. **Framing verification**: The DEFLATED_DATA header, token bytes, and
//!    END_FLAG are verified byte-by-byte against the upstream wire format.
//! 3. **Payload structure**: The zstd frame magic (0xFD2FB528) and frame
//!    header are verified in the compressed payload.
//! 4. **Cross-decode**: Encoded output is decoded back and verified against
//!    the original input to prove the daemon receive path is correct.
//! 5. **Multi-file session**: Verifies the persistent zstd context across
//!    file boundaries in a daemon transfer session.
//!
//! upstream: token.c:send_zstd_token() lines 678-776, recv_zstd_token() 780-870

use std::io::Cursor;

use protocol::wire::{
    CompressedToken, CompressedTokenDecoder, CompressedTokenEncoder, DEFLATED_DATA, END_FLAG,
    MAX_DATA_COUNT, TOKEN_REL,
};

// ---------------------------------------------------------------------------
// Test input data - deterministic, representative of daemon transfer content
// ---------------------------------------------------------------------------

/// Small file content typical of a daemon-mode config file transfer.
/// 59 bytes of printable ASCII - small enough for a single DEFLATED_DATA block.
const SMALL_INPUT: &[u8] = b"# oc-rsyncd.conf\npath = /data/modules/test\nread only = true\n";

/// Medium input with repetitive structure typical of directory listings.
/// This exercises zstd's dictionary/history across the compressed output.
fn medium_input() -> Vec<u8> {
    let mut data = Vec::with_capacity(720);
    for i in 0..16u8 {
        // Repeating structure lets zstd exploit cross-reference compression
        data.extend_from_slice(b"drwxr-xr-x  2 root root 4096 ");
        data.extend_from_slice(format!("file_{i:03}.dat\n").as_bytes());
    }
    data
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Encodes a literal-only stream using zstd at daemon default level (3).
fn encode_literal_only(input: &[u8]) -> Vec<u8> {
    let mut encoder = CompressedTokenEncoder::new_zstd(3).unwrap();
    let mut output = Vec::new();
    encoder.send_literal(&mut output, input).unwrap();
    encoder.finish(&mut output).unwrap();
    output
}

/// Encodes a mixed literal + block-match stream at daemon default level (3).
fn encode_mixed(literals: &[&[u8]], block_indices: &[u32]) -> Vec<u8> {
    let mut encoder = CompressedTokenEncoder::new_zstd(3).unwrap();
    let mut output = Vec::new();

    // Interleave: literal, block, literal, block, ...
    let max_len = literals.len().max(block_indices.len());
    for i in 0..max_len {
        if i < literals.len() {
            encoder.send_literal(&mut output, literals[i]).unwrap();
        }
        if i < block_indices.len() {
            encoder
                .send_block_match(&mut output, block_indices[i])
                .unwrap();
        }
    }

    encoder.finish(&mut output).unwrap();
    output
}

/// Decodes all tokens from a zstd-encoded byte buffer.
fn decode_all(data: &[u8]) -> (Vec<u8>, Vec<u32>) {
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

/// Extracts the compressed payload from a wire buffer's first DEFLATED_DATA block.
/// Returns (header_bytes, payload, rest_of_buffer).
fn extract_first_deflated_block(wire: &[u8]) -> (u8, u8, &[u8], &[u8]) {
    assert!(
        wire.len() >= 3,
        "wire buffer too short for DEFLATED_DATA header"
    );
    let byte0 = wire[0];
    let byte1 = wire[1];
    assert_eq!(
        byte0 & 0xC0,
        DEFLATED_DATA,
        "first byte must have DEFLATED_DATA flag"
    );
    let high = (byte0 & 0x3F) as usize;
    let low = byte1 as usize;
    let len = (high << 8) | low;
    let payload = &wire[2..2 + len];
    let rest = &wire[2 + len..];
    (byte0, byte1, payload, rest)
}

// ===========================================================================
// Section 1: Encoder determinism - same input always produces same wire bytes
// ===========================================================================

/// Encoding the same small input twice must produce byte-identical output.
/// This is the foundation of golden byte testing - if the encoder is not
/// deterministic, frozen references would be meaningless.
///
/// upstream: zstd guarantees deterministic output for same input/level/context
#[test]
fn golden_zstd_daemon_recv_deterministic_small() {
    let encoded1 = encode_literal_only(SMALL_INPUT);
    let encoded2 = encode_literal_only(SMALL_INPUT);
    assert_eq!(
        encoded1, encoded2,
        "zstd encoder must produce identical bytes for identical input"
    );
}

/// Encoding the same medium input twice must produce identical output.
#[test]
fn golden_zstd_daemon_recv_deterministic_medium() {
    let input = medium_input();
    let encoded1 = encode_literal_only(&input);
    let encoded2 = encode_literal_only(&input);
    assert_eq!(
        encoded1, encoded2,
        "zstd encoder must be deterministic for medium input"
    );
}

/// Mixed stream encoding must be deterministic.
#[test]
fn golden_zstd_daemon_recv_deterministic_mixed() {
    let encoded1 = encode_mixed(&[b"daemon push data\n"], &[0]);
    let encoded2 = encode_mixed(&[b"daemon push data\n"], &[0]);
    assert_eq!(
        encoded1, encoded2,
        "zstd encoder must be deterministic for mixed streams"
    );
}

// ===========================================================================
// Section 2: DEFLATED_DATA framing - exact header byte verification
// ===========================================================================

/// Verifies the DEFLATED_DATA header bytes encode the payload length correctly.
/// The 14-bit length field must match the actual compressed payload size.
///
/// upstream: token.c lines 758-759 - obuf[0] = DEFLATED_DATA + (n>>8), obuf[1] = n
#[test]
fn golden_zstd_daemon_recv_deflated_header_encodes_length() {
    let encoded = encode_literal_only(SMALL_INPUT);

    let (byte0, byte1, payload, rest) = extract_first_deflated_block(&encoded);

    // Verify length encoding
    let declared_len = ((byte0 as usize & 0x3F) << 8) | (byte1 as usize);
    assert_eq!(
        declared_len,
        payload.len(),
        "DEFLATED_DATA header length must match actual payload size"
    );

    // Rest should be just END_FLAG for literal-only stream
    assert_eq!(
        rest,
        &[END_FLAG],
        "literal-only stream must end with END_FLAG"
    );

    // Payload must be valid zstd data (starts with magic 0xFD2FB528 LE)
    assert!(
        payload.len() >= 4,
        "zstd payload must be at least 4 bytes (magic number)"
    );
    let magic = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
    assert_eq!(
        magic, 0xFD2F_B528,
        "zstd payload must start with frame magic 0xFD2FB528"
    );
}

/// Verifies that small daemon-mode input fits in a single DEFLATED_DATA block
/// within MAX_DATA_COUNT.
///
/// upstream: token.c line 755 - write only when buffer full or on flush
#[test]
fn golden_zstd_daemon_recv_small_single_block() {
    let encoded = encode_literal_only(SMALL_INPUT);

    let (_, _, payload, rest) = extract_first_deflated_block(&encoded);

    assert!(
        payload.len() <= MAX_DATA_COUNT,
        "small input payload {} exceeds MAX_DATA_COUNT {}",
        payload.len(),
        MAX_DATA_COUNT
    );
    assert_eq!(rest, &[END_FLAG]);
}

// ===========================================================================
// Section 3: Mixed stream wire format - exact token byte verification
// ===========================================================================

/// Verifies the exact wire format of a mixed literal + single block match.
/// After the DEFLATED_DATA block(s), the next byte must be TOKEN_REL | 0,
/// followed by END_FLAG.
///
/// upstream: token.c lines 700-723 - flush, then write token byte
#[test]
fn golden_zstd_daemon_recv_mixed_wire_layout() {
    let encoded = encode_mixed(&[b"daemon push data\n"], &[0]);

    let (_, _, _, rest) = extract_first_deflated_block(&encoded);

    // After the compressed payload: TOKEN_REL|0, END_FLAG
    assert_eq!(
        rest,
        &[TOKEN_REL | 0, END_FLAG],
        "mixed stream must have TOKEN_REL|0 then END_FLAG after DEFLATED_DATA"
    );
}

/// Verifies mixed stream with block match at higher index uses correct encoding.
/// Block 42 (within relative range 0-63) uses TOKEN_REL | 42.
#[test]
fn golden_zstd_daemon_recv_mixed_block_42() {
    let encoded = encode_mixed(&[b"data before block\n"], &[42]);

    let (_, _, _, rest) = extract_first_deflated_block(&encoded);

    assert_eq!(
        rest,
        &[TOKEN_REL | 42, END_FLAG],
        "block match 42 must encode as TOKEN_REL|42"
    );
}

// ===========================================================================
// Section 4: Zstd frame structure verification
// ===========================================================================

/// Verifies the zstd compressed payload contains a valid frame header.
/// The zstd frame format starts with:
///   - Magic: 0xFD2FB528 (4 bytes LE)
///   - Frame header descriptor byte
///
/// This ensures we are producing real zstd frames, not accidentally using
/// another codec.
///
/// upstream: token.c line 688 - ZSTD_initCStream, produces standard frames
#[test]
fn golden_zstd_daemon_recv_frame_magic_present() {
    let encoded = encode_literal_only(b"verify zstd frame magic in daemon mode");

    let (_, _, payload, _) = extract_first_deflated_block(&encoded);

    // Zstd magic number: 0xFD2FB528 in little-endian
    assert_eq!(payload[0], 0x28, "zstd magic byte 0");
    assert_eq!(payload[1], 0xB5, "zstd magic byte 1");
    assert_eq!(payload[2], 0x2F, "zstd magic byte 2");
    assert_eq!(payload[3], 0xFD, "zstd magic byte 3");

    // Frame header descriptor (byte 4) - verify it exists
    assert!(
        payload.len() > 4,
        "payload must contain frame header after magic"
    );
}

/// Verifies that the compressed payload does NOT contain the zlib sync marker.
/// Zstd frames are self-delimiting and never produce 0x00 0x00 0xFF 0xFF.
///
/// upstream: token.c line 685 - zstd uses ZSTD_e_flush, no marker stripping
#[test]
fn golden_zstd_daemon_recv_no_zlib_sync_marker() {
    let encoded = encode_literal_only(SMALL_INPUT);
    let (_, _, payload, _) = extract_first_deflated_block(&encoded);

    let sync_marker = [0x00u8, 0x00, 0xFF, 0xFF];
    for window in payload.windows(4) {
        assert_ne!(
            window, &sync_marker,
            "zstd payload must not contain zlib sync marker"
        );
    }
}

// ===========================================================================
// Section 5: Decode verification - daemon receive path
// ===========================================================================

/// Verifies the complete daemon receive path: decode zstd-compressed small
/// input and recover the original bytes.
#[test]
fn golden_zstd_daemon_recv_decode_small() {
    let encoded = encode_literal_only(SMALL_INPUT);
    let (literals, blocks) = decode_all(&encoded);
    assert_eq!(literals, SMALL_INPUT);
    assert!(blocks.is_empty());
}

/// Verifies the daemon receive path for medium repetitive input.
/// The repetitive structure exercises zstd's back-reference decompression.
#[test]
fn golden_zstd_daemon_recv_decode_medium() {
    let input = medium_input();
    let encoded = encode_literal_only(&input);
    let (literals, blocks) = decode_all(&encoded);
    assert_eq!(literals, input);
    assert!(blocks.is_empty());
}

/// Verifies the daemon receive path for mixed literal + block match stream.
#[test]
fn golden_zstd_daemon_recv_decode_mixed() {
    let encoded = encode_mixed(&[b"daemon push data\n"], &[0]);
    let (literals, blocks) = decode_all(&encoded);
    assert_eq!(literals, b"daemon push data\n");
    assert_eq!(blocks, vec![0]);
}

/// Verifies the daemon receive path with multiple literals and block matches.
/// This is representative of a real incremental transfer where changed regions
/// are sent as literals and unchanged regions as block references.
#[test]
fn golden_zstd_daemon_recv_decode_interleaved() {
    let encoded = encode_mixed(
        &[b"changed region 1\n", b"changed region 2\n", b"tail\n"],
        &[0, 5],
    );
    let (literals, blocks) = decode_all(&encoded);
    assert_eq!(literals, b"changed region 1\nchanged region 2\ntail\n");
    assert_eq!(blocks, vec![0, 5]);
}

// ===========================================================================
// Section 6: Multi-file continuous session - daemon transfer pattern
// ===========================================================================

/// Verifies the daemon-mode multi-file transfer pattern where a single zstd
/// context persists across file boundaries. The second file benefits from
/// the dictionary built during the first file's compression.
///
/// upstream: token.c:688 (CCtx created once), token.c:700-703 (only run
/// state resets between files)
#[test]
fn golden_zstd_daemon_recv_multi_file_session() {
    let mut encoder = CompressedTokenEncoder::new_zstd(3).unwrap();
    let mut wire = Vec::new();

    // File 1: daemon auth response
    encoder.send_literal(&mut wire, b"auth ok\n").unwrap();
    encoder.finish(&mut wire).unwrap();

    // File 2: module content with block match
    encoder.send_literal(&mut wire, b"module data\n").unwrap();
    encoder.send_block_match(&mut wire, 0).unwrap();
    encoder.finish(&mut wire).unwrap();

    // File 3: another module file with repetitive content
    encoder.send_literal(&mut wire, b"module data\n").unwrap();
    encoder.finish(&mut wire).unwrap();

    // Decode all three files with a single persistent decoder
    let mut cursor = Cursor::new(&wire);
    let mut decoder = CompressedTokenDecoder::new_zstd().unwrap();

    // File 1
    let mut f1_lit = Vec::new();
    loop {
        match decoder.recv_token(&mut cursor).unwrap() {
            CompressedToken::Literal(d) => f1_lit.extend_from_slice(&d),
            CompressedToken::End => break,
            CompressedToken::BlockMatch(_) => panic!("file 1 should have no block matches"),
        }
    }
    assert_eq!(f1_lit, b"auth ok\n");
    decoder.reset();

    // File 2
    let mut f2_lit = Vec::new();
    let mut f2_blk = Vec::new();
    loop {
        match decoder.recv_token(&mut cursor).unwrap() {
            CompressedToken::Literal(d) => f2_lit.extend_from_slice(&d),
            CompressedToken::BlockMatch(idx) => f2_blk.push(idx),
            CompressedToken::End => break,
        }
    }
    assert_eq!(f2_lit, b"module data\n");
    assert_eq!(f2_blk, vec![0]);
    decoder.reset();

    // File 3
    let mut f3_lit = Vec::new();
    loop {
        match decoder.recv_token(&mut cursor).unwrap() {
            CompressedToken::Literal(d) => f3_lit.extend_from_slice(&d),
            CompressedToken::End => break,
            CompressedToken::BlockMatch(_) => panic!("file 3 should have no block matches"),
        }
    }
    assert_eq!(f3_lit, b"module data\n");
}

/// Verifies that the second file in a multi-file session compresses better
/// than it would in isolation, proving the zstd context carries dictionary
/// history across file boundaries.
///
/// upstream: token.c - CCtx never reset, dictionary persists
#[test]
fn golden_zstd_daemon_recv_cross_file_dictionary_benefit() {
    // Encode file 1 then file 2 in a session
    let mut encoder = CompressedTokenEncoder::new_zstd(3).unwrap();
    let mut session_wire = Vec::new();

    let shared_content = b"drwxr-xr-x  2 root root 4096 shared_data.txt\n";

    // File 1: same content
    encoder
        .send_literal(&mut session_wire, shared_content)
        .unwrap();
    encoder.finish(&mut session_wire).unwrap();

    let file1_end = session_wire.len();

    // File 2: same content again (benefits from file 1's dictionary)
    encoder
        .send_literal(&mut session_wire, shared_content)
        .unwrap();
    encoder.finish(&mut session_wire).unwrap();

    let file2_wire_len = session_wire.len() - file1_end;

    // Encode the same content in isolation (fresh context)
    let isolated = encode_literal_only(shared_content);

    // File 2 in the session should be smaller or equal to isolated encoding
    // because the zstd context carries dictionary history from file 1.
    assert!(
        file2_wire_len <= isolated.len(),
        "file 2 in session ({file2_wire_len} bytes) should be <= isolated ({} bytes) \
         due to cross-file dictionary benefit",
        isolated.len()
    );
}

// ===========================================================================
// Section 7: Compression level verification for daemon mode
// ===========================================================================

/// Verifies that daemon default level (3) produces different output than
/// level 1 (fast) for the same input, confirming the level parameter is
/// actually applied to the zstd encoder.
#[test]
fn golden_zstd_daemon_recv_level_3_differs_from_level_1() {
    let input = medium_input();

    let mut enc3 = CompressedTokenEncoder::new_zstd(3).unwrap();
    let mut wire3 = Vec::new();
    enc3.send_literal(&mut wire3, &input).unwrap();
    enc3.finish(&mut wire3).unwrap();

    let mut enc1 = CompressedTokenEncoder::new_zstd(1).unwrap();
    let mut wire1 = Vec::new();
    enc1.send_literal(&mut wire1, &input).unwrap();
    enc1.finish(&mut wire1).unwrap();

    // Both must decode to the same content
    let (lit3, _) = decode_all(&wire3);
    let (lit1, _) = decode_all(&wire1);
    assert_eq!(lit3, input);
    assert_eq!(lit1, input);

    // But the wire bytes should differ (different compression levels)
    assert_ne!(
        wire3, wire1,
        "level 3 and level 1 should produce different compressed output"
    );

    // Level 3 should compress better or equal to level 1
    assert!(
        wire3.len() <= wire1.len(),
        "level 3 ({} bytes) should be <= level 1 ({} bytes)",
        wire3.len(),
        wire1.len()
    );
}
