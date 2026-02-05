//! Comprehensive NDX (file-list index) codec tests.
//!
//! This module tests the NDX codec public API for file-list index encoding/decoding.
//! NDX values are used throughout the rsync protocol to reference entries in file lists:
//!
//! - Positive indices reference actual file list entries (0..N)
//! - NDX_DONE (-1) signals completion of file transfer requests
//! - NDX_FLIST_EOF (-2) marks end of file lists in incremental mode
//! - NDX_DEL_STATS (-3) signals delete statistics transmission
//! - NDX_FLIST_OFFSET (-101) marks start of incremental file list indices
//!
//! # Wire Format
//!
//! ## Legacy (Protocol < 30)
//!
//! Simple 4-byte little-endian signed integers for all values.
//!
//! ## Modern (Protocol >= 30)
//!
//! Delta-encoded byte-reduction format:
//! - `0x00`: NDX_DONE (-1)
//! - `0xFF prefix`: negative values (other than -1)
//! - `1-253`: delta-encoded positive index (single byte)
//! - `0xFE prefix`: extended encoding for larger indices
//!
//! # Upstream Reference
//!
//! - `io.c:2243-2287` - `write_ndx()` function
//! - `io.c:2289-2318` - `read_ndx()` function
//! - `rsync.h:285-288` - NDX constant definitions

use protocol::codec::{
    LegacyNdxCodec, ModernNdxCodec, NDX_DEL_STATS, NDX_DONE, NDX_FLIST_EOF, NDX_FLIST_OFFSET,
    NdxCodec, NdxCodecEnum, NdxState, ProtocolCodecs, create_ndx_codec, write_ndx_done,
    write_ndx_flist_eof,
};
use std::io::{self, Cursor, ErrorKind, Read, Write};

// ============================================================================
// Module: NDX Constants Verification
// ============================================================================

mod ndx_constants {
    use super::*;

    /// Verify NDX constant values match rsync upstream definitions.
    #[test]
    fn constants_match_upstream_rsync_definitions() {
        // rsync.h:285 - #define NDX_DONE -1
        assert_eq!(NDX_DONE, -1, "NDX_DONE must be -1 per rsync.h:285");

        // rsync.h:286 - #define NDX_FLIST_EOF -2
        assert_eq!(NDX_FLIST_EOF, -2, "NDX_FLIST_EOF must be -2 per rsync.h:286");

        // rsync.h:287 - #define NDX_DEL_STATS -3
        assert_eq!(NDX_DEL_STATS, -3, "NDX_DEL_STATS must be -3 per rsync.h:287");

        // rsync.h:288 - #define NDX_FLIST_OFFSET -101
        assert_eq!(
            NDX_FLIST_OFFSET, -101,
            "NDX_FLIST_OFFSET must be -101 per rsync.h:288"
        );
    }

    /// Verify NDX constants are negative and properly ordered.
    #[test]
    fn constants_are_properly_ordered() {
        // All special constants are negative
        assert!(NDX_DONE < 0);
        assert!(NDX_FLIST_EOF < 0);
        assert!(NDX_DEL_STATS < 0);
        assert!(NDX_FLIST_OFFSET < 0);

        // NDX_DONE (-1) > NDX_FLIST_EOF (-2) > NDX_DEL_STATS (-3) > NDX_FLIST_OFFSET (-101)
        assert!(NDX_DONE > NDX_FLIST_EOF);
        assert!(NDX_FLIST_EOF > NDX_DEL_STATS);
        assert!(NDX_DEL_STATS > NDX_FLIST_OFFSET);
    }

    /// Verify NDX constants can be distinguished from valid file indices.
    #[test]
    fn constants_do_not_conflict_with_valid_indices() {
        // Valid file indices are non-negative (0, 1, 2, ...)
        // All NDX constants are negative, so no conflict
        for i in 0..1000i32 {
            assert_ne!(i, NDX_DONE);
            assert_ne!(i, NDX_FLIST_EOF);
            assert_ne!(i, NDX_DEL_STATS);
            assert_ne!(i, NDX_FLIST_OFFSET);
        }
    }
}

// ============================================================================
// Module: Legacy Protocol NDX Codec (Protocol < 30)
// ============================================================================

mod legacy_codec {
    use super::*;

    /// Legacy codec should be created for protocols 28-29.
    #[test]
    fn creates_for_protocol_28_29() {
        let codec28 = LegacyNdxCodec::new(28);
        assert_eq!(codec28.protocol_version(), 28);

        let codec29 = LegacyNdxCodec::new(29);
        assert_eq!(codec29.protocol_version(), 29);
    }

    /// Legacy codec should panic for protocol >= 30.
    #[test]
    #[should_panic(expected = "LegacyNdxCodec is for protocol < 30")]
    fn panics_for_protocol_30() {
        let _ = LegacyNdxCodec::new(30);
    }

    /// Legacy codec writes 4-byte little-endian for all values.
    #[test]
    fn writes_4_byte_little_endian() {
        let mut codec = LegacyNdxCodec::new(29);
        let mut buf = Vec::new();

        // Zero
        codec.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf, [0x00, 0x00, 0x00, 0x00]);

        // Positive value
        buf.clear();
        codec.write_ndx(&mut buf, 256).unwrap();
        assert_eq!(buf, [0x00, 0x01, 0x00, 0x00]); // 256 in LE

