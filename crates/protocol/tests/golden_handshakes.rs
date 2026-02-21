//! Golden byte tests for upstream rsync wire format compatibility.
//!
//! These tests embed known-correct byte sequences derived from the upstream
//! rsync 3.4.1 C source code and verify that our encoding and decoding
//! produces identical results. Each test documents the upstream reference
//! (source file and function) so deviations can be traced back to the
//! authoritative implementation.

use std::io::Cursor;

use protocol::{
    CompatibilityFlags, DeleteStats, MessageCode, MessageHeader, ProtocolVersion, TransferStats,
    decode_varint, encode_varint_to_vec, format_legacy_daemon_greeting,
    parse_legacy_daemon_greeting, read_int, read_longint, read_varlong30, write_int, write_longint,
    write_varlong30,
};

// ---------------------------------------------------------------------------
// Server greeting wire format
// upstream: clientserver.c — start_daemon(), sends "@RSYNCD: <ver>.0\n"
// ---------------------------------------------------------------------------

#[test]
fn golden_server_greeting_protocol_32() {
    let greeting = format_legacy_daemon_greeting(ProtocolVersion::V32);
    assert_eq!(greeting, "@RSYNCD: 32.0\n");
    assert_eq!(greeting.as_bytes(), b"@RSYNCD: 32.0\n");
}

#[test]
fn golden_server_greeting_protocol_31() {
    let greeting = format_legacy_daemon_greeting(ProtocolVersion::V31);
    assert_eq!(greeting, "@RSYNCD: 31.0\n");
    assert_eq!(greeting.as_bytes(), b"@RSYNCD: 31.0\n");
}

#[test]
fn golden_server_greeting_protocol_30() {
    let greeting = format_legacy_daemon_greeting(ProtocolVersion::V30);
    assert_eq!(greeting, "@RSYNCD: 30.0\n");
    assert_eq!(greeting.as_bytes(), b"@RSYNCD: 30.0\n");
}

#[test]
fn golden_server_greeting_protocol_29() {
    let greeting = format_legacy_daemon_greeting(ProtocolVersion::V29);
    assert_eq!(greeting, "@RSYNCD: 29.0\n");
    assert_eq!(greeting.as_bytes(), b"@RSYNCD: 29.0\n");
}

#[test]
fn golden_server_greeting_protocol_28() {
    let greeting = format_legacy_daemon_greeting(ProtocolVersion::V28);
    assert_eq!(greeting, "@RSYNCD: 28.0\n");
    assert_eq!(greeting.as_bytes(), b"@RSYNCD: 28.0\n");
}

// ---------------------------------------------------------------------------
// Greeting parsing
// upstream: clientserver.c — start_inband_exchange() parses the greeting
// ---------------------------------------------------------------------------

#[test]
fn golden_parse_server_greeting_32() {
    let version = parse_legacy_daemon_greeting("@RSYNCD: 32.0\n").unwrap();
    assert_eq!(version.as_u8(), 32);
}

#[test]
fn golden_parse_server_greeting_29() {
    let version = parse_legacy_daemon_greeting("@RSYNCD: 29.0\n").unwrap();
    assert_eq!(version.as_u8(), 29);
}

#[test]
fn golden_parse_server_greeting_28() {
    let version = parse_legacy_daemon_greeting("@RSYNCD: 28.0\n").unwrap();
    assert_eq!(version.as_u8(), 28);
}

#[test]
fn golden_greeting_byte_exact_format() {
    // upstream rsync greeting is exactly "@RSYNCD: " + decimal version + ".0\n"
    // The space after the colon and the ".0" sub-version are mandatory.
    let expected_bytes: &[u8] = b"@RSYNCD: 32.0\n";
    let generated = format_legacy_daemon_greeting(ProtocolVersion::V32);
    assert_eq!(
        generated.len(),
        14,
        "greeting must be exactly 14 bytes for protocol 32"
    );
    assert_eq!(generated.as_bytes(), expected_bytes);
}

// ---------------------------------------------------------------------------
// Multiplex frame header encoding
// upstream: io.c — mplex_write(), MPLEX_BASE=7, tag=(code+MPLEX_BASE)<<24 | len
// ---------------------------------------------------------------------------

#[test]
fn golden_mplex_data_header() {
    // MSG_DATA=0, MPLEX_BASE=7 => tag byte = 7
    // payload_len=256 => 0x00000100
    // wire: (7<<24) | 256 = 0x07000100 => LE bytes: [0x00, 0x01, 0x00, 0x07]
    let header = MessageHeader::new(MessageCode::Data, 256).unwrap();
    let bytes = header.encode();
    assert_eq!(bytes, [0x00, 0x01, 0x00, 0x07]);
}

#[test]
fn golden_mplex_info_header() {
    // MSG_INFO=2, MPLEX_BASE=7 => tag byte = 9
    // payload_len=42 => 0x0000002A
    // wire: (9<<24) | 42 = 0x0900002A => LE bytes: [0x2A, 0x00, 0x00, 0x09]
    let header = MessageHeader::new(MessageCode::Info, 42).unwrap();
    let bytes = header.encode();
    assert_eq!(bytes, [0x2A, 0x00, 0x00, 0x09]);
}

#[test]
fn golden_mplex_error_header() {
    // MSG_ERROR=3, MPLEX_BASE=7 => tag byte = 10
    // payload_len=100 => 0x00000064
    // wire: (10<<24) | 100 = 0x0A000064 => LE bytes: [0x64, 0x00, 0x00, 0x0A]
    let header = MessageHeader::new(MessageCode::Error, 100).unwrap();
    let bytes = header.encode();
    assert_eq!(bytes, [0x64, 0x00, 0x00, 0x0A]);
}

#[test]
fn golden_mplex_warning_header() {
    // MSG_WARNING=4, MPLEX_BASE=7 => tag byte = 11
    // payload_len=0 (empty payload)
    // wire: (11<<24) | 0 = 0x0B000000 => LE bytes: [0x00, 0x00, 0x00, 0x0B]
    let header = MessageHeader::new(MessageCode::Warning, 0).unwrap();
    let bytes = header.encode();
    assert_eq!(bytes, [0x00, 0x00, 0x00, 0x0B]);
}

#[test]
fn golden_mplex_error_xfer_header() {
    // MSG_ERROR_XFER=1, MPLEX_BASE=7 => tag byte = 8
    // payload_len=1024 => 0x00000400
    // wire: (8<<24) | 1024 = 0x08000400 => LE bytes: [0x00, 0x04, 0x00, 0x08]
    let header = MessageHeader::new(MessageCode::ErrorXfer, 1024).unwrap();
    let bytes = header.encode();
    assert_eq!(bytes, [0x00, 0x04, 0x00, 0x08]);
}

