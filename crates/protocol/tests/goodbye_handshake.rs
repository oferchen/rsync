//! Goodbye handshake wire format tests for all supported protocol versions.
//!
//! The goodbye handshake is the final exchange at the end of an rsync transfer.
//! Its wire format differs between protocol versions:
//!
//! - **Protocol 28-29 (Legacy)**: NDX_DONE is sent as a 4-byte little-endian
//!   signed integer (`0xFF 0xFF 0xFF 0xFF`), matching upstream's `write_int(-1)`.
//! - **Protocol 30+ (Modern)**: NDX_DONE is sent as a single byte (`0x00`),
//!   using the modern varint NDX encoding.
//!
//! The goodbye exchange itself varies by protocol version:
//!
//! - **Protocol < 24**: No goodbye handshake at all.
//! - **Protocol 24-30**: Simple one-way NDX_DONE exchange.
//! - **Protocol 31+**: Extended 3-way exchange with optional NDX_DEL_STATS.
//!
//! # Upstream Reference
//!
//! - `main.c:875-906` - `read_final_goodbye()`
//! - `main.c:883` - protocol < 29 uses `read_int(f_in)` for goodbye
//! - `main.c:885-886` - protocol >= 29 uses `read_ndx_and_attrs()`
//! - `io.c:2243-2287` - `write_ndx()` (modern encoding)
//! - `io.c` - `write_int()` / `read_int()` (legacy encoding)

use protocol::ProtocolVersion;
use protocol::codec::{
    NDX_DONE, NDX_DONE_LEGACY_BYTES, NDX_DONE_MODERN_BYTE, NdxCodec, create_ndx_codec,
    read_goodbye, write_goodbye, write_ndx_done,
};
use std::io::Cursor;

/// Legacy goodbye wire format (protocol 28-29).
mod legacy_wire_format {
    use super::*;

    /// Legacy NDX_DONE is exactly 4 bytes of 0xFF (i32 -1 in little-endian).
    ///
    /// upstream: io.c write_int() writes 4-byte LE
    #[test]
    fn ndx_done_legacy_bytes_constant_is_minus_one_le() {
        assert_eq!(NDX_DONE_LEGACY_BYTES, (-1i32).to_le_bytes());
        assert_eq!(NDX_DONE_LEGACY_BYTES, [0xFF, 0xFF, 0xFF, 0xFF]);
    }

    /// `write_goodbye` for protocol 28 writes 4-byte LE NDX_DONE.
    #[test]
    fn write_goodbye_proto28_writes_4_bytes() {
        let mut buf = Vec::new();
        write_goodbye(&mut buf, 28).unwrap();
        assert_eq!(buf, NDX_DONE_LEGACY_BYTES);
        assert_eq!(buf.len(), 4);
    }

    /// `write_goodbye` for protocol 29 writes 4-byte LE NDX_DONE.
    #[test]
    fn write_goodbye_proto29_writes_4_bytes() {
        let mut buf = Vec::new();
        write_goodbye(&mut buf, 29).unwrap();
        assert_eq!(buf, NDX_DONE_LEGACY_BYTES);
        assert_eq!(buf.len(), 4);
    }

    /// `read_goodbye` for protocol 28 reads and validates 4-byte LE NDX_DONE.
    #[test]
    fn read_goodbye_proto28_accepts_valid() {
        let mut cursor = Cursor::new(NDX_DONE_LEGACY_BYTES.to_vec());
        read_goodbye(&mut cursor, 28).unwrap();
        assert_eq!(cursor.position(), 4);
    }

    /// `read_goodbye` for protocol 29 reads and validates 4-byte LE NDX_DONE.
    #[test]
    fn read_goodbye_proto29_accepts_valid() {
        let mut cursor = Cursor::new(NDX_DONE_LEGACY_BYTES.to_vec());
        read_goodbye(&mut cursor, 29).unwrap();
        assert_eq!(cursor.position(), 4);
    }

