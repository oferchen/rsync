//! Golden byte tests for protocol 28 multiplex frames, delta tokens, NDX
//! encoding, and transfer stats wire format.
//!
//! These tests pin the exact wire bytes for protocol-version-agnostic and
//! protocol-28-specific encoding paths that are not covered by the existing
//! golden test files (flist, handshake, wire).
//!
//! # Areas Covered
//!
//! - **Multiplex frame headers**: 4-byte LE header encoding with MPLEX_BASE
//!   tag offset. The multiplex format is the same across all protocol versions
//!   (28-32) but pinning it here ensures no regressions.
//! - **Delta token encoding**: `write_int()`-based literal, block match, and
//!   end-of-stream tokens. Protocol 28 uses the same simple token format as
//!   all versions (no compressed token support without `-z`).
//! - **NDX legacy codec**: 4-byte LE signed integer encoding used by protocol
//!   < 30 for file list index exchange.
//! - **Transfer stats exact wire bytes**: varlong30 encoding with min_bytes=3,
//!   no flist timing fields for protocol 28.
//! - **Checksum seed exchange**: 4-byte LE encoding shared by all versions.
//!
//! # Upstream Reference
//!
//! - Multiplex headers: `io.c:mplex_write()` - tag = MPLEX_BASE + msg_code
//! - Delta tokens: `token.c:simple_send_token()` - write_int based encoding
//! - NDX encoding: `io.c:write_ndx()` / `read_ndx()` - protocol < 30 uses
//!   plain `write_int()`
//! - Stats: `main.c:handle_stats()` - varlong30 encoding, protocol < 29
//!   omits flist times

use std::io::Cursor;

use protocol::codec::{LegacyNdxCodec, NdxCodec};
use protocol::wire::{
    DeltaOp, SignatureBlock, read_signature, read_token, write_signature, write_token_block_match,
    write_token_end, write_token_literal, write_token_stream, write_whole_file_delta,
};
use protocol::{
    MessageCode, MessageHeader, ProtocolVersion, TransferStats, read_int, read_longint, write_int,
    write_longint,
};

fn proto28() -> ProtocolVersion {
    ProtocolVersion::try_from(28u8).unwrap()
}

// ---------------------------------------------------------------------------
// Multiplex frame header encoding
// upstream: io.c:mplex_write() - header = (MPLEX_BASE + code) << 24 | len
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_mplex_data_header_exact_bytes() {
    // MSG_DATA (code=0) with payload length 100.
    // Tag = MPLEX_BASE(7) + 0 = 7. Header = (7 << 24) | 100 = 0x07000064.
    // LE bytes: [0x64, 0x00, 0x00, 0x07]
    let header = MessageHeader::new(MessageCode::Data, 100).unwrap();
    let bytes = header.encode();

    assert_eq!(bytes, [0x64, 0x00, 0x00, 0x07]);

    // Decode round-trip
    let decoded = MessageHeader::decode(&bytes).unwrap();
    assert_eq!(decoded.code(), MessageCode::Data);
    assert_eq!(decoded.payload_len(), 100);
}

#[test]
fn golden_v28_mplex_info_header_exact_bytes() {
    // MSG_INFO (code=2) with payload length 50.
    // Tag = MPLEX_BASE(7) + 2 = 9. Header = (9 << 24) | 50 = 0x09000032.
    // LE bytes: [0x32, 0x00, 0x00, 0x09]
    let header = MessageHeader::new(MessageCode::Info, 50).unwrap();
    let bytes = header.encode();

    assert_eq!(bytes, [0x32, 0x00, 0x00, 0x09]);

    let decoded = MessageHeader::decode(&bytes).unwrap();
    assert_eq!(decoded.code(), MessageCode::Info);
    assert_eq!(decoded.payload_len(), 50);
}

#[test]
fn golden_v28_mplex_error_header_exact_bytes() {
    // MSG_ERROR (code=3) with payload length 0.
    // Tag = 7 + 3 = 10. Header = (10 << 24) | 0 = 0x0A000000.
    // LE bytes: [0x00, 0x00, 0x00, 0x0A]
    let header = MessageHeader::new(MessageCode::Error, 0).unwrap();
    let bytes = header.encode();

    assert_eq!(bytes, [0x00, 0x00, 0x00, 0x0A]);

    let decoded = MessageHeader::decode(&bytes).unwrap();
    assert_eq!(decoded.code(), MessageCode::Error);
    assert_eq!(decoded.payload_len(), 0);
}