        // Negative value (NDX_DONE = -1)
        buf.clear();
        codec.write_ndx(&mut buf, NDX_DONE).unwrap();
        assert_eq!(buf, [0xFF, 0xFF, 0xFF, 0xFF]); // -1 in LE two's complement
    }

    /// Legacy codec reads 4-byte little-endian values correctly.
    #[test]
    fn reads_4_byte_little_endian() {
        let mut codec = LegacyNdxCodec::new(29);

        // Zero
        let data = [0x00u8, 0x00, 0x00, 0x00];
        let mut cursor = Cursor::new(&data[..]);
        assert_eq!(codec.read_ndx(&mut cursor).unwrap(), 0);

        // Positive value
        let data = [0x00u8, 0x01, 0x00, 0x00];
        let mut cursor = Cursor::new(&data[..]);
        assert_eq!(codec.read_ndx(&mut cursor).unwrap(), 256);

        // Negative value
        let data = [0xFFu8, 0xFF, 0xFF, 0xFF];
        let mut cursor = Cursor::new(&data[..]);
        assert_eq!(codec.read_ndx(&mut cursor).unwrap(), -1);
    }

    /// Legacy NDX_DONE is written as 4 bytes via write_ndx_done.
    #[test]
    fn write_ndx_done_writes_4_bytes() {
        let mut codec = LegacyNdxCodec::new(28);
        let mut buf = Vec::new();
        codec.write_ndx_done(&mut buf).unwrap();
        assert_eq!(buf, [0xFF, 0xFF, 0xFF, 0xFF]);
    }

    /// Legacy codec round-trips all NDX constants.
    #[test]
    fn roundtrips_all_ndx_constants() {
        let mut codec = LegacyNdxCodec::new(29);
        let constants = [NDX_DONE, NDX_FLIST_EOF, NDX_DEL_STATS, NDX_FLIST_OFFSET];

        let mut buf = Vec::new();
        for &c in &constants {
            codec.write_ndx(&mut buf, c).unwrap();
        }

        let mut cursor = Cursor::new(&buf);
        for &expected in &constants {
            let read = codec.read_ndx(&mut cursor).unwrap();
            assert_eq!(read, expected, "failed for constant {expected}");
        }
    }

    /// Legacy codec round-trips arbitrary positive sequences.
    #[test]
    fn roundtrips_positive_sequence() {
        let mut codec = LegacyNdxCodec::new(29);
        let indices: Vec<i32> = (0..500).collect();

        let mut buf = Vec::new();
        for &idx in &indices {
            codec.write_ndx(&mut buf, idx).unwrap();
        }

        let mut cursor = Cursor::new(&buf);
        for &expected in &indices {
            let read = codec.read_ndx(&mut cursor).unwrap();
            assert_eq!(read, expected);
        }
    }

    /// Legacy codec returns error on truncated input.
    #[test]
    fn returns_error_on_truncated_input() {
        let mut codec = LegacyNdxCodec::new(29);

        // Only 3 bytes when 4 are needed
        let partial = [0x00u8, 0x01, 0x02];
        let mut cursor = Cursor::new(&partial[..]);
        let result = codec.read_ndx(&mut cursor);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), ErrorKind::UnexpectedEof);
    }

    /// Legacy codec returns error on empty input.
    #[test]
    fn returns_error_on_empty_input() {
        let mut codec = LegacyNdxCodec::new(29);
        let empty: [u8; 0] = [];
        let mut cursor = Cursor::new(&empty[..]);
        let result = codec.read_ndx(&mut cursor);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), ErrorKind::UnexpectedEof);
    }

    /// Legacy codec maintains consistent size regardless of value.
    #[test]
    fn always_uses_4_bytes() {
        let mut codec = LegacyNdxCodec::new(29);
        let test_values = [0, 1, 255, 256, 65535, 1_000_000, i32::MAX, -1, -100, i32::MIN];

        for &val in &test_values {
            let mut buf = Vec::new();
            codec.write_ndx(&mut buf, val).unwrap();
            assert_eq!(buf.len(), 4, "value {val} should be 4 bytes, got {}", buf.len());
        }
    }
}

// ============================================================================
// Module: Modern Protocol NDX Codec (Protocol >= 30)
// ============================================================================

mod modern_codec {
    use super::*;

    /// Modern codec should be created for protocols 30+.
    #[test]
    fn creates_for_protocol_30_plus() {
        let codec30 = ModernNdxCodec::new(30);
        assert_eq!(codec30.protocol_version(), 30);

        let codec31 = ModernNdxCodec::new(31);
        assert_eq!(codec31.protocol_version(), 31);

        let codec32 = ModernNdxCodec::new(32);
        assert_eq!(codec32.protocol_version(), 32);
    }

    /// Modern codec should panic for protocol < 30.
    #[test]
    #[should_panic(expected = "ModernNdxCodec is for protocol >= 30")]
    fn panics_for_protocol_29() {
        let _ = ModernNdxCodec::new(29);
    }

    /// Modern codec writes NDX_DONE as single byte 0x00.
    #[test]
    fn writes_ndx_done_as_single_byte() {
        let mut codec = ModernNdxCodec::new(30);
        let mut buf = Vec::new();
        codec.write_ndx(&mut buf, NDX_DONE).unwrap();
        assert_eq!(buf, [0x00]);
    }

    /// Modern codec write_ndx_done helper also writes single byte 0x00.
    #[test]
    fn write_ndx_done_helper_writes_single_byte() {
        let mut codec = ModernNdxCodec::new(30);
        let mut buf = Vec::new();
        codec.write_ndx_done(&mut buf).unwrap();
        assert_eq!(buf, [0x00]);
    }

    /// Modern codec uses delta encoding for sequential indices.
    #[test]
    fn uses_delta_encoding_for_sequential() {
        let mut codec = ModernNdxCodec::new(30);
        let mut buf = Vec::new();

        // First index: 0, prev=-1, diff=1
        codec.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf, [0x01]);