#[test]
fn golden_mplex_stats_header() {
    // MSG_STATS=10, MPLEX_BASE=7 => tag byte = 17 = 0x11
    // payload_len=15
    // wire: (17<<24) | 15 = 0x1100000F => LE bytes: [0x0F, 0x00, 0x00, 0x11]
    let header = MessageHeader::new(MessageCode::Stats, 15).unwrap();
    let bytes = header.encode();
    assert_eq!(bytes, [0x0F, 0x00, 0x00, 0x11]);
}

#[test]
fn golden_mplex_redo_header() {
    // MSG_REDO=9, MPLEX_BASE=7 => tag byte = 16 = 0x10
    // payload_len=4
    // wire: (16<<24) | 4 = 0x10000004 => LE bytes: [0x04, 0x00, 0x00, 0x10]
    let header = MessageHeader::new(MessageCode::Redo, 4).unwrap();
    let bytes = header.encode();
    assert_eq!(bytes, [0x04, 0x00, 0x00, 0x10]);
}

#[test]
fn golden_mplex_success_header() {
    // MSG_SUCCESS=100, MPLEX_BASE=7 => tag byte = 107 = 0x6B
    // payload_len=4
    // wire: (107<<24) | 4 = 0x6B000004 => LE bytes: [0x04, 0x00, 0x00, 0x6B]
    let header = MessageHeader::new(MessageCode::Success, 4).unwrap();
    let bytes = header.encode();
    assert_eq!(bytes, [0x04, 0x00, 0x00, 0x6B]);
}

#[test]
fn golden_mplex_noop_header() {
    // MSG_NOOP=42, MPLEX_BASE=7 => tag byte = 49 = 0x31
    // payload_len=0
    // wire: (49<<24) | 0 = 0x31000000 => LE bytes: [0x00, 0x00, 0x00, 0x31]
    let header = MessageHeader::new(MessageCode::NoOp, 0).unwrap();
    let bytes = header.encode();
    assert_eq!(bytes, [0x00, 0x00, 0x00, 0x31]);
}

#[test]
fn golden_mplex_max_payload_header() {
    // MSG_DATA=0, MPLEX_BASE=7 => tag byte = 7
    // payload_len = 0x00FFFFFF (max 24-bit = 16777215)
    // wire: (7<<24) | 0x00FFFFFF = 0x07FFFFFF => LE bytes: [0xFF, 0xFF, 0xFF, 0x07]
    let header = MessageHeader::new(MessageCode::Data, 0x00FF_FFFF).unwrap();
    let bytes = header.encode();
    assert_eq!(bytes, [0xFF, 0xFF, 0xFF, 0x07]);
}

#[test]
fn golden_mplex_decode_roundtrip_all_codes() {
    // Verify every message code round-trips through encode/decode
    let test_cases: &[(MessageCode, u32)] = &[
        (MessageCode::Data, 1),
        (MessageCode::ErrorXfer, 50),
        (MessageCode::Info, 100),
        (MessageCode::Error, 200),
        (MessageCode::Warning, 300),
        (MessageCode::ErrorSocket, 400),
        (MessageCode::Log, 500),
        (MessageCode::Client, 600),
        (MessageCode::ErrorUtf8, 700),
        (MessageCode::Redo, 4),
        (MessageCode::Stats, 15),
        (MessageCode::IoError, 4),
        (MessageCode::IoTimeout, 4),
        (MessageCode::NoOp, 0),
        (MessageCode::ErrorExit, 4),
        (MessageCode::Success, 4),
        (MessageCode::Deleted, 4),
        (MessageCode::NoSend, 4),
    ];

    for &(code, payload_len) in test_cases {
        let header = MessageHeader::new(code, payload_len).unwrap();
        let encoded = header.encode();
        let decoded = MessageHeader::decode(&encoded).unwrap();
        assert_eq!(decoded.code(), code, "code mismatch for {}", code.name());
        assert_eq!(
            decoded.payload_len(),
            payload_len,
            "payload_len mismatch for {}",
            code.name()
        );
    }
}

// ---------------------------------------------------------------------------
// Varint encoding (protocol 30+)
// upstream: io.c — write_varint() / read_varint()
// ---------------------------------------------------------------------------

#[test]
fn golden_varint_zero() {
    // 0 fits in 7 bits => single byte 0x00
    let mut buf = Vec::new();
    encode_varint_to_vec(0, &mut buf);
    assert_eq!(buf, [0x00]);
    let (val, rest) = decode_varint(&buf).unwrap();
    assert_eq!(val, 0);
    assert!(rest.is_empty());
}

#[test]
fn golden_varint_one() {
    // 1 fits in 7 bits => single byte 0x01
    let mut buf = Vec::new();
    encode_varint_to_vec(1, &mut buf);
    assert_eq!(buf, [0x01]);
    let (val, _) = decode_varint(&buf).unwrap();
    assert_eq!(val, 1);
}

#[test]
fn golden_varint_127() {
    // 127 = 0x7F, fits in 7 bits => single byte 0x7F
    let mut buf = Vec::new();
    encode_varint_to_vec(127, &mut buf);
    assert_eq!(buf, [0x7F]);
    let (val, _) = decode_varint(&buf).unwrap();
    assert_eq!(val, 127);
}

#[test]
fn golden_varint_128() {
    // 128 = 0x80, needs 2 bytes
    // upstream: 10xxxxxx + 1 extra byte
    // encoded: [0x80, 0x80]
    let mut buf = Vec::new();
    encode_varint_to_vec(128, &mut buf);
    assert_eq!(buf, [0x80, 0x80]);
    let (val, _) = decode_varint(&buf).unwrap();
    assert_eq!(val, 128);
}

#[test]
fn golden_varint_255() {
    // 255 = 0xFF, needs 2 bytes
    // encoded: [0x80, 0xFF]
    let mut buf = Vec::new();
    encode_varint_to_vec(255, &mut buf);
    assert_eq!(buf, [0x80, 0xFF]);
    let (val, _) = decode_varint(&buf).unwrap();
    assert_eq!(val, 255);
}

#[test]
fn golden_varint_256() {
    // 256 = 0x0100, needs 2 bytes
    // encoded: [0x81, 0x00]
    let mut buf = Vec::new();
    encode_varint_to_vec(256, &mut buf);
    assert_eq!(buf, [0x81, 0x00]);
    let (val, _) = decode_varint(&buf).unwrap();
    assert_eq!(val, 256);
}

#[test]
fn golden_varint_16383() {
    // 16383 = 0x3FFF, maximum value for 2-byte varint
    // 10|xxxxxx => high 6 bits data mask = 0x3F
    // encoded: [0xBF, 0xFF] (0xBF = 0x80 | 0x3F)
    let mut buf = Vec::new();
    encode_varint_to_vec(16383, &mut buf);
    assert_eq!(buf, [0xBF, 0xFF]);
    let (val, _) = decode_varint(&buf).unwrap();
    assert_eq!(val, 16383);
}