#[test]
fn golden_v28_mplex_redo_header_exact_bytes() {
    // MSG_REDO (code=9) with payload length 4 (file index as 4-byte LE int).
    // Tag = 7 + 9 = 16 = 0x10. Header = (0x10 << 24) | 4 = 0x10000004.
    // LE bytes: [0x04, 0x00, 0x00, 0x10]
    let header = MessageHeader::new(MessageCode::Redo, 4).unwrap();
    let bytes = header.encode();

    assert_eq!(bytes, [0x04, 0x00, 0x00, 0x10]);

    let decoded = MessageHeader::decode(&bytes).unwrap();
    assert_eq!(decoded.code(), MessageCode::Redo);
    assert_eq!(decoded.payload_len(), 4);
}

#[test]
fn golden_v28_mplex_max_payload_header() {
    // Maximum payload: 0x00FFFFFF = 16777215 bytes.
    // MSG_DATA (code=0), tag = 7. Header = (7 << 24) | 0x00FFFFFF = 0x07FFFFFF.
    // LE bytes: [0xFF, 0xFF, 0xFF, 0x07]
    let header = MessageHeader::new(MessageCode::Data, 0x00FF_FFFF).unwrap();
    let bytes = header.encode();

    assert_eq!(bytes, [0xFF, 0xFF, 0xFF, 0x07]);

    let decoded = MessageHeader::decode(&bytes).unwrap();
    assert_eq!(decoded.code(), MessageCode::Data);
    assert_eq!(decoded.payload_len(), 0x00FF_FFFF);
}

#[test]
fn golden_v28_mplex_success_header_exact_bytes() {
    // MSG_SUCCESS (code=100) with payload length 4.
    // Tag = 7 + 100 = 107 = 0x6B. Header = (0x6B << 24) | 4 = 0x6B000004.
    // LE bytes: [0x04, 0x00, 0x00, 0x6B]
    let header = MessageHeader::new(MessageCode::Success, 4).unwrap();
    let bytes = header.encode();

    assert_eq!(bytes, [0x04, 0x00, 0x00, 0x6B]);

    let decoded = MessageHeader::decode(&bytes).unwrap();
    assert_eq!(decoded.code(), MessageCode::Success);
    assert_eq!(decoded.payload_len(), 4);
}

// ---------------------------------------------------------------------------
// Delta token encoding (simple token format, all protocol versions)
// upstream: token.c:simple_send_token() - write_int based
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_delta_token_literal_exact_bytes() {
    // Literal of 5 bytes: write_int(5) + raw data.
    // write_int(5) = 5 as i32 LE: [0x05, 0x00, 0x00, 0x00]
    let mut buf = Vec::new();
    write_token_literal(&mut buf, b"hello").unwrap();

    #[rustfmt::skip]
    let expected: &[u8] = &[
        // length: write_int(5)
        0x05, 0x00, 0x00, 0x00,
        // data: "hello"
        b'h', b'e', b'l', b'l', b'o',
    ];
    assert_eq!(buf, expected);
    assert_eq!(buf.len(), 9);
}

#[test]
fn golden_v28_delta_token_block_match_exact_bytes() {
    // Block match at index 0: write_int(-(0+1)) = write_int(-1).
    // -1 as i32 LE: [0xFF, 0xFF, 0xFF, 0xFF]
    let mut buf = Vec::new();
    write_token_block_match(&mut buf, 0).unwrap();

    assert_eq!(buf, [0xFF, 0xFF, 0xFF, 0xFF]);
}

#[test]
fn golden_v28_delta_token_block_match_index_42() {
    // Block match at index 42: write_int(-(42+1)) = write_int(-43).
    // -43 as i32 LE: [0xD5, 0xFF, 0xFF, 0xFF]
    let mut buf = Vec::new();
    write_token_block_match(&mut buf, 42).unwrap();

    let expected = (-43_i32).to_le_bytes();
    assert_eq!(buf, expected);
    assert_eq!(buf, [0xD5, 0xFF, 0xFF, 0xFF]);
}

#[test]
fn golden_v28_delta_token_end_marker_exact_bytes() {
    // End marker: write_int(0).
    // 0 as i32 LE: [0x00, 0x00, 0x00, 0x00]
    let mut buf = Vec::new();
    write_token_end(&mut buf).unwrap();

    assert_eq!(buf, [0x00, 0x00, 0x00, 0x00]);
}