        buf.clear();
        // Second index: 1, prev=0, diff=1
        codec.write_ndx(&mut buf, 1).unwrap();
        assert_eq!(buf, [0x01]);

        buf.clear();
        // Third index: 2, prev=1, diff=1
        codec.write_ndx(&mut buf, 2).unwrap();
        assert_eq!(buf, [0x01]);
    }

    /// Modern codec uses 0xFF prefix for negative values.
    #[test]
    fn uses_0xff_prefix_for_negative_values() {
        let mut codec = ModernNdxCodec::new(30);
        let mut buf = Vec::new();

        // NDX_FLIST_EOF (-2)
        codec.write_ndx(&mut buf, NDX_FLIST_EOF).unwrap();
        assert_eq!(buf[0], 0xFF, "negative values should start with 0xFF prefix");

        buf.clear();
        let mut codec2 = ModernNdxCodec::new(30);
        // NDX_DEL_STATS (-3)
        codec2.write_ndx(&mut buf, NDX_DEL_STATS).unwrap();
        assert_eq!(buf[0], 0xFF);
    }

    /// Modern codec uses 0xFE prefix for extended encoding.
    #[test]
    fn uses_0xfe_prefix_for_extended_encoding() {
        let mut codec = ModernNdxCodec::new(30);
        let mut buf = Vec::new();

        // Force extended encoding: diff=254 (ndx=253 from prev=-1)
        codec.write_ndx(&mut buf, 253).unwrap();
        assert_eq!(buf[0], 0xFE, "diff >= 254 should trigger extended encoding");
    }

    /// Modern codec round-trips all NDX constants.
    #[test]
    fn roundtrips_all_ndx_constants() {
        let mut write_codec = ModernNdxCodec::new(30);
        let constants = [NDX_DONE, NDX_FLIST_EOF, NDX_DEL_STATS, NDX_FLIST_OFFSET];

        let mut buf = Vec::new();
        for &c in &constants {
            write_codec.write_ndx(&mut buf, c).unwrap();
        }

        let mut read_codec = ModernNdxCodec::new(30);
        let mut cursor = Cursor::new(&buf);
        for &expected in &constants {
            let read = read_codec.read_ndx(&mut cursor).unwrap();
            assert_eq!(read, expected, "failed for constant {expected}");
        }
    }

    /// Modern codec round-trips arbitrary positive sequences.
    #[test]
    fn roundtrips_positive_sequence() {
        let mut write_codec = ModernNdxCodec::new(30);
        let indices: Vec<i32> = (0..500).collect();

        let mut buf = Vec::new();
        for &idx in &indices {
            write_codec.write_ndx(&mut buf, idx).unwrap();
        }

        let mut read_codec = ModernNdxCodec::new(30);
        let mut cursor = Cursor::new(&buf);
        for &expected in &indices {
            let read = read_codec.read_ndx(&mut cursor).unwrap();
            assert_eq!(read, expected);
        }
    }

    /// Modern codec returns error on truncated input.
    #[test]
    fn returns_error_on_truncated_input() {
        let mut codec = ModernNdxCodec::new(30);

        // 0xFE prefix but missing extended bytes
        let partial = [0xFEu8, 0x00];
        let mut cursor = Cursor::new(&partial[..]);
        let result = codec.read_ndx(&mut cursor);
        assert!(result.is_err());
    }

    /// Modern codec returns error on empty input.
    #[test]
    fn returns_error_on_empty_input() {
        let mut codec = ModernNdxCodec::new(30);
        let empty: [u8; 0] = [];
        let mut cursor = Cursor::new(&empty[..]);
        let result = codec.read_ndx(&mut cursor);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), ErrorKind::UnexpectedEof);
    }

    /// Modern codec is more compact than legacy for sequential indices.
    #[test]
    fn is_more_compact_than_legacy_for_sequential() {
        let mut modern = ModernNdxCodec::new(30);
        let mut legacy = LegacyNdxCodec::new(29);

        let mut modern_buf = Vec::new();
        let mut legacy_buf = Vec::new();

        // Write 100 sequential indices
        for i in 0..100i32 {
            modern.write_ndx(&mut modern_buf, i).unwrap();
            legacy.write_ndx(&mut legacy_buf, i).unwrap();
        }

        // Modern should be significantly smaller
        assert!(
            modern_buf.len() < legacy_buf.len(),
            "modern ({} bytes) should be smaller than legacy ({} bytes)",
            modern_buf.len(),
            legacy_buf.len()
        );

        // Legacy uses exactly 400 bytes (100 * 4)
        assert_eq!(legacy_buf.len(), 400);

        // Modern uses ~100 bytes (mostly delta=1, single byte each)
        assert!(modern_buf.len() <= 150, "modern should use <=150 bytes for 100 sequential indices");
    }
}

// ============================================================================
// Module: NdxCodecEnum Factory Tests
// ============================================================================

mod codec_enum {
    use super::*;

    /// create_ndx_codec returns legacy for protocol < 30.
    #[test]
    fn factory_returns_legacy_for_protocol_under_30() {
        for version in [28, 29] {
            let codec = create_ndx_codec(version);
            assert_eq!(codec.protocol_version(), version);
            assert!(matches!(codec, NdxCodecEnum::Legacy(_)));
        }
    }

    /// create_ndx_codec returns modern for protocol >= 30.
    #[test]
    fn factory_returns_modern_for_protocol_30_plus() {
        for version in [30, 31, 32] {
            let codec = create_ndx_codec(version);
            assert_eq!(codec.protocol_version(), version);
            assert!(matches!(codec, NdxCodecEnum::Modern(_)));
        }
    }