    /// `read_goodbye` rejects non-NDX_DONE values for protocol 28.
    #[test]
    fn read_goodbye_proto28_rejects_wrong_value() {
        let data = 5i32.to_le_bytes().to_vec();
        let mut cursor = Cursor::new(data);
        let err = read_goodbye(&mut cursor, 28).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        let msg = err.to_string();
        assert!(msg.contains("expected goodbye NDX_DONE"));
        assert!(msg.contains("got 5"));
    }

    /// `read_goodbye` rejects non-NDX_DONE values for protocol 29.
    #[test]
    fn read_goodbye_proto29_rejects_wrong_value() {
        let data = 0i32.to_le_bytes().to_vec();
        let mut cursor = Cursor::new(data);
        let err = read_goodbye(&mut cursor, 29).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        let msg = err.to_string();
        assert!(msg.contains("expected goodbye NDX_DONE"));
        assert!(msg.contains("got 0"));
    }

    /// `read_goodbye` fails on truncated input for legacy protocol.
    #[test]
    fn read_goodbye_legacy_fails_on_truncated_input() {
        // Only 2 bytes instead of 4
        let mut cursor = Cursor::new(vec![0xFF, 0xFF]);
        let err = read_goodbye(&mut cursor, 28).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    /// Legacy NDX codec `write_ndx_done` matches `write_goodbye` output.
    #[test]
    fn codec_write_ndx_done_matches_write_goodbye() {
        let mut codec = create_ndx_codec(28);
        let mut codec_buf = Vec::new();
        codec.write_ndx_done(&mut codec_buf).unwrap();

        let mut goodbye_buf = Vec::new();
        write_goodbye(&mut goodbye_buf, 28).unwrap();

        assert_eq!(codec_buf, goodbye_buf);
    }

    /// Legacy NDX codec `write_ndx(NDX_DONE)` produces the same bytes as `write_goodbye`.
    #[test]
    fn codec_write_ndx_with_ndx_done_matches_write_goodbye() {
        let mut codec = create_ndx_codec(29);
        let mut codec_buf = Vec::new();
        codec.write_ndx(&mut codec_buf, NDX_DONE).unwrap();

        let mut goodbye_buf = Vec::new();
        write_goodbye(&mut goodbye_buf, 29).unwrap();

        assert_eq!(codec_buf, goodbye_buf);
    }

    /// Write-then-read roundtrip for protocol 28.
    #[test]
    fn roundtrip_proto28() {
        let mut buf = Vec::new();
        write_goodbye(&mut buf, 28).unwrap();

        let mut cursor = Cursor::new(buf);
        read_goodbye(&mut cursor, 28).unwrap();
    }

    /// Write-then-read roundtrip for protocol 29.
    #[test]
    fn roundtrip_proto29() {
        let mut buf = Vec::new();
        write_goodbye(&mut buf, 29).unwrap();

        let mut cursor = Cursor::new(buf);
        read_goodbye(&mut cursor, 29).unwrap();
    }
}

/// Modern goodbye wire format (protocol 30+).
mod modern_wire_format {
    use super::*;

    /// Modern NDX_DONE byte is 0x00.
    #[test]
    fn ndx_done_modern_byte_constant_is_zero() {
        assert_eq!(NDX_DONE_MODERN_BYTE, 0x00);
    }

    /// `write_goodbye` for protocol 30 writes single byte 0x00.
    #[test]
    fn write_goodbye_proto30_writes_1_byte() {
        let mut buf = Vec::new();
        write_goodbye(&mut buf, 30).unwrap();
        assert_eq!(buf, [0x00]);
        assert_eq!(buf.len(), 1);
    }

    /// `write_goodbye` for protocol 32 writes single byte 0x00.
    #[test]
    fn write_goodbye_proto32_writes_1_byte() {
        let mut buf = Vec::new();
        write_goodbye(&mut buf, 32).unwrap();
        assert_eq!(buf, [0x00]);
        assert_eq!(buf.len(), 1);
    }