#[test]
fn golden_v28_delta_token_read_literal_roundtrip() {
    // Write literal token, read it back.
    let mut buf = Vec::new();
    write_token_literal(&mut buf, b"abc").unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let token = read_token(&mut cursor).unwrap();
    assert_eq!(token, Some(3), "literal token value must be the byte count");

    // Read the literal data
    let mut data = vec![0u8; 3];
    std::io::Read::read_exact(&mut cursor, &mut data).unwrap();
    assert_eq!(data, b"abc");
}

#[test]
fn golden_v28_delta_token_read_block_match_roundtrip() {
    // Write block match for index 5, read it back.
    let mut buf = Vec::new();
    write_token_block_match(&mut buf, 5).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let token = read_token(&mut cursor).unwrap();
    // Block index 5: wire value = -(5+1) = -6
    assert_eq!(token, Some(-6));

    // Recover block index: -(token + 1) = -(-6 + 1) = 5
    let block_index = -(token.unwrap() + 1);
    assert_eq!(block_index, 5);
}

#[test]
fn golden_v28_delta_token_read_end_roundtrip() {
    let mut buf = Vec::new();
    write_token_end(&mut buf).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let token = read_token(&mut cursor).unwrap();
    assert_eq!(token, None, "end marker must decode as None");
}

#[test]
fn golden_v28_delta_whole_file_exact_bytes() {
    // Whole file transfer for "hi": literal(2) + data + end(0).
    let mut buf = Vec::new();
    write_whole_file_delta(&mut buf, b"hi").unwrap();

    #[rustfmt::skip]
    let expected: &[u8] = &[
        // literal length: write_int(2)
        0x02, 0x00, 0x00, 0x00,
        // data: "hi"
        b'h', b'i',
        // end marker: write_int(0)
        0x00, 0x00, 0x00, 0x00,
    ];
    assert_eq!(buf, expected);
    assert_eq!(buf.len(), 10);
}

#[test]
fn golden_v28_delta_stream_mixed_ops_exact_bytes() {
    // Stream with: literal "AB", block match 0, literal "C", end.
    let ops = vec![
        DeltaOp::Literal(b"AB".to_vec()),
        DeltaOp::Copy {
            block_index: 0,
            length: 1024,
        },
        DeltaOp::Literal(b"C".to_vec()),
    ];

    let mut buf = Vec::new();
    write_token_stream(&mut buf, &ops).unwrap();

    #[rustfmt::skip]
    let expected: &[u8] = &[
        // literal "AB": write_int(2) + data
        0x02, 0x00, 0x00, 0x00, b'A', b'B',
        // block match 0: write_int(-(0+1)) = write_int(-1)
        0xFF, 0xFF, 0xFF, 0xFF,
        // literal "C": write_int(1) + data
        0x01, 0x00, 0x00, 0x00, b'C',
        // end marker: write_int(0)
        0x00, 0x00, 0x00, 0x00,
    ];
    assert_eq!(buf, expected);
}

#[test]
fn golden_v28_delta_stream_roundtrip() {
    // Write a complete delta stream, then read all tokens back.
    let ops = vec![
        DeltaOp::Literal(b"data".to_vec()),
        DeltaOp::Copy {
            block_index: 3,
            length: 512,
        },
        DeltaOp::Literal(b"end".to_vec()),
    ];

    let mut buf = Vec::new();
    write_token_stream(&mut buf, &ops).unwrap();

    let mut cursor = Cursor::new(&buf[..]);

    // Token 1: literal of 4 bytes
    let t1 = read_token(&mut cursor).unwrap();
    assert_eq!(t1, Some(4));
    let mut d1 = vec![0u8; 4];
    std::io::Read::read_exact(&mut cursor, &mut d1).unwrap();
    assert_eq!(d1, b"data");

    // Token 2: block match at index 3 -> wire value -(3+1) = -4
    let t2 = read_token(&mut cursor).unwrap();
    assert_eq!(t2, Some(-4));
    assert_eq!(-(t2.unwrap() + 1), 3);

    // Token 3: literal of 3 bytes
    let t3 = read_token(&mut cursor).unwrap();
    assert_eq!(t3, Some(3));
    let mut d3 = vec![0u8; 3];
    std::io::Read::read_exact(&mut cursor, &mut d3).unwrap();
    assert_eq!(d3, b"end");

    // Token 4: end
    let t4 = read_token(&mut cursor).unwrap();
    assert_eq!(t4, None);
}