#[test]
fn golden_varint_16384() {
    // 16384 = 0x4000, needs 3 bytes
    // 110|xxxxx + 2 extra bytes
    // encoded: [0xC0, 0x00, 0x40]
    let mut buf = Vec::new();
    encode_varint_to_vec(16384, &mut buf);
    assert_eq!(buf, [0xC0, 0x00, 0x40]);
    let (val, _) = decode_varint(&buf).unwrap();
    assert_eq!(val, 16384);
}

#[test]
fn golden_varint_large_positive() {
    // 1073741824 = 0x40000000, needs 5 bytes
    // 11110|xxx + 4 extra bytes
    // encoded: [0xF0, 0x00, 0x00, 0x00, 0x40]
    let mut buf = Vec::new();
    encode_varint_to_vec(1_073_741_824, &mut buf);
    assert_eq!(buf, [0xF0, 0x00, 0x00, 0x00, 0x40]);
    let (val, _) = decode_varint(&buf).unwrap();
    assert_eq!(val, 1_073_741_824);
}

#[test]
fn golden_varint_negative_one() {
    // -1 in two's complement = 0xFFFFFFFF, needs 5 bytes
    // encoded: [0xF0, 0xFF, 0xFF, 0xFF, 0xFF]
    let mut buf = Vec::new();
    encode_varint_to_vec(-1, &mut buf);
    assert_eq!(buf, [0xF0, 0xFF, 0xFF, 0xFF, 0xFF]);
    let (val, _) = decode_varint(&buf).unwrap();
    assert_eq!(val, -1);
}

#[test]
fn golden_varint_negative_128() {
    // -128 in two's complement = 0xFFFFFF80, needs 5 bytes
    // encoded: [0xF0, 0x80, 0xFF, 0xFF, 0xFF]
    let mut buf = Vec::new();
    encode_varint_to_vec(-128, &mut buf);
    assert_eq!(buf, [0xF0, 0x80, 0xFF, 0xFF, 0xFF]);
    let (val, _) = decode_varint(&buf).unwrap();
    assert_eq!(val, -128);
}

// ---------------------------------------------------------------------------
// Fixed 4-byte integer encoding (write_int / read_int, all protocol versions)
// upstream: io.c — write_int() / read_int()
// ---------------------------------------------------------------------------

#[test]
fn golden_write_int_zero() {
    let mut buf = Vec::new();
    write_int(&mut buf, 0).unwrap();
    assert_eq!(buf, [0x00, 0x00, 0x00, 0x00]);

    let mut cursor = Cursor::new(&buf);
    assert_eq!(read_int(&mut cursor).unwrap(), 0);
}

#[test]
fn golden_write_int_one() {
    let mut buf = Vec::new();
    write_int(&mut buf, 1).unwrap();
    assert_eq!(buf, [0x01, 0x00, 0x00, 0x00]);

    let mut cursor = Cursor::new(&buf);
    assert_eq!(read_int(&mut cursor).unwrap(), 1);
}

#[test]
fn golden_write_int_max_positive() {
    // i32::MAX = 0x7FFFFFFF
    let mut buf = Vec::new();
    write_int(&mut buf, i32::MAX).unwrap();
    assert_eq!(buf, [0xFF, 0xFF, 0xFF, 0x7F]);

    let mut cursor = Cursor::new(&buf);
    assert_eq!(read_int(&mut cursor).unwrap(), i32::MAX);
}

#[test]
fn golden_write_int_negative_one() {
    // -1 = 0xFFFFFFFF in LE
    let mut buf = Vec::new();
    write_int(&mut buf, -1).unwrap();
    assert_eq!(buf, [0xFF, 0xFF, 0xFF, 0xFF]);

    let mut cursor = Cursor::new(&buf);
    assert_eq!(read_int(&mut cursor).unwrap(), -1);
}

#[test]
fn golden_write_int_256() {
    let mut buf = Vec::new();
    write_int(&mut buf, 256).unwrap();
    assert_eq!(buf, [0x00, 0x01, 0x00, 0x00]);

    let mut cursor = Cursor::new(&buf);
    assert_eq!(read_int(&mut cursor).unwrap(), 256);
}

// ---------------------------------------------------------------------------
// Longint encoding (protocol < 30)
// upstream: io.c — write_longint() / read_longint()
// ---------------------------------------------------------------------------

#[test]
fn golden_longint_small_value() {
    // Values 0..=0x7FFFFFFF are encoded as 4-byte LE i32
    let mut buf = Vec::new();
    write_longint(&mut buf, 42).unwrap();
    assert_eq!(buf, [0x2A, 0x00, 0x00, 0x00]);
    assert_eq!(buf.len(), 4);

    let mut cursor = Cursor::new(&buf);
    assert_eq!(read_longint(&mut cursor).unwrap(), 42);
}

#[test]
fn golden_longint_max_small() {
    // 0x7FFFFFFF = i32::MAX is the largest value that fits in 4 bytes
    let mut buf = Vec::new();
    write_longint(&mut buf, 0x7FFF_FFFF).unwrap();
    assert_eq!(buf, [0xFF, 0xFF, 0xFF, 0x7F]);
    assert_eq!(buf.len(), 4);

    let mut cursor = Cursor::new(&buf);
    assert_eq!(read_longint(&mut cursor).unwrap(), 0x7FFF_FFFF);
}

#[test]
fn golden_longint_large_value() {
    // Values > 0x7FFFFFFF use 0xFFFFFFFF marker + 8-byte i64
    // 0x80000000 = 2147483648
    let value: i64 = 0x8000_0000;
    let mut buf = Vec::new();
    write_longint(&mut buf, value).unwrap();

    // First 4 bytes: marker 0xFFFFFFFF
    assert_eq!(&buf[0..4], [0xFF, 0xFF, 0xFF, 0xFF]);
    // Next 8 bytes: value in LE
    assert_eq!(
        &buf[4..12],
        [0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00]
    );
    assert_eq!(buf.len(), 12);

    let mut cursor = Cursor::new(&buf);
    assert_eq!(read_longint(&mut cursor).unwrap(), value);
}

#[test]
fn golden_longint_very_large() {
    // 1 TB = 1099511627776 = 0x10000000000
    let value: i64 = 1_099_511_627_776;
    let mut buf = Vec::new();
    write_longint(&mut buf, value).unwrap();

    assert_eq!(&buf[0..4], [0xFF, 0xFF, 0xFF, 0xFF]);
    assert_eq!(
        &buf[4..12],
        [0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00]
    );
    assert_eq!(buf.len(), 12);

    let mut cursor = Cursor::new(&buf);
    assert_eq!(read_longint(&mut cursor).unwrap(), value);
}

// ---------------------------------------------------------------------------
// Varlong30 encoding (protocol 30+)
// upstream: io.c — write_varlong() / read_varlong() with min_bytes parameter
// ---------------------------------------------------------------------------