    /// `read_goodbye` for protocol 30 reads and validates the 0x00 byte.
    #[test]
    fn read_goodbye_proto30_accepts_valid() {
        let mut cursor = Cursor::new(vec![0x00]);
        read_goodbye(&mut cursor, 30).unwrap();
        assert_eq!(cursor.position(), 1);
    }

    /// `read_goodbye` rejects non-zero bytes for protocol 30+.
    #[test]
    fn read_goodbye_proto30_rejects_wrong_value() {
        let mut cursor = Cursor::new(vec![0x01]);
        let err = read_goodbye(&mut cursor, 30).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        let msg = err.to_string();
        assert!(msg.contains("expected goodbye NDX_DONE (0x00)"));
        assert!(msg.contains("0x01"));
    }

    /// `read_goodbye` fails on empty input for modern protocol.
    #[test]
    fn read_goodbye_modern_fails_on_empty_input() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let err = read_goodbye(&mut cursor, 32).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    /// Modern NDX codec `write_ndx_done` matches `write_goodbye` output.
    #[test]
    fn codec_write_ndx_done_matches_write_goodbye() {
        let mut codec = create_ndx_codec(30);
        let mut codec_buf = Vec::new();
        codec.write_ndx_done(&mut codec_buf).unwrap();

        let mut goodbye_buf = Vec::new();
        write_goodbye(&mut goodbye_buf, 30).unwrap();

        assert_eq!(codec_buf, goodbye_buf);
    }

    /// Standalone `write_ndx_done` matches `write_goodbye` for protocol 30+.
    #[test]
    fn standalone_write_ndx_done_matches_write_goodbye() {
        let mut standalone_buf = Vec::new();
        write_ndx_done(&mut standalone_buf).unwrap();

        let mut goodbye_buf = Vec::new();
        write_goodbye(&mut goodbye_buf, 30).unwrap();

        assert_eq!(standalone_buf, goodbye_buf);
    }

    /// Write-then-read roundtrip for protocol 30.
    #[test]
    fn roundtrip_proto30() {
        let mut buf = Vec::new();
        write_goodbye(&mut buf, 30).unwrap();

        let mut cursor = Cursor::new(buf);
        read_goodbye(&mut cursor, 30).unwrap();
    }

    /// Write-then-read roundtrip for protocol 32.
    #[test]
    fn roundtrip_proto32() {
        let mut buf = Vec::new();
        write_goodbye(&mut buf, 32).unwrap();

        let mut cursor = Cursor::new(buf);
        read_goodbye(&mut cursor, 32).unwrap();
    }
}

/// Cross-version wire format divergence tests.
mod cross_version {
    use super::*;

    /// Legacy and modern goodbye have different wire sizes.
    #[test]
    fn legacy_is_4_bytes_modern_is_1_byte() {
        let mut legacy_buf = Vec::new();
        write_goodbye(&mut legacy_buf, 29).unwrap();

        let mut modern_buf = Vec::new();
        write_goodbye(&mut modern_buf, 30).unwrap();

        assert_eq!(legacy_buf.len(), 4);
        assert_eq!(modern_buf.len(), 1);
    }

    /// Legacy and modern goodbye produce different byte patterns.
    #[test]
    fn legacy_and_modern_bytes_differ() {
        let mut legacy_buf = Vec::new();
        write_goodbye(&mut legacy_buf, 29).unwrap();

        let mut modern_buf = Vec::new();
        write_goodbye(&mut modern_buf, 30).unwrap();

        assert_ne!(legacy_buf, modern_buf);
    }

    /// Protocol 30 is the boundary between legacy and modern encoding.
    ///
    /// upstream: NDX codec uses 4-byte LE for proto < 30, varint for proto >= 30
    #[test]
    fn boundary_at_protocol_30() {
        // Protocol 29: legacy (4 bytes)
        let mut buf_29 = Vec::new();
        write_goodbye(&mut buf_29, 29).unwrap();
        assert_eq!(buf_29.len(), 4);
        assert_eq!(buf_29, [0xFF, 0xFF, 0xFF, 0xFF]);

        // Protocol 30: modern (1 byte)
        let mut buf_30 = Vec::new();
        write_goodbye(&mut buf_30, 30).unwrap();
        assert_eq!(buf_30.len(), 1);
        assert_eq!(buf_30, [0x00]);
    }