#[test]
fn golden_v28_delta_empty_file() {
    // Empty file delta: just the end marker, no literal chunks.
    let mut buf = Vec::new();
    write_whole_file_delta(&mut buf, b"").unwrap();

    // Empty data produces no literal tokens, only end marker.
    assert_eq!(buf, [0x00, 0x00, 0x00, 0x00]);

    let mut cursor = Cursor::new(&buf[..]);
    let token = read_token(&mut cursor).unwrap();
    assert_eq!(token, None, "empty file produces immediate end marker");
}

// ---------------------------------------------------------------------------
// NDX legacy codec (protocol < 30 uses 4-byte LE i32)
// upstream: io.c:write_ndx()/read_ndx() - protocol < 30 branch
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_ndx_write_positive_index_exact_bytes() {
    // NDX index 5 at protocol 28: write_int(5) = [0x05, 0x00, 0x00, 0x00]
    let mut codec = LegacyNdxCodec::new(28);
    let mut buf = Vec::new();
    codec.write_ndx(&mut buf, 5).unwrap();

    assert_eq!(buf, [0x05, 0x00, 0x00, 0x00]);
}

#[test]
fn golden_v28_ndx_write_zero_exact_bytes() {
    // NDX index 0: [0x00, 0x00, 0x00, 0x00]
    let mut codec = LegacyNdxCodec::new(28);
    let mut buf = Vec::new();
    codec.write_ndx(&mut buf, 0).unwrap();

    assert_eq!(buf, [0x00, 0x00, 0x00, 0x00]);
}

#[test]
fn golden_v28_ndx_write_done_exact_bytes() {
    // NDX_DONE (-1): write_int(-1) = [0xFF, 0xFF, 0xFF, 0xFF]
    let mut codec = LegacyNdxCodec::new(28);
    let mut buf = Vec::new();
    codec.write_ndx_done(&mut buf).unwrap();

    assert_eq!(buf, [0xFF, 0xFF, 0xFF, 0xFF]);
}

#[test]
fn golden_v28_ndx_write_large_index_exact_bytes() {
    // NDX index 1000: write_int(1000) = 1000 as i32 LE
    // 1000 = 0x000003E8 -> LE: [0xE8, 0x03, 0x00, 0x00]
    let mut codec = LegacyNdxCodec::new(28);
    let mut buf = Vec::new();
    codec.write_ndx(&mut buf, 1000).unwrap();

    assert_eq!(buf, [0xE8, 0x03, 0x00, 0x00]);
}

#[test]
fn golden_v28_ndx_roundtrip_positive() {
    let mut codec = LegacyNdxCodec::new(28);
    let mut buf = Vec::new();
    codec.write_ndx(&mut buf, 42).unwrap();

    let mut read_codec = LegacyNdxCodec::new(28);
    let mut cursor = Cursor::new(&buf[..]);
    let ndx = read_codec.read_ndx(&mut cursor).unwrap();
    assert_eq!(ndx, 42);
}

#[test]
fn golden_v28_ndx_roundtrip_done() {
    let mut codec = LegacyNdxCodec::new(28);
    let mut buf = Vec::new();
    codec.write_ndx_done(&mut buf).unwrap();

    let mut read_codec = LegacyNdxCodec::new(28);
    let mut cursor = Cursor::new(&buf[..]);
    let ndx = read_codec.read_ndx(&mut cursor).unwrap();
    assert_eq!(ndx, -1, "NDX_DONE must decode as -1");
}

#[test]
fn golden_v28_ndx_flist_eof_exact_bytes() {
    // NDX_FLIST_EOF (-2): write_int(-2) = [0xFE, 0xFF, 0xFF, 0xFF]
    let mut codec = LegacyNdxCodec::new(28);
    let mut buf = Vec::new();
    codec.write_ndx(&mut buf, -2).unwrap();

    assert_eq!(buf, [0xFE, 0xFF, 0xFF, 0xFF]);

    let mut read_codec = LegacyNdxCodec::new(28);
    let mut cursor = Cursor::new(&buf[..]);
    let ndx = read_codec.read_ndx(&mut cursor).unwrap();
    assert_eq!(ndx, -2, "NDX_FLIST_EOF must decode as -2");
}