#[test]
fn golden_varlong30_zero_min3() {
    // varlong30(0, min_bytes=3): value fits in 3 bytes, leading byte < 0x80
    // leading=0x00, followed by 2 zero bytes
    let mut buf = Vec::new();
    write_varlong30(&mut buf, 0, 3).unwrap();
    assert_eq!(buf, [0x00, 0x00, 0x00]);

    let mut cursor = Cursor::new(&buf);
    assert_eq!(read_varlong30(&mut cursor, 3).unwrap(), 0);
}

#[test]
fn golden_varlong30_small_min3() {
    // varlong30(1024, min_bytes=3): 1024 = 0x000400
    // LE bytes: [0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
    // cnt starts at 8, trims trailing zeros to cnt=2, but min_bytes=3 so cnt=3
    // bit = 1 << (7+3-3) = 0x80
    // bytes[2] = 0x00 < 0x80, and cnt == min_bytes
    // leading = bytes[2] = 0x00
    // output: [0x00] + [0x00, 0x04] => [0x00, 0x00, 0x04]
    let mut buf = Vec::new();
    write_varlong30(&mut buf, 1024, 3).unwrap();
    assert_eq!(buf, [0x00, 0x00, 0x04]);

    let mut cursor = Cursor::new(&buf);
    assert_eq!(read_varlong30(&mut cursor, 3).unwrap(), 1024);
}

#[test]
fn golden_varlong30_medium_min3() {
    // varlong30(16777215, min_bytes=3): 16777215 = 0x00FFFFFF
    // LE bytes: [0xFF, 0xFF, 0xFF, 0x00, ...]
    // cnt trims trailing zeros => cnt=3
    // bit = 1 << (7+3-3) = 0x80
    // bytes[2] = 0xFF >= 0x80, so cnt becomes 4
    // leading = !(0x80 - 1) = !0x7F = 0x80
    // output: [0x80] + bytes[0..3] = [0x80, 0xFF, 0xFF, 0xFF]
    let mut buf = Vec::new();
    write_varlong30(&mut buf, 16_777_215, 3).unwrap();
    assert_eq!(buf, [0x80, 0xFF, 0xFF, 0xFF]);

    let mut cursor = Cursor::new(&buf);
    assert_eq!(read_varlong30(&mut cursor, 3).unwrap(), 16_777_215);
}

#[test]
fn golden_varlong30_unix_timestamp_min3() {
    // A typical Unix timestamp: 1700000000 = 0x6553F100
    // LE bytes: [0x00, 0xF1, 0x53, 0x65, 0x00, ...]
    // cnt trims to 4
    // bit = 1 << (7+3-4) = 0x40
    // bytes[3] = 0x65 >= 0x40, so cnt becomes 5
    // leading = !(0x40 - 1) = !0x3F = 0xC0
    // output: [0xC0] + bytes[0..4] = [0xC0, 0x00, 0xF1, 0x53, 0x65]
    let mut buf = Vec::new();
    write_varlong30(&mut buf, 1_700_000_000, 3).unwrap();
    assert_eq!(buf, [0xC0, 0x00, 0xF1, 0x53, 0x65]);

    let mut cursor = Cursor::new(&buf);
    assert_eq!(read_varlong30(&mut cursor, 3).unwrap(), 1_700_000_000);
}

// ---------------------------------------------------------------------------
// Compatibility flags wire format
// upstream: compat.c — send/recv compatibility flags as varint
// ---------------------------------------------------------------------------

#[test]
fn golden_compat_flags_empty() {
    // Empty flags = varint(0) = single byte 0x00
    let flags = CompatibilityFlags::EMPTY;
    let mut buf = Vec::new();
    flags.encode_to_vec(&mut buf).unwrap();
    assert_eq!(buf, [0x00]);

    let (decoded, rest) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
    assert_eq!(decoded, flags);
    assert!(rest.is_empty());
}

#[test]
fn golden_compat_flags_inc_recurse() {
    // INC_RECURSE = bit 0 = varint(1) = 0x01
    let flags = CompatibilityFlags::INC_RECURSE;
    let mut buf = Vec::new();
    flags.encode_to_vec(&mut buf).unwrap();
    assert_eq!(buf, [0x01]);

    let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
    assert!(decoded.contains(CompatibilityFlags::INC_RECURSE));
}

#[test]
fn golden_compat_flags_safe_flist() {
    // SAFE_FILE_LIST = bit 3 = varint(8) = 0x08
    let flags = CompatibilityFlags::SAFE_FILE_LIST;
    let mut buf = Vec::new();
    flags.encode_to_vec(&mut buf).unwrap();
    assert_eq!(buf, [0x08]);

    let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
    assert!(decoded.contains(CompatibilityFlags::SAFE_FILE_LIST));
}

#[test]
fn golden_compat_flags_typical_server() {
    // A typical rsync 3.4.1 server sends:
    // INC_RECURSE | SYMLINK_TIMES | SAFE_FILE_LIST | CHECKSUM_SEED_FIX
    // = bits 0,1,3,5 = 0b00101011 = 43
    // varint(43) = single byte 0x2B
    let flags = CompatibilityFlags::INC_RECURSE
        | CompatibilityFlags::SYMLINK_TIMES
        | CompatibilityFlags::SAFE_FILE_LIST
        | CompatibilityFlags::CHECKSUM_SEED_FIX;

    assert_eq!(flags.bits(), 43);
    let mut buf = Vec::new();
    flags.encode_to_vec(&mut buf).unwrap();
    assert_eq!(buf, [0x2B]);

    let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
    assert_eq!(decoded, flags);
}

#[test]
fn golden_compat_flags_full_modern() {
    // Full modern flag set with varint flist flags:
    // INC_RECURSE | SYMLINK_TIMES | SAFE_FILE_LIST | CHECKSUM_SEED_FIX |
    // VARINT_FLIST_FLAGS | ID0_NAMES
    // = bits 0,1,3,5,7,8 = 0x1AB = 427
    let flags = CompatibilityFlags::INC_RECURSE
        | CompatibilityFlags::SYMLINK_TIMES
        | CompatibilityFlags::SAFE_FILE_LIST
        | CompatibilityFlags::CHECKSUM_SEED_FIX
        | CompatibilityFlags::VARINT_FLIST_FLAGS
        | CompatibilityFlags::ID0_NAMES;

    assert_eq!(flags.bits(), 0x1AB);

    let mut buf = Vec::new();
    flags.encode_to_vec(&mut buf).unwrap();
    assert_eq!(buf, [0x81, 0xAB]);

    let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
    assert_eq!(decoded, flags);
}