    /// NdxCodecEnum::new matches factory function behavior.
    #[test]
    fn enum_new_matches_factory() {
        for version in [28, 29, 30, 31, 32] {
            let from_factory = create_ndx_codec(version);
            let from_new = NdxCodecEnum::new(version);
            assert_eq!(from_factory.protocol_version(), from_new.protocol_version());
        }
    }

    /// NdxCodecEnum dispatches correctly to underlying codec.
    #[test]
    fn dispatches_write_correctly() {
        // Legacy via enum
        let mut legacy = NdxCodecEnum::new(29);
        let mut buf = Vec::new();
        legacy.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf.len(), 4, "legacy should write 4 bytes");

        // Modern via enum
        let mut modern = NdxCodecEnum::new(30);
        buf.clear();
        modern.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf.len(), 1, "modern should write 1 byte");
    }

    /// NdxCodecEnum roundtrips correctly for all supported versions.
    #[test]
    fn roundtrips_for_all_versions() {
        let test_values = [0, 1, 100, 1000, NDX_DONE, NDX_FLIST_EOF, NDX_DEL_STATS];

        for version in [28, 29, 30, 31, 32] {
            let mut write_codec = create_ndx_codec(version);
            let mut buf = Vec::new();

            for &val in &test_values {
                write_codec.write_ndx(&mut buf, val).unwrap();
            }

            let mut read_codec = create_ndx_codec(version);
            let mut cursor = Cursor::new(&buf);

            for &expected in &test_values {
                let read = read_codec.read_ndx(&mut cursor).unwrap();
                assert_eq!(read, expected, "v{version}: failed for {expected}");
            }
        }
    }
}

// ============================================================================
// Module: NdxState Legacy API Tests
// ============================================================================

mod ndx_state {
    use super::*;

    /// NdxState::new and NdxState::default produce same initial state.
    #[test]
    fn new_equals_default() {
        let new_state = NdxState::new();
        let default_state = NdxState::default();

        let mut new_buf = Vec::new();
        let mut default_buf = Vec::new();

        let mut n = new_state.clone();
        let mut d = default_state.clone();

        // Both should produce same output for same input
        n.write_ndx(&mut new_buf, 0).unwrap();
        d.write_ndx(&mut default_buf, 0).unwrap();

        assert_eq!(new_buf, default_buf);
    }

    /// write_ndx_done helper writes single byte 0x00.
    #[test]
    fn write_ndx_done_helper() {
        let mut buf = Vec::new();
        write_ndx_done(&mut buf).unwrap();
        assert_eq!(buf, [0x00]);
    }

    /// write_ndx_flist_eof helper writes correctly.
    #[test]
    fn write_ndx_flist_eof_helper() {
        let mut buf = Vec::new();
        let mut state = NdxState::new();
        write_ndx_flist_eof(&mut buf, &mut state).unwrap();
        // NDX_FLIST_EOF (-2): -(-2)=2, diff=2-1=1, so 0xFF prefix + 0x01
        assert_eq!(buf, [0xFF, 0x01]);
    }

    /// NdxState clone produces independent state.
    #[test]
    fn clone_produces_independent_state() {
        let mut state = NdxState::new();
        let mut buf = Vec::new();
        state.write_ndx(&mut buf, 100).unwrap();

        let mut cloned = state.clone();
        let mut state_buf = Vec::new();
        let mut cloned_buf = Vec::new();

        // Both should produce same output for same next input
        state.write_ndx(&mut state_buf, 200).unwrap();
        cloned.write_ndx(&mut cloned_buf, 200).unwrap();

        assert_eq!(state_buf, cloned_buf);
    }
}

// ============================================================================
// Module: ProtocolCodecs Container Tests
// ============================================================================

mod protocol_codecs_container {
    use super::*;

    /// ProtocolCodecs creates matching wire and ndx codecs.
    #[test]
    fn creates_matching_codecs() {
        for version in [28, 29, 30, 31, 32] {
            let codecs = ProtocolCodecs::for_version(version);
            assert_eq!(codecs.protocol_version(), version);
            assert_eq!(codecs.ndx.protocol_version(), version);
        }
    }

    /// ProtocolCodecs is_legacy matches version boundary.
    #[test]
    fn is_legacy_matches_version_boundary() {
        assert!(ProtocolCodecs::for_version(28).is_legacy());
        assert!(ProtocolCodecs::for_version(29).is_legacy());
        assert!(!ProtocolCodecs::for_version(30).is_legacy());
        assert!(!ProtocolCodecs::for_version(31).is_legacy());
        assert!(!ProtocolCodecs::for_version(32).is_legacy());
    }

    /// ProtocolCodecs ndx codec roundtrips correctly.
    #[test]
    fn ndx_codec_roundtrips() {
        let mut codecs = ProtocolCodecs::for_version(30);
        let mut buf = Vec::new();

        codecs.ndx.write_ndx(&mut buf, 0).unwrap();
        codecs.ndx.write_ndx(&mut buf, 1).unwrap();
        codecs.ndx.write_ndx(&mut buf, NDX_DONE).unwrap();

        let mut read_codecs = ProtocolCodecs::for_version(30);
        let mut cursor = Cursor::new(&buf);

        assert_eq!(read_codecs.ndx.read_ndx(&mut cursor).unwrap(), 0);
        assert_eq!(read_codecs.ndx.read_ndx(&mut cursor).unwrap(), 1);
        assert_eq!(read_codecs.ndx.read_ndx(&mut cursor).unwrap(), NDX_DONE);
    }
}