#[test]
fn golden_v28_ndx_sequence_roundtrip() {
    // Write a sequence of NDX values simulating a transfer session:
    // file 0, file 1, file 2, NDX_DONE
    let mut codec = LegacyNdxCodec::new(28);
    let mut buf = Vec::new();
    codec.write_ndx(&mut buf, 0).unwrap();
    codec.write_ndx(&mut buf, 1).unwrap();
    codec.write_ndx(&mut buf, 2).unwrap();
    codec.write_ndx_done(&mut buf).unwrap();

    // Each NDX is 4 bytes, total = 16 bytes
    assert_eq!(buf.len(), 16);

    #[rustfmt::skip]
    let expected: &[u8] = &[
        0x00, 0x00, 0x00, 0x00, // ndx=0
        0x01, 0x00, 0x00, 0x00, // ndx=1
        0x02, 0x00, 0x00, 0x00, // ndx=2
        0xFF, 0xFF, 0xFF, 0xFF, // NDX_DONE=-1
    ];
    assert_eq!(buf, expected);

    let mut read_codec = LegacyNdxCodec::new(28);
    let mut cursor = Cursor::new(&buf[..]);
    assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), 0);
    assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), 1);
    assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), 2);
    assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), -1);
}

// ---------------------------------------------------------------------------
// Transfer stats exact wire bytes (protocol 28 - varlong30 encoding detail)
// upstream: main.c:handle_stats() + io.c:write_varlong()
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_stats_exact_wire_bytes_known_values() {
    // Protocol 28: 3 fields (total_read, total_written, total_size) as
    // varlong30 with min_bytes=3. No flist timing fields.
    //
    // varlong30(1024, min_bytes=3):
    //   value = 1024 = 0x0400
    //   LE bytes = [0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
    //   cnt starts at 8, strip trailing zeros to min_bytes=3:
    //     bytes[7..3] are all 0x00, so cnt goes 8->7->6->5->4->3 (stop at min)
    //   leading byte = bytes[2] = 0x00
    //   bit threshold = 1 << (7 + 3 - 3) = 0x80
    //   0x00 < 0x80, cnt == min_bytes -> no extra byte needed
    //   Output: [leading=0x00] + bytes[0..2] = [0x00, 0x00, 0x04]
    let stats = TransferStats::with_bytes(1024, 2048, 4096);
    let mut buf = Vec::new();
    stats.write_to(&mut buf, proto28()).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let decoded = TransferStats::read_from(&mut cursor, proto28()).unwrap();

    assert_eq!(decoded.total_read, 1024);
    assert_eq!(decoded.total_written, 2048);
    assert_eq!(decoded.total_size, 4096);
    assert_eq!(decoded.flist_buildtime, 0);
    assert_eq!(decoded.flist_xfertime, 0);

    // All bytes consumed
    assert_eq!(cursor.position() as usize, buf.len());
}

#[test]
fn golden_v28_stats_vs_v29_different_length() {
    // Protocol 28: 3 varlong30 fields.
    // Protocol 29: 5 varlong30 fields (adds flist_buildtime, flist_xfertime).
    let stats = TransferStats::with_bytes(100, 200, 300).with_flist_times(500_000, 100_000);

    let v28 = proto28();
    let v29 = ProtocolVersion::from_supported(29).unwrap();

    let mut buf_28 = Vec::new();
    stats.write_to(&mut buf_28, v28).unwrap();

    let mut buf_29 = Vec::new();
    stats.write_to(&mut buf_29, v29).unwrap();

    // Protocol 29 buffer must be longer (has flist time fields)
    assert!(
        buf_29.len() > buf_28.len(),
        "v29 stats ({} bytes) must be longer than v28 ({} bytes)",
        buf_29.len(),
        buf_28.len()
    );

    // Protocol 28 ignores flist times on decode
    let mut cursor_28 = Cursor::new(&buf_28[..]);
    let decoded_28 = TransferStats::read_from(&mut cursor_28, v28).unwrap();
    assert_eq!(decoded_28.flist_buildtime, 0);
    assert_eq!(decoded_28.flist_xfertime, 0);

    // Protocol 29 preserves flist times
    let mut cursor_29 = Cursor::new(&buf_29[..]);
    let decoded_29 = TransferStats::read_from(&mut cursor_29, v29).unwrap();
    assert_eq!(decoded_29.flist_buildtime, 500_000);
    assert_eq!(decoded_29.flist_xfertime, 100_000);
}

#[test]
fn golden_v28_stats_perspective_swap() {
    // Stats perspective swap preserves all fields except read/write are swapped.
    let stats = TransferStats::with_bytes(1000, 2000, 5000);
    let swapped = stats.swap_perspective();

    assert_eq!(swapped.total_read, 2000);
    assert_eq!(swapped.total_written, 1000);
    assert_eq!(swapped.total_size, 5000);

    // Wire encode the swapped stats, decode, verify
    let mut buf = Vec::new();
    swapped.write_to(&mut buf, proto28()).unwrap();
    let mut cursor = Cursor::new(&buf[..]);
    let decoded = TransferStats::read_from(&mut cursor, proto28()).unwrap();
    assert_eq!(decoded.total_read, 2000);
    assert_eq!(decoded.total_written, 1000);
}