#[test]
fn golden_compat_flags_all_known() {
    // ALL_KNOWN has bits 0..=8 set = 0x1FF = 511
    let flags = CompatibilityFlags::ALL_KNOWN;
    assert_eq!(flags.bits(), 0x1FF);

    let mut buf = Vec::new();
    flags.encode_to_vec(&mut buf).unwrap();
    assert_eq!(buf, [0x81, 0xFF]);

    let (decoded, _) = CompatibilityFlags::decode_from_slice(&buf).unwrap();
    assert_eq!(decoded, flags);
}

// ---------------------------------------------------------------------------
// Transfer stats wire format
// upstream: main.c — output_summary(), stats exchanged via varlong30(min_bytes=3)
// ---------------------------------------------------------------------------

#[test]
fn golden_stats_zero_proto30() {
    // All-zero stats for protocol 30+: 5 varlong30 fields, each 3 bytes of zeros
    let stats = TransferStats::new();
    let mut buf = Vec::new();
    stats.write_to(&mut buf, ProtocolVersion::V30).unwrap();

    // 3 core stats (3 bytes each) + 2 flist times (3 bytes each) = 15 bytes
    assert_eq!(buf.len(), 15);
    assert!(buf.iter().all(|&b| b == 0));

    let mut cursor = Cursor::new(&buf);
    let decoded = TransferStats::read_from(&mut cursor, ProtocolVersion::V30).unwrap();
    assert_eq!(decoded.total_read, 0);
    assert_eq!(decoded.total_written, 0);
    assert_eq!(decoded.total_size, 0);
    assert_eq!(decoded.flist_buildtime, 0);
    assert_eq!(decoded.flist_xfertime, 0);
}

#[test]
fn golden_stats_zero_proto28() {
    // Protocol 28 only sends 3 core stats, no flist times
    let stats = TransferStats::new();
    let mut buf = Vec::new();
    stats.write_to(&mut buf, ProtocolVersion::V28).unwrap();

    // 3 core stats * 3 bytes = 9 bytes
    assert_eq!(buf.len(), 9);
    assert!(buf.iter().all(|&b| b == 0));

    let mut cursor = Cursor::new(&buf);
    let decoded = TransferStats::read_from(&mut cursor, ProtocolVersion::V28).unwrap();
    assert_eq!(decoded.total_read, 0);
    assert_eq!(decoded.total_written, 0);
    assert_eq!(decoded.total_size, 0);
    assert_eq!(decoded.flist_buildtime, 0);
    assert_eq!(decoded.flist_xfertime, 0);
}

#[test]
fn golden_stats_typical_transfer_proto32() {
    let stats = TransferStats::with_bytes(1024, 2048, 10000);
    let mut buf = Vec::new();
    stats.write_to(&mut buf, ProtocolVersion::V32).unwrap();

    let mut cursor = Cursor::new(&buf);
    let decoded = TransferStats::read_from(&mut cursor, ProtocolVersion::V32).unwrap();
    assert_eq!(decoded.total_read, 1024);
    assert_eq!(decoded.total_written, 2048);
    assert_eq!(decoded.total_size, 10000);
}

#[test]
fn golden_stats_with_flist_times_proto30() {
    let stats = TransferStats::with_bytes(5000, 3000, 50000).with_flist_times(500_000, 100_000);
    let mut buf = Vec::new();
    stats.write_to(&mut buf, ProtocolVersion::V30).unwrap();

    let mut cursor = Cursor::new(&buf);
    let decoded = TransferStats::read_from(&mut cursor, ProtocolVersion::V30).unwrap();
    assert_eq!(decoded.total_read, 5000);
    assert_eq!(decoded.total_written, 3000);
    assert_eq!(decoded.total_size, 50000);
    assert_eq!(decoded.flist_buildtime, 500_000);
    assert_eq!(decoded.flist_xfertime, 100_000);
}

#[test]
fn golden_stats_large_values_proto32() {
    // 100 TB read, 50 TB written, 200 TB total
    let stats =
        TransferStats::with_bytes(100_000_000_000_000, 50_000_000_000_000, 200_000_000_000_000)
            .with_flist_times(1_000_000_000, 500_000_000);

    let mut buf = Vec::new();
    stats.write_to(&mut buf, ProtocolVersion::V32).unwrap();

    let mut cursor = Cursor::new(&buf);
    let decoded = TransferStats::read_from(&mut cursor, ProtocolVersion::V32).unwrap();
    assert_eq!(decoded.total_read, 100_000_000_000_000);
    assert_eq!(decoded.total_written, 50_000_000_000_000);
    assert_eq!(decoded.total_size, 200_000_000_000_000);
    assert_eq!(decoded.flist_buildtime, 1_000_000_000);
    assert_eq!(decoded.flist_xfertime, 500_000_000);
}

#[test]
fn golden_stats_swap_perspective() {
    let stats = TransferStats::with_bytes(100, 200, 1000);
    let swapped = stats.swap_perspective();
    assert_eq!(swapped.total_read, 200);
    assert_eq!(swapped.total_written, 100);
    assert_eq!(swapped.total_size, 1000);
}

// ---------------------------------------------------------------------------
// File list XMIT flags encoding
// upstream: rsync.h — XMIT_* constants, also mirrored in wire::file_entry
// ---------------------------------------------------------------------------

#[test]
fn golden_xmit_flag_values() {
    // Verify XMIT flag constants match upstream rsync.h exactly.
    // Primary byte flags (bits 0-7).
    use protocol::wire::file_entry::{
        XMIT_EXTENDED_FLAGS, XMIT_LONG_NAME, XMIT_SAME_GID, XMIT_SAME_MODE, XMIT_SAME_NAME,
        XMIT_SAME_TIME, XMIT_SAME_UID, XMIT_TOP_DIR,
    };

    assert_eq!(XMIT_TOP_DIR, 0x01); // 1 << 0
    assert_eq!(XMIT_SAME_MODE, 0x02); // 1 << 1
    assert_eq!(XMIT_EXTENDED_FLAGS, 0x04); // 1 << 2
    assert_eq!(XMIT_SAME_UID, 0x08); // 1 << 3
    assert_eq!(XMIT_SAME_GID, 0x10); // 1 << 4
    assert_eq!(XMIT_SAME_NAME, 0x20); // 1 << 5
    assert_eq!(XMIT_LONG_NAME, 0x40); // 1 << 6
    assert_eq!(XMIT_SAME_TIME, 0x80); // 1 << 7
}