// ============================================================================
// Module: Delta Encoding Boundary Tests
// ============================================================================

mod delta_encoding_boundaries {
    use super::*;

    /// Single-byte delta encoding for diff 1-253.
    #[test]
    fn single_byte_for_diff_1_to_253() {
        let mut codec = ModernNdxCodec::new(30);

        // diff=1 (ndx=0 from prev=-1)
        let mut buf = Vec::new();
        codec.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0], 0x01);

        // diff=253 (ndx=252 from prev=-1, fresh codec)
        let mut codec2 = ModernNdxCodec::new(30);
        buf.clear();
        codec2.write_ndx(&mut buf, 252).unwrap();
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0], 253);
    }

    /// Extended 2-byte encoding for diff 254-32767.
    #[test]
    fn two_byte_for_diff_254_to_32767() {
        let mut codec = ModernNdxCodec::new(30);

        // diff=254 (ndx=253 from prev=-1)
        let mut buf = Vec::new();
        codec.write_ndx(&mut buf, 253).unwrap();
        assert_eq!(buf[0], 0xFE);
        assert_eq!(buf.len(), 3); // 0xFE + 2 bytes

        // diff=32767 (ndx=32766 from prev=-1, fresh codec)
        let mut codec2 = ModernNdxCodec::new(30);
        buf.clear();
        codec2.write_ndx(&mut buf, 32766).unwrap();
        assert_eq!(buf[0], 0xFE);
        // High bit not set means 2-byte diff
        assert!(buf[1] & 0x80 == 0);
    }

    /// Extended 4-byte encoding for diff > 32767 or negative diff.
    #[test]
    fn four_byte_for_large_diff() {
        let mut codec = ModernNdxCodec::new(30);

        // Large value requiring full encoding
        let mut buf = Vec::new();
        codec.write_ndx(&mut buf, 0x01_00_00_00).unwrap();
        assert_eq!(buf[0], 0xFE);
        // High bit set means 4-byte value
        assert!(buf[1] & 0x80 != 0);
        assert_eq!(buf.len(), 5); // 0xFE + 4 bytes
    }

    /// Zero diff triggers extended encoding to avoid collision with NDX_DONE.
    #[test]
    fn zero_diff_uses_extended_encoding() {
        let mut codec = ModernNdxCodec::new(30);
        let mut buf = Vec::new();

        // First write: ndx=0, diff=1
        codec.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf[0], 0x01);

        buf.clear();
        // Second write: ndx=0 again, diff=0 (same value)
        // Zero diff must use extended encoding to avoid 0x00 which means NDX_DONE
        codec.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf[0], 0xFE, "zero diff should use 0xFE prefix");
    }

    /// Negative diff (decreasing index) uses extended encoding.
    #[test]
    fn negative_diff_uses_extended_encoding() {
        let mut codec = ModernNdxCodec::new(30);
        let mut buf = Vec::new();

        // Write 100
        codec.write_ndx(&mut buf, 100).unwrap();
        buf.clear();

        // Write 50 (diff = 50 - 100 = -50)
        codec.write_ndx(&mut buf, 50).unwrap();
        assert_eq!(buf[0], 0xFE, "negative diff should use 0xFE prefix");
    }
}

// ============================================================================
// Module: State Tracking Tests
// ============================================================================

mod state_tracking {
    use super::*;

    /// NDX_DONE does not affect prev_positive state.
    #[test]
    fn ndx_done_does_not_affect_positive_state() {
        let mut write_codec = ModernNdxCodec::new(30);
        let mut buf = Vec::new();

        // Write: 0, 1, NDX_DONE, 2
        write_codec.write_ndx(&mut buf, 0).unwrap();
        write_codec.write_ndx(&mut buf, 1).unwrap();
        write_codec.write_ndx(&mut buf, NDX_DONE).unwrap();
        write_codec.write_ndx(&mut buf, 2).unwrap();

        let mut read_codec = ModernNdxCodec::new(30);
        let mut cursor = Cursor::new(&buf);

        assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), 0);
        assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), 1);
        assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), NDX_DONE);
        assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), 2);
    }

    /// Positive and negative sequences maintain separate state.
    #[test]
    fn separate_state_for_positive_and_negative() {
        let mut write_codec = ModernNdxCodec::new(30);
        let mut buf = Vec::new();

        // Interleave positive and negative values
        let sequence = [
            0,               // positive
            NDX_FLIST_EOF,   // negative (-2)
            5,               // positive
            NDX_DEL_STATS,   // negative (-3)
            10,              // positive
        ];

        for &val in &sequence {
            write_codec.write_ndx(&mut buf, val).unwrap();
        }

        let mut read_codec = ModernNdxCodec::new(30);
        let mut cursor = Cursor::new(&buf);

        for &expected in &sequence {
            let read = read_codec.read_ndx(&mut cursor).unwrap();
            assert_eq!(read, expected);
        }
    }

    /// Multiple negative values use delta encoding correctly.
    #[test]
    fn negative_sequence_uses_delta() {
        let mut write_codec = ModernNdxCodec::new(30);
        let mut buf = Vec::new();

        // Write multiple negative values in sequence
        write_codec.write_ndx(&mut buf, NDX_FLIST_EOF).unwrap();    // -2
        write_codec.write_ndx(&mut buf, NDX_DEL_STATS).unwrap();    // -3
        write_codec.write_ndx(&mut buf, NDX_FLIST_OFFSET).unwrap(); // -101

        let mut read_codec = ModernNdxCodec::new(30);
        let mut cursor = Cursor::new(&buf);

        assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), NDX_FLIST_EOF);
        assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), NDX_DEL_STATS);
        assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), NDX_FLIST_OFFSET);
    }

    /// Codec instances have independent state.
    #[test]
    fn independent_codec_instances() {
        let mut codec1 = ModernNdxCodec::new(30);
        let mut codec2 = ModernNdxCodec::new(30);

        let mut buf1 = Vec::new();
        let mut buf2 = Vec::new();

        // codec1: 0, 1, 2
        codec1.write_ndx(&mut buf1, 0).unwrap();
        codec1.write_ndx(&mut buf1, 1).unwrap();
        codec1.write_ndx(&mut buf1, 2).unwrap();

        // codec2: 100, 101 (independent state)
        codec2.write_ndx(&mut buf2, 100).unwrap();
        codec2.write_ndx(&mut buf2, 101).unwrap();

        // Verify each reads back correctly
        let mut read1 = ModernNdxCodec::new(30);
        let mut cursor1 = Cursor::new(&buf1);
        assert_eq!(read1.read_ndx(&mut cursor1).unwrap(), 0);
        assert_eq!(read1.read_ndx(&mut cursor1).unwrap(), 1);
        assert_eq!(read1.read_ndx(&mut cursor1).unwrap(), 2);

        let mut read2 = ModernNdxCodec::new(30);
        let mut cursor2 = Cursor::new(&buf2);
        assert_eq!(read2.read_ndx(&mut cursor2).unwrap(), 100);
        assert_eq!(read2.read_ndx(&mut cursor2).unwrap(), 101);
    }
}