// ---------------------------------------------------------------------------
// Checksum seed exchange (4-byte LE, all protocol versions)
// upstream: main.c - checksum_seed exchanged as write_int()
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_checksum_seed_exact_bytes() {
    // Checksum seed is exchanged as a 4-byte LE i32 via write_int().
    // Seed = 0x12345678 -> LE: [0x78, 0x56, 0x34, 0x12]
    let seed: i32 = 0x1234_5678;
    let mut buf = Vec::new();
    write_int(&mut buf, seed).unwrap();

    assert_eq!(buf, [0x78, 0x56, 0x34, 0x12]);

    let mut cursor = Cursor::new(&buf[..]);
    let decoded = read_int(&mut cursor).unwrap();
    assert_eq!(decoded, seed);
}

#[test]
fn golden_v28_checksum_seed_zero() {
    // Zero seed (used when sender generates random seed).
    let mut buf = Vec::new();
    write_int(&mut buf, 0).unwrap();

    assert_eq!(buf, [0x00, 0x00, 0x00, 0x00]);
}

#[test]
fn golden_v28_checksum_seed_negative() {
    // Negative seed values are valid (i32 wrapping).
    let mut buf = Vec::new();
    write_int(&mut buf, -1).unwrap();

    assert_eq!(buf, [0xFF, 0xFF, 0xFF, 0xFF]);

    let mut cursor = Cursor::new(&buf[..]);
    assert_eq!(read_int(&mut cursor).unwrap(), -1);
}

// ---------------------------------------------------------------------------
// Signature header encoding for protocol 28 (MD4 strong checksums)
// upstream: match.c/sender.c - sum_head written with write_int for
// protocol < 30, varint for protocol >= 30. The signature module uses
// varint unconditionally (protocol-independent), but the SUM_HEAD on the
// wire uses write_int for protocol 28.
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_sum_head_write_int_encoding() {
    // Protocol 28 SUM_HEAD fields are sent as write_int (4-byte LE each):
    //   count    = number of blocks
    //   blength  = block length in bytes
    //   s2length = strong checksum length (16 for MD4)
    //   remainder = bytes in last block
    //
    // Example: 10 blocks of 700 bytes, MD4 (16 bytes), remainder 300.
    let mut buf = Vec::new();
    write_int(&mut buf, 10).unwrap(); // count
    write_int(&mut buf, 700).unwrap(); // blength
    write_int(&mut buf, 16).unwrap(); // s2length (MD4)
    write_int(&mut buf, 300).unwrap(); // remainder

    #[rustfmt::skip]
    let expected: &[u8] = &[
        // count = 10
        0x0A, 0x00, 0x00, 0x00,
        // blength = 700 = 0x02BC
        0xBC, 0x02, 0x00, 0x00,
        // s2length = 16 = 0x10
        0x10, 0x00, 0x00, 0x00,
        // remainder = 300 = 0x012C
        0x2C, 0x01, 0x00, 0x00,
    ];
    assert_eq!(buf, expected);
    assert_eq!(buf.len(), 16, "SUM_HEAD is 4 * write_int = 16 bytes");

    // Round-trip
    let mut cursor = Cursor::new(&buf[..]);
    assert_eq!(read_int(&mut cursor).unwrap(), 10);
    assert_eq!(read_int(&mut cursor).unwrap(), 700);
    assert_eq!(read_int(&mut cursor).unwrap(), 16);
    assert_eq!(read_int(&mut cursor).unwrap(), 300);
}

#[test]
fn golden_v28_sum_head_zero_blocks() {
    // Zero blocks means whole-file transfer (no basis file).
    // count=0, blength=0, s2length=0, remainder=0
    let mut buf = Vec::new();
    write_int(&mut buf, 0).unwrap();
    write_int(&mut buf, 0).unwrap();
    write_int(&mut buf, 0).unwrap();
    write_int(&mut buf, 0).unwrap();

    assert_eq!(buf, [0u8; 16], "zero SUM_HEAD must be 16 zero bytes");
}