    /// All supported protocol versions produce valid goodbye roundtrips.
    #[test]
    fn roundtrip_all_supported_versions() {
        for version in 28..=32u8 {
            let mut buf = Vec::new();
            write_goodbye(&mut buf, version).unwrap();

            let mut cursor = Cursor::new(buf.clone());
            read_goodbye(&mut cursor, version).unwrap();

            // Verify expected size
            if version < 30 {
                assert_eq!(buf.len(), 4, "proto {version} should use 4-byte goodbye");
            } else {
                assert_eq!(buf.len(), 1, "proto {version} should use 1-byte goodbye");
            }
        }
    }

    /// Reading a legacy goodbye with a modern reader fails.
    #[test]
    fn legacy_goodbye_rejected_by_modern_reader() {
        let mut buf = Vec::new();
        write_goodbye(&mut buf, 28).unwrap();

        // First byte of legacy NDX_DONE is 0xFF, not 0x00
        let mut cursor = Cursor::new(vec![buf[0]]);
        let err = read_goodbye(&mut cursor, 30).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    /// Reading a modern goodbye with a legacy reader fails.
    #[test]
    fn modern_goodbye_rejected_by_legacy_reader() {
        let mut buf = Vec::new();
        write_goodbye(&mut buf, 30).unwrap();

        // Pad to 4 bytes so read_exact succeeds, but value is 0 not -1
        let padded = vec![0x00, 0x00, 0x00, 0x00];
        let mut cursor = Cursor::new(padded);
        let err = read_goodbye(&mut cursor, 28).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("got 0"));
    }

    /// Codec-based write_ndx_done matches write_goodbye for all versions.
    #[test]
    fn codec_matches_standalone_for_all_versions() {
        for version in 28..=32u8 {
            let mut codec = create_ndx_codec(version);
            let mut codec_buf = Vec::new();
            codec.write_ndx_done(&mut codec_buf).unwrap();

            let mut goodbye_buf = Vec::new();
            write_goodbye(&mut goodbye_buf, version).unwrap();

            assert_eq!(
                codec_buf, goodbye_buf,
                "codec and write_goodbye should produce identical bytes for proto {version}"
            );
        }
    }
}

/// Protocol version feature gate tests for goodbye support.
mod feature_gates {
    use super::*;

    /// Protocol 28 supports goodbye but not extended goodbye.
    #[test]
    fn proto28_goodbye_support() {
        let v28 = ProtocolVersion::try_from(28u8).unwrap();
        assert!(v28.supports_goodbye_exchange());
        assert!(!v28.supports_extended_goodbye());
    }

    /// Protocol 29 supports goodbye but not extended goodbye.
    #[test]
    fn proto29_goodbye_support() {
        let v29 = ProtocolVersion::try_from(29u8).unwrap();
        assert!(v29.supports_goodbye_exchange());
        assert!(!v29.supports_extended_goodbye());
    }

    /// Protocol 30 supports goodbye but not extended goodbye.
    #[test]
    fn proto30_goodbye_support() {
        let v30 = ProtocolVersion::try_from(30u8).unwrap();
        assert!(v30.supports_goodbye_exchange());
        assert!(!v30.supports_extended_goodbye());
    }

    /// Protocol 31 supports both goodbye and extended goodbye.
    #[test]
    fn proto31_goodbye_support() {
        let v31 = ProtocolVersion::try_from(31u8).unwrap();
        assert!(v31.supports_goodbye_exchange());
        assert!(v31.supports_extended_goodbye());
    }

    /// Protocol 32 supports both goodbye and extended goodbye.
    #[test]
    fn proto32_goodbye_support() {
        let v32 = ProtocolVersion::try_from(32u8).unwrap();
        assert!(v32.supports_goodbye_exchange());
        assert!(v32.supports_extended_goodbye());
    }
}