// ============================================================================
// Module: Wire Format Verification Tests
// ============================================================================

mod wire_format {
    use super::*;

    /// Legacy wire format byte patterns match upstream.
    #[test]
    fn legacy_byte_patterns() {
        let mut codec = LegacyNdxCodec::new(29);
        let mut buf = Vec::new();

        // 0 as 4-byte LE
        codec.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf, [0x00, 0x00, 0x00, 0x00]);

        // 0x12345678 as 4-byte LE
        buf.clear();
        codec.write_ndx(&mut buf, 0x12345678).unwrap();
        assert_eq!(buf, [0x78, 0x56, 0x34, 0x12]);

        // -1 (NDX_DONE) as 4-byte LE
        buf.clear();
        codec.write_ndx(&mut buf, -1).unwrap();
        assert_eq!(buf, [0xFF, 0xFF, 0xFF, 0xFF]);

        // -2 (NDX_FLIST_EOF) as 4-byte LE
        buf.clear();
        codec.write_ndx(&mut buf, -2).unwrap();
        assert_eq!(buf, [0xFE, 0xFF, 0xFF, 0xFF]);
    }

    /// Modern wire format byte patterns match upstream.
    #[test]
    fn modern_byte_patterns() {
        let mut codec = ModernNdxCodec::new(30);
        let mut buf = Vec::new();

        // NDX_DONE (-1) is always 0x00
        codec.write_ndx(&mut buf, NDX_DONE).unwrap();
        assert_eq!(buf, [0x00]);

        // First positive (0): diff=1 from prev=-1
        buf.clear();
        let mut codec2 = ModernNdxCodec::new(30);
        codec2.write_ndx(&mut buf, 0).unwrap();
        assert_eq!(buf, [0x01]);

        // NDX_FLIST_EOF (-2): 0xFF prefix + delta
        buf.clear();
        let mut codec3 = ModernNdxCodec::new(30);
        codec3.write_ndx(&mut buf, NDX_FLIST_EOF).unwrap();
        assert_eq!(buf[0], 0xFF);
    }

    /// Modern sequential indices produce all 0x01 bytes.
    #[test]
    fn sequential_indices_are_all_0x01() {
        let mut codec = ModernNdxCodec::new(30);
        let mut buf = Vec::new();

        for i in 0..10i32 {
            codec.write_ndx(&mut buf, i).unwrap();
        }

        // All 10 sequential indices should be delta 1 = 0x01
        assert_eq!(buf, vec![0x01; 10]);
    }
}

// ============================================================================
// Module: Error Handling Tests
// ============================================================================

mod error_handling {
    use super::*;

    /// Write errors are propagated correctly.
    #[test]
    fn write_error_propagation() {
        struct FailWriter;
        impl Write for FailWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(ErrorKind::BrokenPipe, "test error"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut legacy = LegacyNdxCodec::new(29);
        let result = legacy.write_ndx(&mut FailWriter, 0);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), ErrorKind::BrokenPipe);

        let mut modern = ModernNdxCodec::new(30);
        let result = modern.write_ndx(&mut FailWriter, 0);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), ErrorKind::BrokenPipe);
    }

    /// Read errors for truncated 4-byte extended encoding.
    #[test]
    fn truncated_4byte_extended_encoding() {
        let mut codec = ModernNdxCodec::new(30);
        // 0xFE prefix, high bit set (4-byte mode), but only 2 extra bytes
        let truncated = [0xFEu8, 0x80, 0x00, 0x00];
        let mut cursor = Cursor::new(&truncated[..]);
        let result = codec.read_ndx(&mut cursor);
        assert!(result.is_err());
    }

    /// Read errors for truncated 2-byte extended encoding.
    #[test]
    fn truncated_2byte_extended_encoding() {
        let mut codec = ModernNdxCodec::new(30);
        // 0xFE prefix, but only 1 extra byte instead of 2
        let truncated = [0xFEu8, 0x00];
        let mut cursor = Cursor::new(&truncated[..]);
        let result = codec.read_ndx(&mut cursor);
        assert!(result.is_err());
    }

    /// Read errors for truncated negative prefix.
    #[test]
    fn truncated_negative_prefix() {
        let mut codec = ModernNdxCodec::new(30);
        // 0xFF prefix for negative, but missing delta byte
        let truncated = [0xFFu8];
        let mut cursor = Cursor::new(&truncated[..]);
        let result = codec.read_ndx(&mut cursor);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), ErrorKind::UnexpectedEof);
    }
}