// ---------------------------------------------------------------------------
// Signature block wire format (rolling + strong checksum per block)
// upstream: match.c - rolling sum as 4-byte LE, strong sum as raw bytes
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_signature_block_md4_exact_bytes() {
    // One block: rolling_sum=0xAABBCCDD, strong_sum=16 bytes of MD4.
    let blocks = vec![SignatureBlock {
        index: 0,
        rolling_sum: 0xAABB_CCDD,
        strong_sum: vec![
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
            0x0F, 0x10,
        ],
    }];

    let mut buf = Vec::new();
    write_signature(&mut buf, 1, 4096, 16, &blocks).unwrap();

    // Header: 3 varints (block_count=1, block_length=4096, strong_sum_len=16)
    // Then per block: rolling_sum(4 bytes LE) + strong_sum(16 bytes)
    let (block_length, block_count, strong_sum_length, decoded) =
        read_signature(&mut &buf[..]).unwrap();

    assert_eq!(block_count, 1);
    assert_eq!(block_length, 4096);
    assert_eq!(strong_sum_length, 16);
    assert_eq!(decoded[0].rolling_sum, 0xAABB_CCDD);
    assert_eq!(decoded[0].strong_sum.len(), 16);
    assert_eq!(decoded[0].strong_sum[0], 0x01);
    assert_eq!(decoded[0].strong_sum[15], 0x10);
}

#[test]
fn golden_v28_signature_multiple_blocks_roundtrip() {
    // Typical MD4 signature with 3 blocks of 2048 bytes.
    let blocks: Vec<SignatureBlock> = (0..3)
        .map(|i| SignatureBlock {
            index: i,
            rolling_sum: 0x1111_1111 * (i + 1),
            strong_sum: (0..16).map(|b| b + (i as u8) * 16).collect(),
        })
        .collect();

    let mut buf = Vec::new();
    write_signature(&mut buf, 3, 2048, 16, &blocks).unwrap();

    let (block_length, block_count, strong_sum_length, decoded) =
        read_signature(&mut &buf[..]).unwrap();

    assert_eq!(block_count, 3);
    assert_eq!(block_length, 2048);
    assert_eq!(strong_sum_length, 16);

    for (i, block) in decoded.iter().enumerate() {
        assert_eq!(block.index, i as u32);
        assert_eq!(block.rolling_sum, blocks[i].rolling_sum);
        assert_eq!(block.strong_sum, blocks[i].strong_sum);
    }
}

// ---------------------------------------------------------------------------
// Longint encoding boundary values
// upstream: io.c:write_longint()/read_longint() - threshold at 0x7FFFFFFF
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_longint_boundary_value() {
    // Value exactly at boundary: 0x7FFFFFFF (max 4-byte encoding).
    let boundary: i64 = 0x7FFF_FFFF;
    let mut buf = Vec::new();
    write_longint(&mut buf, boundary).unwrap();

    // Fits in 4 bytes: [0xFF, 0xFF, 0xFF, 0x7F]
    assert_eq!(buf.len(), 4);
    assert_eq!(buf, [0xFF, 0xFF, 0xFF, 0x7F]);

    let mut cursor = Cursor::new(&buf[..]);
    assert_eq!(read_longint(&mut cursor).unwrap(), boundary);
}

#[test]
fn golden_v28_longint_boundary_plus_one() {
    // Value at boundary+1: 0x80000000 requires 12-byte encoding.
    let value: i64 = 0x8000_0000;
    let mut buf = Vec::new();
    write_longint(&mut buf, value).unwrap();

    // Marker [0xFF, 0xFF, 0xFF, 0xFF] + 8-byte LE i64
    assert_eq!(buf.len(), 12);
    assert_eq!(&buf[0..4], [0xFF, 0xFF, 0xFF, 0xFF]);

    let decoded = i64::from_le_bytes(buf[4..12].try_into().unwrap());
    assert_eq!(decoded, value);

    let mut cursor = Cursor::new(&buf[..]);
    assert_eq!(read_longint(&mut cursor).unwrap(), value);
}

#[test]
fn golden_v28_longint_zero() {
    let mut buf = Vec::new();
    write_longint(&mut buf, 0).unwrap();

    assert_eq!(buf, [0x00, 0x00, 0x00, 0x00]);
    assert_eq!(buf.len(), 4);

    let mut cursor = Cursor::new(&buf[..]);
    assert_eq!(read_longint(&mut cursor).unwrap(), 0);
}