#[test]
fn golden_xmit_extended_flag_values() {
    // Extended flags (second byte, bits 8-15 of the varint flags word).
    use protocol::wire::file_entry::{
        XMIT_CRTIME_EQ_MTIME, XMIT_GROUP_NAME_FOLLOWS, XMIT_HLINK_FIRST, XMIT_HLINKED,
        XMIT_MOD_NSEC, XMIT_SAME_ATIME, XMIT_SAME_RDEV_MAJOR, XMIT_USER_NAME_FOLLOWS,
    };

    assert_eq!(XMIT_SAME_RDEV_MAJOR, 0x01); // 1 << 0 of extended byte
    assert_eq!(XMIT_HLINKED, 0x02); // 1 << 1 of extended byte
    assert_eq!(XMIT_USER_NAME_FOLLOWS, 0x04); // 1 << 2 (proto 30+)
    assert_eq!(XMIT_GROUP_NAME_FOLLOWS, 0x08); // 1 << 3 (proto 30+)
    assert_eq!(XMIT_HLINK_FIRST, 0x10); // 1 << 4 of extended byte
    assert_eq!(XMIT_MOD_NSEC, 0x20); // 1 << 5 (proto 31+)
    assert_eq!(XMIT_SAME_ATIME, 0x40); // 1 << 6 of extended byte
    assert_eq!(XMIT_CRTIME_EQ_MTIME, 0x02); // 1 << 1 of third byte
}

#[test]
fn golden_xmit_fileflags_from_u32_roundtrip() {
    use protocol::flist::FileFlags;

    // Simulate a varint-encoded flag set with all three flag bytes:
    // primary=0x2A (SAME_MODE | SAME_UID | SAME_NAME)
    // extended=0x05 (SAME_RDEV_MAJOR | USER_NAME_FOLLOWS)
    // extended16=0x02 (CRTIME_EQ_MTIME)
    let value: u32 = 0x02_05_2A;
    let flags = FileFlags::from_u32(value);
    assert_eq!(flags.primary, 0x2A);
    assert_eq!(flags.extended, 0x05);
    assert_eq!(flags.extended16, 0x02);

    assert!(flags.same_mode());
    assert!(flags.same_uid());
    assert!(flags.same_name());
    assert!(!flags.same_gid());
    assert!(flags.same_rdev_major());
    assert!(flags.user_name_follows());
    assert!(flags.crtime_eq_mtime());

    assert_eq!(flags.to_u32(), value);
}

#[test]
fn golden_xmit_file_entry_first_in_list() {
    // The first file entry in a list typically has no "same_*" flags set,
    // since there is no previous entry to share fields with.
    use protocol::flist::FileFlags;

    let flags = FileFlags::default();
    assert!(!flags.same_name());
    assert!(!flags.same_mode());
    assert!(!flags.same_time());
    assert!(!flags.same_uid());
    assert!(!flags.same_gid());
    assert!(!flags.has_extended());
    assert_eq!(flags.to_u32(), 0);
}

// ---------------------------------------------------------------------------
// Varint encoding for file list flags (VARINT_FLIST_FLAGS mode)
// upstream: flist.c — send_file_entry() / recv_file_entry()
// ---------------------------------------------------------------------------

#[test]
fn golden_varint_flist_flags_simple() {
    // In VARINT_FLIST_FLAGS mode, the flags are encoded as a varint.
    // A simple file with XMIT_SAME_MODE (0x02) encodes as varint(2) = 0x02
    let mut buf = Vec::new();
    encode_varint_to_vec(0x02, &mut buf);
    assert_eq!(buf, [0x02]);

    let (val, _) = decode_varint(&buf).unwrap();
    assert_eq!(val, 0x02);
}

#[test]
fn golden_varint_flist_flags_with_extended() {
    // Flags = XMIT_EXTENDED_FLAGS | XMIT_SAME_MODE | XMIT_SAME_TIME
    // with extended byte having XMIT_HLINKED set
    // primary = 0x04 | 0x02 | 0x80 = 0x86
    // extended = 0x02 (HLINKED)
    // as u32 = 0x0286
    let flags_u32: i32 = 0x0286;
    let mut buf = Vec::new();
    encode_varint_to_vec(flags_u32, &mut buf);

    let (val, _) = decode_varint(&buf).unwrap();
    assert_eq!(val, flags_u32);

    use protocol::flist::FileFlags;
    let flags = FileFlags::from_u32(val as u32);
    assert!(flags.same_mode());
    assert!(flags.same_time());
    assert!(flags.has_extended());
    assert!(flags.hlinked());
}

// ---------------------------------------------------------------------------
// File list end-of-list marker
// upstream: flist.c — a zero byte marks end of file list (non-incremental)
// ---------------------------------------------------------------------------

#[test]
fn golden_flist_end_marker() {
    // In non-incremental mode, end of file list is signaled by a zero byte.
    // In varint mode, this is varint(0) = 0x00.
    let mut buf = Vec::new();
    encode_varint_to_vec(0, &mut buf);
    assert_eq!(buf, [0x00]);
}

// ---------------------------------------------------------------------------
// Checksum header (SumHead) wire format
// upstream: match.c / sender.c — sum_head: count, blength, s2length, remainder
// Each field is write_int() = 4-byte LE i32, total 16 bytes
// ---------------------------------------------------------------------------

#[test]
fn golden_sum_head_empty() {
    // Empty SumHead (whole-file transfer): all zeros
    // 4 fields * 4 bytes = 16 bytes, all zero
    let mut buf = Vec::new();
    write_int(&mut buf, 0).unwrap(); // count
    write_int(&mut buf, 0).unwrap(); // blength
    write_int(&mut buf, 0).unwrap(); // s2length
    write_int(&mut buf, 0).unwrap(); // remainder

    assert_eq!(buf.len(), 16);
    assert!(buf.iter().all(|&b| b == 0));

    let mut cursor = Cursor::new(&buf);
    assert_eq!(read_int(&mut cursor).unwrap(), 0); // count
    assert_eq!(read_int(&mut cursor).unwrap(), 0); // blength
    assert_eq!(read_int(&mut cursor).unwrap(), 0); // s2length
    assert_eq!(read_int(&mut cursor).unwrap(), 0); // remainder
}

#[test]
fn golden_sum_head_typical() {
    // Typical SumHead for a 1MB file with 700-byte block size:
    // count=1490, blength=700, s2length=16 (MD5), remainder=200
    let count: i32 = 1490;
    let blength: i32 = 700;
    let s2length: i32 = 16;
    let remainder: i32 = 200;

    let mut buf = Vec::new();
    write_int(&mut buf, count).unwrap();
    write_int(&mut buf, blength).unwrap();
    write_int(&mut buf, s2length).unwrap();
    write_int(&mut buf, remainder).unwrap();

    // Verify exact wire bytes
    assert_eq!(buf.len(), 16);
    // count=1490 = 0x000005D2 LE: [0xD2, 0x05, 0x00, 0x00]
    assert_eq!(&buf[0..4], [0xD2, 0x05, 0x00, 0x00]);
    // blength=700 = 0x000002BC LE: [0xBC, 0x02, 0x00, 0x00]
    assert_eq!(&buf[4..8], [0xBC, 0x02, 0x00, 0x00]);
    // s2length=16 = 0x00000010 LE: [0x10, 0x00, 0x00, 0x00]
    assert_eq!(&buf[8..12], [0x10, 0x00, 0x00, 0x00]);
    // remainder=200 = 0x000000C8 LE: [0xC8, 0x00, 0x00, 0x00]
    assert_eq!(&buf[12..16], [0xC8, 0x00, 0x00, 0x00]);

    // Decode and verify
    let mut cursor = Cursor::new(&buf);
    assert_eq!(read_int(&mut cursor).unwrap(), count);
    assert_eq!(read_int(&mut cursor).unwrap(), blength);
    assert_eq!(read_int(&mut cursor).unwrap(), s2length);
    assert_eq!(read_int(&mut cursor).unwrap(), remainder);
}