// ============================================================================
// Module: Extreme Value Tests
// ============================================================================

mod extreme_values {
    use super::*;

    /// Roundtrip large values (within codec limits).
    /// Note: i32::MAX is not supported by the NDX codec due to encoding constraints.
    #[test]
    fn roundtrips_large_values() {
        // NDX codec uses variable-length encoding with special markers
        // Values up to ~16 million are typically supported
        let large_values = [
            1_000_000,
            10_000_000,
            16_000_000,
        ];
        for version in [28, 29, 30, 31, 32] {
            for &value in &large_values {
                let mut write_codec = create_ndx_codec(version);
                let mut buf = Vec::new();
                write_codec.write_ndx(&mut buf, value).unwrap();

                let mut read_codec = create_ndx_codec(version);
                let mut cursor = Cursor::new(&buf);
                let read = read_codec.read_ndx(&mut cursor).unwrap();
                assert_eq!(read, value, "v{version}: {value} roundtrip failed");
            }
        }
    }

    /// Roundtrip boundary values.
    #[test]
    fn roundtrips_boundary_values() {
        let boundary_values = [
            0,
            1,
            127,
            128,
            253,
            254,
            255,
            256,
            32767,
            32768,
            65535,
            65536,
            0x00FF_FFFF, // 24-bit max
            0x7FFF_FFFF, // i32::MAX
        ];

        for version in [28, 29, 30, 31, 32] {
            let mut write_codec = create_ndx_codec(version);
            let mut buf = Vec::new();

            for &val in &boundary_values {
                write_codec.write_ndx(&mut buf, val).unwrap();
            }

            let mut read_codec = create_ndx_codec(version);
            let mut cursor = Cursor::new(&buf);

            for &expected in &boundary_values {
                let read = read_codec.read_ndx(&mut cursor).unwrap();
                assert_eq!(read, expected, "v{version}: {expected} roundtrip failed");
            }
        }
    }

    /// Roundtrip with large gaps between values.
    #[test]
    fn roundtrips_large_gaps() {
        let sparse_values: Vec<i32> = (0..50).map(|i| i * 10000).collect();

        for version in [28, 29, 30, 31, 32] {
            let mut write_codec = create_ndx_codec(version);
            let mut buf = Vec::new();

            for &val in &sparse_values {
                write_codec.write_ndx(&mut buf, val).unwrap();
            }

            let mut read_codec = create_ndx_codec(version);
            let mut cursor = Cursor::new(&buf);

            for &expected in &sparse_values {
                let read = read_codec.read_ndx(&mut cursor).unwrap();
                assert_eq!(read, expected, "v{version}: sparse {expected} roundtrip failed");
            }
        }
    }
}

// ============================================================================
// Module: Real-World Usage Pattern Tests
// ============================================================================

mod usage_patterns {
    use super::*;

    /// Simulate file transfer request sequence.
    #[test]
    fn file_transfer_request_sequence() {
        for version in [28, 29, 30, 31, 32] {
            let mut write_codec = create_ndx_codec(version);
            let mut buf = Vec::new();

            // Simulate requesting files 0, 1, 2, then signaling done
            write_codec.write_ndx(&mut buf, 0).unwrap();
            write_codec.write_ndx(&mut buf, 1).unwrap();
            write_codec.write_ndx(&mut buf, 2).unwrap();
            write_codec.write_ndx(&mut buf, NDX_DONE).unwrap();

            let mut read_codec = create_ndx_codec(version);
            let mut cursor = Cursor::new(&buf);

            assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), 0);
            assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), 1);
            assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), 2);
            assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), NDX_DONE);
        }
    }

    /// Simulate incremental file list sequence.
    #[test]
    fn incremental_file_list_sequence() {
        for version in [30, 31, 32] { // Incremental is protocol 30+
            let mut write_codec = create_ndx_codec(version);
            let mut buf = Vec::new();

            // First batch of files
            write_codec.write_ndx(&mut buf, 0).unwrap();
            write_codec.write_ndx(&mut buf, 1).unwrap();
            write_codec.write_ndx(&mut buf, NDX_DONE).unwrap();

            // Signal end of file lists
            write_codec.write_ndx(&mut buf, NDX_FLIST_EOF).unwrap();

            let mut read_codec = create_ndx_codec(version);
            let mut cursor = Cursor::new(&buf);

            assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), 0);
            assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), 1);
            assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), NDX_DONE);
            assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), NDX_FLIST_EOF);
        }
    }

    /// Simulate delete statistics sequence.
    #[test]
    fn delete_statistics_sequence() {
        for version in [28, 29, 30, 31, 32] {
            let mut write_codec = create_ndx_codec(version);
            let mut buf = Vec::new();

            // Signal delete stats transmission
            write_codec.write_ndx(&mut buf, NDX_DEL_STATS).unwrap();

            let mut read_codec = create_ndx_codec(version);
            let mut cursor = Cursor::new(&buf);

            assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), NDX_DEL_STATS);
        }
    }

    /// Mixed positive and negative value sequences (common in rsync).
    #[test]
    fn mixed_sequence() {
        for version in [28, 29, 30, 31, 32] {
            let mut write_codec = create_ndx_codec(version);
            let mut buf = Vec::new();

            let sequence = [
                0,
                1,
                NDX_DONE,
                5,
                10,
                NDX_DONE,
                NDX_FLIST_EOF,
            ];

            for &val in &sequence {
                write_codec.write_ndx(&mut buf, val).unwrap();
            }

            let mut read_codec = create_ndx_codec(version);
            let mut cursor = Cursor::new(&buf);

            for &expected in &sequence {
                let read = read_codec.read_ndx(&mut cursor).unwrap();
                assert_eq!(read, expected, "v{version}: mixed sequence failed at {expected}");
            }
        }
    }
}