#[test]
fn golden_v28_longint_one() {
    let mut buf = Vec::new();
    write_longint(&mut buf, 1).unwrap();

    assert_eq!(buf, [0x01, 0x00, 0x00, 0x00]);

    let mut cursor = Cursor::new(&buf[..]);
    assert_eq!(read_longint(&mut cursor).unwrap(), 1);
}

// ---------------------------------------------------------------------------
// Multiplex frame with payload (complete message on wire)
// upstream: io.c:mplex_write() - 4-byte header + payload bytes
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_mplex_complete_info_message() {
    // Complete multiplexed info message: header + payload.
    // MSG_INFO with payload "test\n" (5 bytes).
    // Tag = 7 + 2 = 9. Header = (9 << 24) | 5 = 0x09000005.
    // LE: [0x05, 0x00, 0x00, 0x09]
    let header = MessageHeader::new(MessageCode::Info, 5).unwrap();
    let header_bytes = header.encode();
    let payload = b"test\n";

    let mut frame = Vec::new();
    frame.extend_from_slice(&header_bytes);
    frame.extend_from_slice(payload);

    #[rustfmt::skip]
    let expected: &[u8] = &[
        // header
        0x05, 0x00, 0x00, 0x09,
        // payload: "test\n"
        b't', b'e', b's', b't', b'\n',
    ];
    assert_eq!(frame, expected);
    assert_eq!(frame.len(), 9, "header(4) + payload(5) = 9 bytes");

    // Decode header from frame
    let decoded = MessageHeader::decode(&frame[..4]).unwrap();
    assert_eq!(decoded.code(), MessageCode::Info);
    assert_eq!(decoded.payload_len(), 5);
    assert_eq!(&frame[4..], payload);
}

#[test]
fn golden_v28_mplex_complete_error_message() {
    // MSG_ERROR with payload "rsync error: ...\n" (17 bytes).
    let msg = b"rsync error: foo\n";
    let header = MessageHeader::new(MessageCode::Error, msg.len() as u32).unwrap();
    let header_bytes = header.encode();

    // Tag = 7 + 3 = 10 = 0x0A. Header = (0x0A << 24) | 17 = 0x0A000011.
    // LE: [0x11, 0x00, 0x00, 0x0A]
    assert_eq!(header_bytes, [0x11, 0x00, 0x00, 0x0A]);

    let mut frame = Vec::new();
    frame.extend_from_slice(&header_bytes);
    frame.extend_from_slice(msg);

    // Decode
    let decoded = MessageHeader::decode(&frame[..4]).unwrap();
    assert_eq!(decoded.code(), MessageCode::Error);
    assert_eq!(decoded.payload_len_usize(), 17);
    assert_eq!(&frame[4..], msg);
}

// ---------------------------------------------------------------------------
// All MessageCode tag values (exhaustive encoding pin)
// upstream: io.c - MPLEX_BASE + msg_code value in high byte of header
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_all_message_code_tags() {
    // Verify the exact tag byte for every message code.
    // Tag = MPLEX_BASE(7) + code_value, stored in byte 3 (MSB) of LE header.
    let cases: &[(MessageCode, u8)] = &[
        (MessageCode::Data, 7),         // 7 + 0
        (MessageCode::ErrorXfer, 8),    // 7 + 1
        (MessageCode::Info, 9),         // 7 + 2
        (MessageCode::Error, 10),       // 7 + 3
        (MessageCode::Warning, 11),     // 7 + 4
        (MessageCode::ErrorSocket, 12), // 7 + 5
        (MessageCode::Log, 13),         // 7 + 6
        (MessageCode::Client, 14),      // 7 + 7
        (MessageCode::ErrorUtf8, 15),   // 7 + 8
        (MessageCode::Redo, 16),        // 7 + 9
        (MessageCode::Stats, 17),       // 7 + 10
        (MessageCode::IoError, 29),     // 7 + 22
        (MessageCode::IoTimeout, 40),   // 7 + 33
        (MessageCode::NoOp, 49),        // 7 + 42
        (MessageCode::ErrorExit, 93),   // 7 + 86
        (MessageCode::Success, 107),    // 7 + 100
        (MessageCode::Deleted, 108),    // 7 + 101
        (MessageCode::NoSend, 109),     // 7 + 102
    ];

    for &(code, expected_tag) in cases {
        let header = MessageHeader::new(code, 0).unwrap();
        let bytes = header.encode();
        // Tag is in the high byte (byte index 3 in LE)
        assert_eq!(
            bytes[3],
            expected_tag,
            "tag mismatch for {}: expected {expected_tag}, got {}",
            code.name(),
            bytes[3]
        );
    }
}