// ---------------------------------------------------------------------------
// Multiplex header decode from raw bytes
// upstream: io.c — read_a_msg() reads 4-byte LE header
// ---------------------------------------------------------------------------

#[test]
fn golden_mplex_decode_from_bytes() {
    // Construct wire bytes for MSG_INFO with payload_len=13
    // MSG_INFO=2, tag=2+7=9, raw=(9<<24)|13 = 0x0900000D
    // LE: [0x0D, 0x00, 0x00, 0x09]
    let wire: [u8; 4] = [0x0D, 0x00, 0x00, 0x09];
    let header = MessageHeader::decode(&wire).unwrap();
    assert_eq!(header.code(), MessageCode::Info);
    assert_eq!(header.payload_len(), 13);
}

#[test]
fn golden_mplex_decode_data_frame() {
    // MSG_DATA=0, tag=0+7=7, payload_len=8192
    // raw=(7<<24)|8192 = 0x07002000
    // LE: [0x00, 0x20, 0x00, 0x07]
    let wire: [u8; 4] = [0x00, 0x20, 0x00, 0x07];
    let header = MessageHeader::decode(&wire).unwrap();
    assert_eq!(header.code(), MessageCode::Data);
    assert_eq!(header.payload_len(), 8192);
}

#[test]
fn golden_mplex_decode_error_exit() {
    // MSG_ERROR_EXIT=86, tag=86+7=93=0x5D, payload_len=4
    // raw=(93<<24)|4 = 0x5D000004
    // LE: [0x04, 0x00, 0x00, 0x5D]
    let wire: [u8; 4] = [0x04, 0x00, 0x00, 0x5D];
    let header = MessageHeader::decode(&wire).unwrap();
    assert_eq!(header.code(), MessageCode::ErrorExit);
    assert_eq!(header.payload_len(), 4);
}

#[test]
fn golden_mplex_decode_deleted() {
    // MSG_DELETED=101, tag=101+7=108=0x6C, payload_len=20
    // raw=(108<<24)|20 = 0x6C000014
    // LE: [0x14, 0x00, 0x00, 0x6C]
    let wire: [u8; 4] = [0x14, 0x00, 0x00, 0x6C];
    let header = MessageHeader::decode(&wire).unwrap();
    assert_eq!(header.code(), MessageCode::Deleted);
    assert_eq!(header.payload_len(), 20);
}

#[test]
fn golden_mplex_decode_no_send() {
    // MSG_NO_SEND=102, tag=102+7=109=0x6D, payload_len=4
    // raw=(109<<24)|4 = 0x6D000004
    // LE: [0x04, 0x00, 0x00, 0x6D]
    let wire: [u8; 4] = [0x04, 0x00, 0x00, 0x6D];
    let header = MessageHeader::decode(&wire).unwrap();
    assert_eq!(header.code(), MessageCode::NoSend);
    assert_eq!(header.payload_len(), 4);
}

// ---------------------------------------------------------------------------
// Multiplex header rejects invalid tags
// upstream: tags < MPLEX_BASE (7) are not valid multiplexed headers
// ---------------------------------------------------------------------------

#[test]
fn golden_mplex_reject_tag_below_base() {
    // tag=6 < MPLEX_BASE=7, should fail
    // raw=(6<<24)|0 = 0x06000000
    // LE: [0x00, 0x00, 0x00, 0x06]
    let wire: [u8; 4] = [0x00, 0x00, 0x00, 0x06];
    assert!(MessageHeader::decode(&wire).is_err());
}

#[test]
fn golden_mplex_reject_tag_zero() {
    // tag=0 < MPLEX_BASE=7, should fail
    let wire: [u8; 4] = [0x00, 0x00, 0x00, 0x00];
    assert!(MessageHeader::decode(&wire).is_err());
}

// ---------------------------------------------------------------------------
// Delete stats wire format
// upstream: generator.c — send_delete_stats() uses varint for each count
// ---------------------------------------------------------------------------

#[test]
fn golden_delete_stats_empty() {
    let stats = DeleteStats::new();
    let mut buf = Vec::new();
    stats.write_to(&mut buf).unwrap();

    // 5 varint fields, each varint(0) = 1 byte
    assert_eq!(buf, [0x00, 0x00, 0x00, 0x00, 0x00]);

    let mut cursor = Cursor::new(&buf);
    let decoded = DeleteStats::read_from(&mut cursor).unwrap();
    assert_eq!(decoded, stats);
}

#[test]
fn golden_delete_stats_typical() {
    let stats = DeleteStats {
        files: 10,
        dirs: 3,
        symlinks: 2,
        devices: 0,
        specials: 0,
    };

    let mut buf = Vec::new();
    stats.write_to(&mut buf).unwrap();

    // varint(10)=0x0A, varint(3)=0x03, varint(2)=0x02, varint(0)=0x00, varint(0)=0x00
    assert_eq!(buf, [0x0A, 0x03, 0x02, 0x00, 0x00]);

    let mut cursor = Cursor::new(&buf);
    let decoded = DeleteStats::read_from(&mut cursor).unwrap();
    assert_eq!(decoded, stats);
    assert_eq!(decoded.total(), 15);
}

#[test]
fn golden_delete_stats_large_counts() {
    let stats = DeleteStats {
        files: 1000,
        dirs: 200,
        symlinks: 50,
        devices: 5,
        specials: 1,
    };

    let mut buf = Vec::new();
    stats.write_to(&mut buf).unwrap();

    let mut cursor = Cursor::new(&buf);
    let decoded = DeleteStats::read_from(&mut cursor).unwrap();
    assert_eq!(decoded, stats);
    assert_eq!(decoded.total(), 1256);
}

// ---------------------------------------------------------------------------
// Varint sequence encoding (multiple values in a stream)
// upstream: io.c — sequential varint reads/writes in protocol 30+
// ---------------------------------------------------------------------------