// ============================================================================
// Module: Property-Based Tests
// ============================================================================

mod property_tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    /// Deterministic pseudo-random generator for tests.
    fn pseudo_random(seed: u64, i: usize) -> i32 {
        let mut hasher = DefaultHasher::new();
        (seed, i).hash(&mut hasher);
        (hasher.finish() % 10000) as i32
    }

    /// Roundtrip pseudo-random positive sequence.
    #[test]
    fn roundtrip_pseudo_random_sequence() {
        let seed = 42u64;
        let count = 200;

        for version in [28, 29, 30, 31, 32] {
            let mut write_codec = create_ndx_codec(version);
            let mut buf = Vec::new();

            for i in 0..count {
                let val = pseudo_random(seed, i);
                write_codec.write_ndx(&mut buf, val).unwrap();
            }

            let mut read_codec = create_ndx_codec(version);
            let mut cursor = Cursor::new(&buf);

            for i in 0..count {
                let expected = pseudo_random(seed, i);
                let read = read_codec.read_ndx(&mut cursor).unwrap();
                assert_eq!(read, expected, "v{version}: failed at index {i}");
            }
        }
    }

    /// Monotonically increasing sequence (common in rsync file lists).
    #[test]
    fn monotonic_sequence() {
        for version in [28, 29, 30, 31, 32] {
            let mut write_codec = create_ndx_codec(version);
            let mut buf = Vec::new();

            for i in 0..500i32 {
                write_codec.write_ndx(&mut buf, i).unwrap();
            }

            let mut read_codec = create_ndx_codec(version);
            let mut cursor = Cursor::new(&buf);

            for i in 0..500i32 {
                let read = read_codec.read_ndx(&mut cursor).unwrap();
                assert_eq!(read, i, "v{version}: monotonic failed at {i}");
            }
        }
    }

    /// Non-monotonic sequence with varying gaps.
    #[test]
    fn non_monotonic_sequence() {
        let sequence: Vec<i32> = vec![
            0, 5, 3, 10, 7, 100, 50, 200, 150, 1000, 500, 999, 998, 997
        ];

        for version in [28, 29, 30, 31, 32] {
            let mut write_codec = create_ndx_codec(version);
            let mut buf = Vec::new();

            for &val in &sequence {
                write_codec.write_ndx(&mut buf, val).unwrap();
            }

            let mut read_codec = create_ndx_codec(version);
            let mut cursor = Cursor::new(&buf);

            for &expected in &sequence {
                let read = read_codec.read_ndx(&mut cursor).unwrap();
                assert_eq!(read, expected, "v{version}: non-monotonic failed at {expected}");
            }
        }
    }
}

// ============================================================================
// Module: Cross-Version Compatibility Tests
// ============================================================================

mod cross_version {
    use super::*;

    /// All versions produce readable output for NDX constants.
    #[test]
    fn all_versions_roundtrip_constants() {
        let constants = [NDX_DONE, NDX_FLIST_EOF, NDX_DEL_STATS, NDX_FLIST_OFFSET];

        for version in [28, 29, 30, 31, 32] {
            let mut write_codec = create_ndx_codec(version);
            let mut buf = Vec::new();

            for &c in &constants {
                write_codec.write_ndx(&mut buf, c).unwrap();
            }

            let mut read_codec = create_ndx_codec(version);
            let mut cursor = Cursor::new(&buf);

            for &expected in &constants {
                let read = read_codec.read_ndx(&mut cursor).unwrap();
                assert_eq!(read, expected, "v{version}: constant {expected} failed");
            }
        }
    }

    /// Wire format differs at protocol boundary (v29 vs v30).
    #[test]
    fn wire_format_differs_at_boundary() {
        let mut legacy = create_ndx_codec(29);
        let mut modern = create_ndx_codec(30);

        let mut legacy_buf = Vec::new();
        let mut modern_buf = Vec::new();

        legacy.write_ndx(&mut legacy_buf, 0).unwrap();
        modern.write_ndx(&mut modern_buf, 0).unwrap();

        // Wire formats should be different
        assert_ne!(
            legacy_buf, modern_buf,
            "legacy and modern should use different wire formats"
        );

        // Legacy: 4 bytes
        assert_eq!(legacy_buf.len(), 4);
        // Modern: 1 byte
        assert_eq!(modern_buf.len(), 1);
    }

    /// Same semantic value across versions.
    #[test]
    fn same_semantic_value_across_versions() {
        // All versions should decode to the same semantic value
        for version in [28, 29, 30, 31, 32] {
            let mut codec = create_ndx_codec(version);
            let mut buf = Vec::new();
            codec.write_ndx(&mut buf, 42).unwrap();

            let mut read_codec = create_ndx_codec(version);
            let mut cursor = Cursor::new(&buf);
            let read = read_codec.read_ndx(&mut cursor).unwrap();
            assert_eq!(read, 42, "v{version}: semantic value 42 failed");
        }
    }
}