#[test]
fn golden_varint_sequence() {
    // Encode a sequence of values and verify the concatenated byte stream
    let values: &[i32] = &[0, 1, 127, 128, 255, 16384, -1];
    let mut buf = Vec::new();
    for &v in values {
        encode_varint_to_vec(v, &mut buf);
    }

    // Expected byte sequence from the individual test cases above:
    let expected: &[u8] = &[
        0x00, // 0
        0x01, // 1
        0x7F, // 127
        0x80, 0x80, // 128
        0x80, 0xFF, // 255
        0xC0, 0x00, 0x40, // 16384
        0xF0, 0xFF, 0xFF, 0xFF, 0xFF, // -1
    ];
    assert_eq!(buf, expected);

    // Decode the sequence back
    let mut remaining: &[u8] = &buf;
    for &expected_val in values {
        let (val, rest) = decode_varint(remaining).unwrap();
        assert_eq!(val, expected_val);
        remaining = rest;
    }
    assert!(remaining.is_empty());
}

// ---------------------------------------------------------------------------
// Protocol version feature queries
// upstream: compat.c — feature gates based on protocol version
// ---------------------------------------------------------------------------

#[test]
fn golden_protocol_version_features() {
    // Protocol 28: legacy ASCII negotiation, no varint, no flist times
    let v28 = ProtocolVersion::V28;
    assert!(v28.uses_legacy_ascii_negotiation());
    assert!(!v28.uses_varint_encoding());
    assert!(!v28.supports_flist_times());

    // Protocol 29: legacy ASCII, no varint, HAS flist times
    let v29 = ProtocolVersion::V29;
    assert!(v29.uses_legacy_ascii_negotiation());
    assert!(!v29.uses_varint_encoding());
    assert!(v29.supports_flist_times());

    // Protocol 30: binary negotiation, varint encoding, flist times
    let v30 = ProtocolVersion::V30;
    assert!(v30.uses_binary_negotiation());
    assert!(v30.uses_varint_encoding());
    assert!(v30.supports_flist_times());

    // Protocol 31: binary negotiation, varint encoding, flist times
    let v31 = ProtocolVersion::V31;
    assert!(v31.uses_binary_negotiation());
    assert!(v31.uses_varint_encoding());
    assert!(v31.supports_flist_times());

    // Protocol 32: binary negotiation, varint encoding, flist times
    let v32 = ProtocolVersion::V32;
    assert!(v32.uses_binary_negotiation());
    assert!(v32.uses_varint_encoding());
    assert!(v32.supports_flist_times());
}

// ---------------------------------------------------------------------------
// Message code numeric values match upstream enum msgcode
// upstream: rsync.h — enum msgcode
// ---------------------------------------------------------------------------

#[test]
fn golden_message_code_values() {
    // Verify every MessageCode numeric value matches upstream rsync.h
    assert_eq!(MessageCode::Data.as_u8(), 0);
    assert_eq!(MessageCode::ErrorXfer.as_u8(), 1);
    assert_eq!(MessageCode::Info.as_u8(), 2);
    assert_eq!(MessageCode::Error.as_u8(), 3);
    assert_eq!(MessageCode::Warning.as_u8(), 4);
    assert_eq!(MessageCode::ErrorSocket.as_u8(), 5);
    assert_eq!(MessageCode::Log.as_u8(), 6);
    assert_eq!(MessageCode::Client.as_u8(), 7);
    assert_eq!(MessageCode::ErrorUtf8.as_u8(), 8);
    assert_eq!(MessageCode::Redo.as_u8(), 9);
    assert_eq!(MessageCode::Stats.as_u8(), 10);
    assert_eq!(MessageCode::IoError.as_u8(), 22);
    assert_eq!(MessageCode::IoTimeout.as_u8(), 33);
    assert_eq!(MessageCode::NoOp.as_u8(), 42);
    assert_eq!(MessageCode::ErrorExit.as_u8(), 86);
    assert_eq!(MessageCode::Success.as_u8(), 100);
    assert_eq!(MessageCode::Deleted.as_u8(), 101);
    assert_eq!(MessageCode::NoSend.as_u8(), 102);
}

#[test]
fn golden_mplex_base_is_7() {
    // MPLEX_BASE is defined as 7 in upstream rsync.h
    assert_eq!(protocol::MPLEX_BASE, 7);
}

#[test]
fn golden_max_payload_is_24_bit() {
    // Maximum payload in a multiplex header is 2^24 - 1 = 16777215
    assert_eq!(protocol::MAX_PAYLOAD_LENGTH, 0x00FF_FFFF);
    assert_eq!(protocol::MAX_PAYLOAD_LENGTH, 16_777_215);
}

#[test]
fn golden_message_header_len_is_4() {
    // Multiplex headers are exactly 4 bytes
    assert_eq!(protocol::MESSAGE_HEADER_LEN, 4);
}

// ---------------------------------------------------------------------------
// Encoding consistency: varint30_int dispatches correctly by protocol version
// upstream: io.h — write_varint30() / read_varint30() inline wrappers
// ---------------------------------------------------------------------------

#[test]
fn golden_varint30_int_proto29_uses_fixed_int() {
    use protocol::{read_varint30_int, write_varint30_int};

    // Protocol 29: uses fixed 4-byte LE int
    let mut buf = Vec::new();
    write_varint30_int(&mut buf, 42, 29).unwrap();
    assert_eq!(buf, [0x2A, 0x00, 0x00, 0x00]); // 4-byte LE
    assert_eq!(buf.len(), 4);

    let mut cursor = Cursor::new(&buf);
    assert_eq!(read_varint30_int(&mut cursor, 29).unwrap(), 42);
}

#[test]
fn golden_varint30_int_proto30_uses_varint() {
    use protocol::{read_varint30_int, write_varint30_int};

    // Protocol 30: uses varint encoding
    let mut buf = Vec::new();
    write_varint30_int(&mut buf, 42, 30).unwrap();
    assert_eq!(buf, [0x2A]); // 1-byte varint
    assert_eq!(buf.len(), 1);

    let mut cursor = Cursor::new(&buf);
    assert_eq!(read_varint30_int(&mut cursor, 30).unwrap(), 42);
}

#[test]
fn golden_varint30_int_proto32_uses_varint() {
    use protocol::{read_varint30_int, write_varint30_int};

    // Protocol 32: uses varint encoding
    let mut buf = Vec::new();
    write_varint30_int(&mut buf, 1000, 32).unwrap();

    let mut cursor = Cursor::new(&buf);
    assert_eq!(read_varint30_int(&mut cursor, 32).unwrap(), 1000);

    // Verify the varint encoding matches
    let mut expected = Vec::new();
    encode_varint_to_vec(1000, &mut expected);
    assert_eq!(buf, expected);
}

// ---------------------------------------------------------------------------
// Legacy daemon protocol prefix constant
// upstream: clientserver.c — RSYNCD_PREFIX "@RSYNCD: "
// ---------------------------------------------------------------------------

#[test]
fn golden_legacy_daemon_prefix() {
    assert_eq!(protocol::LEGACY_DAEMON_PREFIX, "@RSYNCD:");
    assert_eq!(protocol::LEGACY_DAEMON_PREFIX_BYTES, b"@RSYNCD:");
    assert_eq!(protocol::LEGACY_DAEMON_PREFIX_LEN, 8);
}
