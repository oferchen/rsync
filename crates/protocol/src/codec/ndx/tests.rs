use std::io::{self, Cursor};

use super::*;

#[test]
fn test_legacy_codec_writes_4_byte_le() {
    let mut codec = LegacyNdxCodec::new(29);
    let mut buf = Vec::new();

    codec.write_ndx(&mut buf, 5).unwrap();
    assert_eq!(buf, vec![5, 0, 0, 0], "positive index should be 4-byte LE");

    buf.clear();
    codec.write_ndx(&mut buf, 0).unwrap();
    assert_eq!(buf, vec![0, 0, 0, 0], "zero should be 4-byte LE");

    buf.clear();
    codec.write_ndx(&mut buf, 1000).unwrap();
    assert_eq!(
        buf,
        vec![0xE8, 0x03, 0x00, 0x00],
        "1000 should be 4-byte LE"
    );
}

#[test]
fn test_legacy_codec_writes_ndx_done_as_4_bytes() {
    let mut codec = LegacyNdxCodec::new(28);
    let mut buf = Vec::new();

    codec.write_ndx_done(&mut buf).unwrap();
    assert_eq!(
        buf,
        vec![0xFF, 0xFF, 0xFF, 0xFF],
        "NDX_DONE should be -1 as 4-byte LE"
    );
}

#[test]
fn test_legacy_codec_reads_4_byte_le() {
    let mut codec = LegacyNdxCodec::new(29);

    let data = vec![5u8, 0, 0, 0];
    let mut cursor = Cursor::new(&data);
    assert_eq!(codec.read_ndx(&mut cursor).unwrap(), 5);

    let data = vec![0xFFu8, 0xFF, 0xFF, 0xFF];
    let mut cursor = Cursor::new(&data);
    assert_eq!(codec.read_ndx(&mut cursor).unwrap(), NDX_DONE);

    let data = vec![0xE8u8, 0x03, 0x00, 0x00];
    let mut cursor = Cursor::new(&data);
    assert_eq!(codec.read_ndx(&mut cursor).unwrap(), 1000);
}

#[test]
fn test_legacy_codec_roundtrip() {
    let mut codec = LegacyNdxCodec::new(29);
    let values = [0, 1, 5, 100, 1000, 50000, NDX_DONE, NDX_FLIST_EOF];

    let mut buf = Vec::new();
    for &v in &values {
        codec.write_ndx(&mut buf, v).unwrap();
    }

    let mut cursor = Cursor::new(&buf);
    for &expected in &values {
        assert_eq!(codec.read_ndx(&mut cursor).unwrap(), expected);
    }
}

#[test]
fn test_modern_codec_writes_delta_encoded() {
    let mut codec = ModernNdxCodec::new(32);
    let mut buf = Vec::new();

    // First positive: prev=-1, ndx=0, diff=1
    codec.write_ndx(&mut buf, 0).unwrap();
    assert_eq!(buf, vec![0x01], "first index 0 should be delta 1");

    buf.clear();
    // Second: prev=0, ndx=1, diff=1
    codec.write_ndx(&mut buf, 1).unwrap();
    assert_eq!(buf, vec![0x01], "sequential index should be delta 1");

    buf.clear();
    // Third: prev=1, ndx=5, diff=4
    codec.write_ndx(&mut buf, 5).unwrap();
    assert_eq!(buf, vec![0x04], "index 5 should be delta 4");
}

#[test]
fn test_modern_codec_writes_ndx_done_as_single_byte() {
    let mut codec = ModernNdxCodec::new(30);
    let mut buf = Vec::new();

    codec.write_ndx_done(&mut buf).unwrap();
    assert_eq!(buf, vec![0x00], "NDX_DONE should be single byte 0x00");
}

#[test]
fn test_modern_codec_roundtrip() {
    let mut write_codec = ModernNdxCodec::new(32);
    let mut buf = Vec::new();

    for ndx in [0, 1, 2, 5, 100, 253, 254, 500, 10000] {
        write_codec.write_ndx(&mut buf, ndx).unwrap();
    }

    let mut cursor = Cursor::new(&buf);
    let mut read_codec = ModernNdxCodec::new(32);

    for expected in [0, 1, 2, 5, 100, 253, 254, 500, 10000] {
        assert_eq!(read_codec.read_ndx(&mut cursor).unwrap(), expected);
    }
}

#[test]
fn test_create_ndx_codec_selects_correct_implementation() {
    let legacy = create_ndx_codec(29);
    assert_eq!(legacy.protocol_version(), 29);

    let modern = create_ndx_codec(32);
    assert_eq!(modern.protocol_version(), 32);
}

#[test]
fn test_codec_factory_protocol_boundary() {
    let mut codec29 = create_ndx_codec(29);
    let mut buf = Vec::new();
    codec29.write_ndx(&mut buf, 5).unwrap();
    assert_eq!(buf.len(), 4, "protocol 29 should use 4-byte format");

    let mut codec30 = create_ndx_codec(30);
    buf.clear();
    codec30.write_ndx(&mut buf, 0).unwrap();
    assert_eq!(buf.len(), 1, "protocol 30 should use delta format");
}

#[test]
#[should_panic(expected = "LegacyNdxCodec is for protocol < 30")]
fn test_legacy_codec_panics_for_protocol_30() {
    let _ = LegacyNdxCodec::new(30);
}

#[test]
#[should_panic(expected = "ModernNdxCodec is for protocol >= 30")]
fn test_modern_codec_panics_for_protocol_29() {
    let _ = ModernNdxCodec::new(29);
}

#[test]
fn test_ndx_done_encoding() {
    let mut buf = Vec::new();
    let mut state = NdxState::new();
    state.write_ndx(&mut buf, NDX_DONE).unwrap();
    assert_eq!(buf, vec![0x00]);
}

#[test]
fn test_ndx_flist_eof_encoding() {
    let mut buf = Vec::new();
    let mut state = NdxState::new();
    state.write_ndx(&mut buf, NDX_FLIST_EOF).unwrap();
    assert_eq!(buf, vec![0xFF, 0x01]);
}

#[test]
fn test_positive_index_first() {
    let mut buf = Vec::new();
    let mut state = NdxState::new();
    state.write_ndx(&mut buf, 0).unwrap();
    assert_eq!(buf, vec![0x01]);
}

#[test]
fn test_positive_index_sequence() {
    let mut buf = Vec::new();
    let mut state = NdxState::new();

    state.write_ndx(&mut buf, 0).unwrap();
    assert_eq!(buf, vec![0x01]);

    buf.clear();
    state.write_ndx(&mut buf, 1).unwrap();
    assert_eq!(buf, vec![0x01]);

    buf.clear();
    state.write_ndx(&mut buf, 5).unwrap();
    assert_eq!(buf, vec![0x04]);
}

#[test]
fn test_roundtrip_positive() {
    let mut buf = Vec::new();
    let mut write_state = NdxState::new();

    for ndx in [0, 1, 2, 5, 100, 253, 254, 500, 10000, 50000] {
        write_state.write_ndx(&mut buf, ndx).unwrap();
    }

    let mut cursor = Cursor::new(&buf);
    let mut read_state = NdxState::new();

    for expected in [0, 1, 2, 5, 100, 253, 254, 500, 10000, 50000] {
        let ndx = read_state.read_ndx(&mut cursor).unwrap();
        assert_eq!(ndx, expected);
    }
}

#[test]
fn test_roundtrip_negative() {
    let mut buf = Vec::new();
    let mut write_state = NdxState::new();

    state_write_ndx(&mut write_state, &mut buf, NDX_DONE).unwrap();
    state_write_ndx(&mut write_state, &mut buf, NDX_FLIST_EOF).unwrap();
    state_write_ndx(&mut write_state, &mut buf, NDX_DEL_STATS).unwrap();

    let mut cursor = Cursor::new(&buf);
    let mut read_state = NdxState::new();

    assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), NDX_DONE);
    assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), NDX_FLIST_EOF);
    assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), NDX_DEL_STATS);
}

fn state_write_ndx(state: &mut NdxState, buf: &mut Vec<u8>, ndx: i32) -> std::io::Result<()> {
    state.write_ndx(buf, ndx)
}

#[test]
fn test_ndx_done_roundtrip() {
    let mut buf = Vec::new();
    write_ndx_done(&mut buf).unwrap();
    assert_eq!(buf, vec![0x00]);

    let mut cursor = Cursor::new(&buf);
    let mut state = NdxState::new();
    assert_eq!(state.read_ndx(&mut cursor).unwrap(), NDX_DONE);
}

#[test]
fn test_large_index_encoding() {
    let mut buf = Vec::new();
    let mut state = NdxState::new();

    state.write_ndx(&mut buf, 253).unwrap();
    assert_eq!(buf[0], 0xFE);
    assert_eq!(buf.len(), 3);
}

#[test]
fn test_very_large_index_encoding() {
    let mut buf = Vec::new();
    let mut state = NdxState::new();

    let large_ndx = 0x01_00_00_00;
    state.write_ndx(&mut buf, large_ndx).unwrap();

    assert_eq!(buf[0], 0xFE);
    assert!(buf[1] & 0x80 != 0);
    assert_eq!(buf.len(), 5);
}

#[test]
fn test_sign_transition_positive_to_negative_to_positive() {
    let mut buf = Vec::new();
    let mut write_state = NdxState::new();

    write_state.write_ndx(&mut buf, 0).unwrap();
    write_state.write_ndx(&mut buf, 1).unwrap();
    write_state.write_ndx(&mut buf, 2).unwrap();
    write_state.write_ndx(&mut buf, NDX_DONE).unwrap();
    write_state.write_ndx(&mut buf, 3).unwrap();
    write_state.write_ndx(&mut buf, 4).unwrap();

    let mut cursor = Cursor::new(&buf);
    let mut read_state = NdxState::new();

    assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), 0);
    assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), 1);
    assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), 2);
    assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), NDX_DONE);
    assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), 3);
    assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), 4);
}

#[test]
fn test_alternating_positive_negative_values() {
    let mut buf = Vec::new();
    let mut write_state = NdxState::new();

    let sequence = [0, NDX_DONE, 5, NDX_FLIST_EOF, 10, NDX_DEL_STATS, 15];

    for &val in &sequence {
        write_state.write_ndx(&mut buf, val).unwrap();
    }

    let mut cursor = Cursor::new(&buf);
    let mut read_state = NdxState::new();

    for &expected in &sequence {
        assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), expected);
    }
}

#[test]
fn test_negative_sequence_tracks_prev_negative() {
    let mut buf = Vec::new();
    let mut write_state = NdxState::new();

    write_state.write_ndx(&mut buf, NDX_FLIST_EOF).unwrap();
    write_state.write_ndx(&mut buf, NDX_DEL_STATS).unwrap();
    write_state.write_ndx(&mut buf, NDX_FLIST_OFFSET).unwrap();

    let mut cursor = Cursor::new(&buf);
    let mut read_state = NdxState::new();

    assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), NDX_FLIST_EOF);
    assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), NDX_DEL_STATS);
    assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), NDX_FLIST_OFFSET);
}

#[test]
fn test_codec_instances_have_independent_state() {
    let mut codec1 = ModernNdxCodec::new(32);
    let mut codec2 = ModernNdxCodec::new(32);

    let mut buf1 = Vec::new();
    let mut buf2 = Vec::new();

    codec1.write_ndx(&mut buf1, 0).unwrap();
    codec1.write_ndx(&mut buf1, 1).unwrap();
    codec1.write_ndx(&mut buf1, 2).unwrap();

    codec2.write_ndx(&mut buf2, 100).unwrap();
    codec2.write_ndx(&mut buf2, 101).unwrap();

    let mut cursor1 = Cursor::new(&buf1);
    let mut read_codec1 = ModernNdxCodec::new(32);
    assert_eq!(read_codec1.read_ndx(&mut cursor1).unwrap(), 0);
    assert_eq!(read_codec1.read_ndx(&mut cursor1).unwrap(), 1);
    assert_eq!(read_codec1.read_ndx(&mut cursor1).unwrap(), 2);

    let mut cursor2 = Cursor::new(&buf2);
    let mut read_codec2 = ModernNdxCodec::new(32);
    assert_eq!(read_codec2.read_ndx(&mut cursor2).unwrap(), 100);
    assert_eq!(read_codec2.read_ndx(&mut cursor2).unwrap(), 101);
}

#[test]
fn test_delta_boundary_at_253() {
    let mut buf = Vec::new();
    let mut state = NdxState::new();

    state.write_ndx(&mut buf, 252).unwrap();
    assert_eq!(buf.len(), 1, "diff=253 should be single byte");
    assert_eq!(buf[0], 253);
}

#[test]
fn test_delta_boundary_at_254() {
    let mut buf = Vec::new();
    let mut state = NdxState::new();

    state.write_ndx(&mut buf, 253).unwrap();
    assert_eq!(buf[0], 0xFE, "diff=254 needs 0xFE prefix");
    assert_eq!(buf.len(), 3, "diff=254 should be 3 bytes (prefix + 2)");
}

#[test]
fn test_large_gap_encoding() {
    let mut buf = Vec::new();
    let mut write_state = NdxState::new();

    write_state.write_ndx(&mut buf, 100).unwrap();
    buf.clear();

    write_state.write_ndx(&mut buf, 50000).unwrap();

    assert_eq!(buf[0], 0xFE, "large gap needs extended encoding");

    let mut write_state2 = NdxState::new();
    let mut buf2 = Vec::new();
    write_state2.write_ndx(&mut buf2, 100).unwrap();
    write_state2.write_ndx(&mut buf2, 50000).unwrap();

    let mut cursor = Cursor::new(&buf2);
    let mut read_state = NdxState::new();
    assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), 100);
    assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), 50000);
}

#[test]
fn test_4byte_encoding_for_very_large_diff() {
    let mut buf = Vec::new();
    let mut state = NdxState::new();

    state.write_ndx(&mut buf, 40000).unwrap();

    assert_eq!(buf[0], 0xFE, "large diff needs 0xFE prefix");
    assert!(buf[1] & 0x80 != 0, "4-byte format should have high bit set");
    assert_eq!(buf.len(), 5, "4-byte format: prefix + 4 bytes");
}

#[test]
fn test_all_negative_constants() {
    let mut buf = Vec::new();
    let mut write_state = NdxState::new();

    let negatives = [NDX_DONE, NDX_FLIST_EOF, NDX_DEL_STATS, NDX_FLIST_OFFSET];

    for &val in &negatives {
        write_state.write_ndx(&mut buf, val).unwrap();
    }

    let mut cursor = Cursor::new(&buf);
    let mut read_state = NdxState::new();

    for &expected in &negatives {
        assert_eq!(
            read_state.read_ndx(&mut cursor).unwrap(),
            expected,
            "failed for constant {expected}"
        );
    }
}

#[test]
fn test_ndx_flist_offset_roundtrip() {
    let mut buf = Vec::new();
    let mut write_state = NdxState::new();

    write_state.write_ndx(&mut buf, NDX_FLIST_OFFSET).unwrap();

    let mut cursor = Cursor::new(&buf);
    let mut read_state = NdxState::new();

    assert_eq!(read_state.read_ndx(&mut cursor).unwrap(), NDX_FLIST_OFFSET);
}

#[test]
fn test_all_versions_create_valid_ndx_codecs() {
    for version in 28..=32 {
        let codec = create_ndx_codec(version);
        assert_eq!(codec.protocol_version(), version);
    }
}

#[test]
fn test_version_boundary_at_30_for_ndx() {
    let mut legacy = create_ndx_codec(29);
    let mut legacy_buf = Vec::new();
    legacy.write_ndx(&mut legacy_buf, 0).unwrap();
    assert_eq!(legacy_buf.len(), 4);

    let mut modern = create_ndx_codec(30);
    let mut modern_buf = Vec::new();
    modern.write_ndx(&mut modern_buf, 0).unwrap();
    assert_eq!(modern_buf.len(), 1);
}

#[test]
fn test_legacy_ndx_upstream_byte_patterns() {
    let mut codec = LegacyNdxCodec::new(29);

    let mut buf = Vec::new();
    codec.write_ndx(&mut buf, 0).unwrap();
    assert_eq!(buf, [0x00, 0x00, 0x00, 0x00]);

    buf.clear();
    codec.write_ndx(&mut buf, 255).unwrap();
    assert_eq!(buf, [0xff, 0x00, 0x00, 0x00]);

    buf.clear();
    codec.write_ndx(&mut buf, NDX_DONE).unwrap();
    assert_eq!(buf, [0xff, 0xff, 0xff, 0xff]);

    buf.clear();
    codec.write_ndx(&mut buf, NDX_FLIST_EOF).unwrap();
    assert_eq!(buf, [0xfe, 0xff, 0xff, 0xff]);
}

#[test]
fn test_modern_ndx_done_is_single_byte_zero() {
    let mut codec = ModernNdxCodec::new(30);
    let mut buf = Vec::new();
    codec.write_ndx(&mut buf, NDX_DONE).unwrap();
    assert_eq!(buf, [0x00]);
}

#[test]
fn test_modern_ndx_first_positive_is_delta_one() {
    let mut codec = ModernNdxCodec::new(30);
    let mut buf = Vec::new();
    codec.write_ndx(&mut buf, 0).unwrap();
    assert_eq!(buf, [0x01]);
}

#[test]
fn test_modern_ndx_sequential_indices() {
    let mut codec = ModernNdxCodec::new(30);
    let mut buf = Vec::new();

    for ndx in 0..5 {
        codec.write_ndx(&mut buf, ndx).unwrap();
    }

    assert_eq!(buf, [0x01, 0x01, 0x01, 0x01, 0x01]);
}

#[test]
fn test_modern_ndx_negative_prefix_0xff() {
    let mut codec = ModernNdxCodec::new(30);
    let mut buf = Vec::new();

    codec.write_ndx(&mut buf, NDX_FLIST_EOF).unwrap();
    assert_eq!(buf[0], 0xFF);

    buf.clear();
    let mut codec2 = ModernNdxCodec::new(30);
    codec2.write_ndx(&mut buf, NDX_DEL_STATS).unwrap();
    assert_eq!(buf[0], 0xFF);
}

#[test]
fn test_legacy_ndx_read_truncated() {
    let mut codec = LegacyNdxCodec::new(29);
    let truncated = [0u8, 0, 0];
    let mut cursor = Cursor::new(&truncated[..]);
    assert!(codec.read_ndx(&mut cursor).is_err());
}

#[test]
fn test_modern_ndx_read_truncated_extended() {
    let mut codec = ModernNdxCodec::new(30);

    let truncated = [0xFE, 0x00];
    let mut cursor = Cursor::new(&truncated[..]);
    assert!(codec.read_ndx(&mut cursor).is_err());
}

#[test]
fn test_modern_ndx_read_truncated_4byte() {
    let mut codec = ModernNdxCodec::new(30);

    let truncated = [0xFE, 0x80, 0x00, 0x00];
    let mut cursor = Cursor::new(&truncated[..]);
    assert!(codec.read_ndx(&mut cursor).is_err());
}

#[test]
fn test_empty_input_returns_error() {
    let mut legacy = LegacyNdxCodec::new(29);
    let mut modern = ModernNdxCodec::new(30);
    let empty: [u8; 0] = [];

    let mut cursor = Cursor::new(&empty[..]);
    assert!(legacy.read_ndx(&mut cursor).is_err());

    let mut cursor = Cursor::new(&empty[..]);
    assert!(modern.read_ndx(&mut cursor).is_err());
}

#[test]
fn test_all_versions_roundtrip_ndx_constants() {
    let constants = [NDX_DONE, NDX_FLIST_EOF, NDX_DEL_STATS, NDX_FLIST_OFFSET];

    for version in [28, 29, 30, 31, 32] {
        let mut write_codec = create_ndx_codec(version);
        let mut buf = Vec::new();

        for &ndx in &constants {
            write_codec.write_ndx(&mut buf, ndx).unwrap();
        }

        let mut read_codec = create_ndx_codec(version);
        let mut cursor = Cursor::new(&buf);

        for &expected in &constants {
            let read = read_codec.read_ndx(&mut cursor).unwrap();
            assert_eq!(read, expected, "v{version} roundtrip failed for {expected}");
        }
    }
}

#[test]
fn test_all_versions_roundtrip_positive_sequence() {
    let indices: Vec<i32> = (0..100).collect();

    for version in [28, 29, 30, 31, 32] {
        let mut write_codec = create_ndx_codec(version);
        let mut buf = Vec::new();

        for &ndx in &indices {
            write_codec.write_ndx(&mut buf, ndx).unwrap();
        }

        let mut read_codec = create_ndx_codec(version);
        let mut cursor = Cursor::new(&buf);

        for &expected in &indices {
            let read = read_codec.read_ndx(&mut cursor).unwrap();
            assert_eq!(
                read, expected,
                "v{version} roundtrip failed for index {expected}"
            );
        }
    }
}

#[test]
fn test_modern_single_byte_max_diff_253() {
    let mut codec = ModernNdxCodec::new(30);
    let mut buf = Vec::new();

    codec.write_ndx(&mut buf, 252).unwrap();
    assert_eq!(buf.len(), 1);
    assert_eq!(buf[0], 253);
}

#[test]
fn test_modern_two_byte_at_diff_254() {
    let mut codec = ModernNdxCodec::new(30);
    let mut buf = Vec::new();

    codec.write_ndx(&mut buf, 253).unwrap();
    assert_eq!(buf[0], 0xFE);
    assert_eq!(buf.len(), 3);
}

#[test]
fn test_modern_two_byte_max_diff_32767() {
    let mut codec = ModernNdxCodec::new(30);
    let mut buf = Vec::new();

    codec.write_ndx(&mut buf, 0).unwrap();
    buf.clear();

    codec.write_ndx(&mut buf, 32767).unwrap();
    assert_eq!(buf[0], 0xFE);
    assert!(buf[1] & 0x80 == 0);
}

#[test]
fn test_modern_four_byte_for_large_diff() {
    let mut codec = ModernNdxCodec::new(30);
    let mut buf = Vec::new();

    codec.write_ndx(&mut buf, 0x01_00_00_00).unwrap();
    assert_eq!(buf[0], 0xFE);
    assert!(buf[1] & 0x80 != 0);
    assert_eq!(buf.len(), 5);
}

#[test]
fn test_ndx_codec_enum_dispatches_correctly() {
    let mut legacy_enum = NdxCodecEnum::new(29);
    let mut buf = Vec::new();
    legacy_enum.write_ndx(&mut buf, 5).unwrap();
    assert_eq!(buf.len(), 4, "enum should use legacy 4-byte format");

    let mut modern_enum = NdxCodecEnum::new(30);
    buf.clear();
    modern_enum.write_ndx(&mut buf, 0).unwrap();
    assert_eq!(buf.len(), 1, "enum should use modern delta format");
}

#[test]
fn test_ndx_codec_enum_protocol_version() {
    for version in [28, 29, 30, 31, 32] {
        let codec = NdxCodecEnum::new(version);
        assert_eq!(codec.protocol_version(), version);
    }
}

#[test]
fn test_create_ndx_codec_matches_direct_construction() {
    let factory = create_ndx_codec(29);
    let direct = NdxCodecEnum::Legacy(LegacyNdxCodec::new(29));
    assert_eq!(factory.protocol_version(), direct.protocol_version());

    let factory = create_ndx_codec(30);
    let direct = NdxCodecEnum::Modern(ModernNdxCodec::new(30));
    assert_eq!(factory.protocol_version(), direct.protocol_version());
}

#[test]
fn test_ndx_state_default_equals_new() {
    let default_state = NdxState::default();
    let new_state = NdxState::new();

    let mut default_buf = Vec::new();
    let mut new_buf = Vec::new();

    let mut d = default_state.clone();
    let mut n = new_state.clone();

    d.write_ndx(&mut default_buf, 0).unwrap();
    n.write_ndx(&mut new_buf, 0).unwrap();

    assert_eq!(default_buf, new_buf);
}

#[test]
fn test_write_ndx_done_helper() {
    let mut buf = Vec::new();
    write_ndx_done(&mut buf).unwrap();
    assert_eq!(buf, [0x00]);
}

#[test]
fn test_write_ndx_flist_eof_helper() {
    let mut buf = Vec::new();
    let mut state = NdxState::new();
    write_ndx_flist_eof(&mut buf, &mut state).unwrap();
    assert_eq!(buf, [0xFF, 0x01]);
}

#[test]
fn test_ndx_state_clone_independence() {
    let mut state = NdxState::new();
    let mut buf = Vec::new();
    state.write_ndx(&mut buf, 0).unwrap();
    state.write_ndx(&mut buf, 1).unwrap();

    let mut cloned = state.clone();

    let mut orig_buf = Vec::new();
    let mut clone_buf = Vec::new();

    state.write_ndx(&mut orig_buf, 10).unwrap();
    cloned.write_ndx(&mut clone_buf, 10).unwrap();

    assert_eq!(orig_buf, clone_buf);
}

#[test]
fn test_ndx_extreme_positive_values() {
    let extreme_values = [
        0i32,
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
        0x7FFF_FFFF, // i32::MAX
    ];

    for version in [28, 29, 30, 31, 32] {
        let mut write_codec = create_ndx_codec(version);
        let mut buf = Vec::new();

        for &ndx in &extreme_values {
            write_codec.write_ndx(&mut buf, ndx).unwrap();
        }

        let mut read_codec = create_ndx_codec(version);
        let mut cursor = Cursor::new(&buf);

        for &expected in &extreme_values {
            let read = read_codec.read_ndx(&mut cursor).unwrap();
            assert_eq!(read, expected, "v{version} failed for {expected}");
        }
    }
}

#[test]
fn test_ndx_large_gaps() {
    let values = [0i32, 10000, 20000, 100000, 1000000];

    for version in [28, 29, 30, 31, 32] {
        let mut write_codec = create_ndx_codec(version);
        let mut buf = Vec::new();

        for &ndx in &values {
            write_codec.write_ndx(&mut buf, ndx).unwrap();
        }

        let mut read_codec = create_ndx_codec(version);
        let mut cursor = Cursor::new(&buf);

        for &expected in &values {
            let read = read_codec.read_ndx(&mut cursor).unwrap();
            assert_eq!(read, expected, "v{version} failed for {expected}");
        }
    }
}

#[test]
fn test_legacy_ndx_write_io_error() {
    use std::io::{self, Write};

    struct FailWriter;
    impl Write for FailWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "write failed"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut codec = LegacyNdxCodec::new(29);
    let result = codec.write_ndx(&mut FailWriter, 42);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
}

#[test]
fn test_modern_ndx_write_io_error() {
    use std::io::{self, Write};

    struct FailWriter;
    impl Write for FailWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "write failed"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut codec = ModernNdxCodec::new(30);
    let result = codec.write_ndx(&mut FailWriter, 42);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
}

#[test]
fn test_legacy_ndx_read_partial_data() {
    let mut codec = LegacyNdxCodec::new(29);
    let partial_data = [0x01u8, 0x02, 0x03];
    let mut cursor = Cursor::new(&partial_data[..]);
    let result = codec.read_ndx(&mut cursor);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn test_modern_ndx_read_truncated_negative_prefix() {
    let mut codec = ModernNdxCodec::new(30);
    let truncated = [0xFFu8];
    let mut cursor = Cursor::new(&truncated[..]);
    let result = codec.read_ndx(&mut cursor);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn test_modern_ndx_read_truncated_2byte_diff() {
    let mut codec = ModernNdxCodec::new(30);
    let truncated = [0xFEu8, 0x00];
    let mut cursor = Cursor::new(&truncated[..]);
    let result = codec.read_ndx(&mut cursor);
    assert!(result.is_err());
}

#[test]
fn test_modern_ndx_read_truncated_4byte_value() {
    let mut codec = ModernNdxCodec::new(30);
    let truncated = [0xFEu8, 0x80, 0x00, 0x00];
    let mut cursor = Cursor::new(&truncated[..]);
    let result = codec.read_ndx(&mut cursor);
    assert!(result.is_err());
}

#[test]
fn test_modern_ndx_zero_diff() {
    let mut codec = ModernNdxCodec::new(30);
    let mut buf = Vec::new();

    codec.write_ndx(&mut buf, 0).unwrap();
    assert_eq!(buf, [0x01]);

    buf.clear();
    codec.write_ndx(&mut buf, 0).unwrap();
    assert!(buf[0] == 0xFE, "zero diff should use extended encoding");
}

#[test]
fn test_modern_ndx_negative_diff() {
    let mut codec = ModernNdxCodec::new(30);
    let mut buf = Vec::new();

    codec.write_ndx(&mut buf, 100).unwrap();
    buf.clear();
    codec.write_ndx(&mut buf, 50).unwrap();
    assert!(buf[0] == 0xFE, "negative diff should use extended encoding");
}

#[test]
fn test_modern_ndx_diff_boundary_253() {
    let mut codec = ModernNdxCodec::new(30);
    let mut buf = Vec::new();

    codec.write_ndx(&mut buf, 252).unwrap();
    assert_eq!(buf.len(), 1);
    assert_eq!(buf[0], 253);
}

#[test]
fn test_modern_ndx_diff_boundary_254() {
    let mut codec = ModernNdxCodec::new(30);
    let mut buf = Vec::new();

    codec.write_ndx(&mut buf, 253).unwrap();
    assert_eq!(buf[0], 0xFE);
    assert!(buf.len() >= 3);
}

#[test]
fn test_modern_ndx_state_after_ndx_done() {
    let mut write_codec = ModernNdxCodec::new(30);
    let mut buf = Vec::new();

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

#[test]
fn test_modern_ndx_interleaved_positive_negative() {
    let mut write_codec = ModernNdxCodec::new(30);
    let mut buf = Vec::new();

    let sequence = [0, NDX_FLIST_EOF, 5, NDX_DEL_STATS, 10, NDX_FLIST_OFFSET];

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

#[test]
fn test_ndx_codec_enum_write_ndx_done_legacy() {
    let mut codec = NdxCodecEnum::new(29);
    let mut buf = Vec::new();
    codec.write_ndx_done(&mut buf).unwrap();
    assert_eq!(buf, [0xFF, 0xFF, 0xFF, 0xFF]);
}

#[test]
fn test_ndx_codec_enum_write_ndx_done_modern() {
    let mut codec = NdxCodecEnum::new(30);
    let mut buf = Vec::new();
    codec.write_ndx_done(&mut buf).unwrap();
    assert_eq!(buf, [0x00]);
}

#[test]
fn test_ndx_roundtrip_random_sequence() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    fn pseudo_random(seed: u64, i: usize) -> i32 {
        let mut hasher = DefaultHasher::new();
        (seed, i).hash(&mut hasher);
        (hasher.finish() % 10000) as i32
    }

    for version in [28, 29, 30, 31, 32] {
        let mut write_codec = create_ndx_codec(version);
        let mut buf = Vec::new();

        let seed = 42u64;
        let count = 100;

        for i in 0..count {
            let ndx = pseudo_random(seed, i);
            write_codec.write_ndx(&mut buf, ndx).unwrap();
        }

        let mut read_codec = create_ndx_codec(version);
        let mut cursor = Cursor::new(&buf);

        for i in 0..count {
            let expected = pseudo_random(seed, i);
            let read = read_codec.read_ndx(&mut cursor).unwrap();
            assert_eq!(read, expected, "v{version} failed at index {i}");
        }
    }
}

#[test]
fn test_ndx_monotonic_sequence() {
    for version in [28, 29, 30, 31, 32] {
        let mut write_codec = create_ndx_codec(version);
        let mut buf = Vec::new();

        for i in 0..1000 {
            write_codec.write_ndx(&mut buf, i).unwrap();
        }

        let mut read_codec = create_ndx_codec(version);
        let mut cursor = Cursor::new(&buf);

        for i in 0..1000 {
            let read = read_codec.read_ndx(&mut cursor).unwrap();
            assert_eq!(read, i, "v{version} failed at index {i}");
        }
    }
}

#[test]
fn test_ndx_sparse_sequence() {
    let sparse_indices: Vec<i32> = (0..100).map(|i| i * 1000).collect();

    for version in [28, 29, 30, 31, 32] {
        let mut write_codec = create_ndx_codec(version);
        let mut buf = Vec::new();

        for &ndx in &sparse_indices {
            write_codec.write_ndx(&mut buf, ndx).unwrap();
        }

        let mut read_codec = create_ndx_codec(version);
        let mut cursor = Cursor::new(&buf);

        for &expected in &sparse_indices {
            let read = read_codec.read_ndx(&mut cursor).unwrap();
            assert_eq!(read, expected, "v{version} failed for {expected}");
        }
    }
}

#[test]
fn test_modern_ndx_wire_format_single_byte() {
    let mut codec = ModernNdxCodec::new(30);
    let mut buf = Vec::new();

    codec.write_ndx(&mut buf, 0).unwrap();
    assert_eq!(buf, [0x01]);

    buf.clear();
    codec.write_ndx(&mut buf, 1).unwrap();
    assert_eq!(buf, [0x01]);

    buf.clear();
    codec.write_ndx(&mut buf, 11).unwrap();
    assert_eq!(buf, [0x0A]);
}

#[test]
fn test_modern_ndx_wire_format_two_byte_diff() {
    let mut codec = ModernNdxCodec::new(30);
    let mut buf = Vec::new();

    codec.write_ndx(&mut buf, 299).unwrap();
    assert_eq!(buf[0], 0xFE);
    assert_eq!(buf[1], 0x01); // 300 >> 8 = 1
    assert_eq!(buf[2], 0x2C); // 300 & 0xFF = 0x2C
}

#[test]
fn test_modern_ndx_wire_format_four_byte_value() {
    let mut codec = ModernNdxCodec::new(30);
    let mut buf = Vec::new();

    let large_value = 0x01_23_45_67i32;
    codec.write_ndx(&mut buf, large_value).unwrap();
    assert_eq!(buf[0], 0xFE);
    assert!(buf[1] & 0x80 != 0);
}

#[test]
fn test_modern_ndx_wire_format_negative() {
    let mut codec = ModernNdxCodec::new(30);
    let mut buf = Vec::new();

    codec.write_ndx(&mut buf, NDX_FLIST_EOF).unwrap();
    assert_eq!(buf, [0xFF, 0x01]);
}

#[test]
fn test_legacy_ndx_wire_format() {
    let mut codec = LegacyNdxCodec::new(29);
    let mut buf = Vec::new();

    codec.write_ndx(&mut buf, 0x12345678).unwrap();
    assert_eq!(buf, [0x78, 0x56, 0x34, 0x12]);

    buf.clear();
    codec.write_ndx(&mut buf, -1).unwrap();
    assert_eq!(buf, [0xFF, 0xFF, 0xFF, 0xFF]);
}

#[test]
fn test_ndx_done_legacy_bytes_constant() {
    assert_eq!(NDX_DONE_LEGACY_BYTES, (-1i32).to_le_bytes());
}

#[test]
fn test_ndx_done_modern_byte_constant() {
    assert_eq!(NDX_DONE_MODERN_BYTE, 0x00);
}

#[test]
fn test_write_goodbye_legacy() {
    let mut buf = Vec::new();
    write_goodbye(&mut buf, 28).unwrap();
    assert_eq!(buf, NDX_DONE_LEGACY_BYTES);

    buf.clear();
    write_goodbye(&mut buf, 29).unwrap();
    assert_eq!(buf, NDX_DONE_LEGACY_BYTES);
}

#[test]
fn test_write_goodbye_modern() {
    let mut buf = Vec::new();
    write_goodbye(&mut buf, 30).unwrap();
    assert_eq!(buf, [0x00]);

    buf.clear();
    write_goodbye(&mut buf, 32).unwrap();
    assert_eq!(buf, [0x00]);
}

#[test]
fn test_read_goodbye_legacy_valid() {
    let mut cursor = Cursor::new(NDX_DONE_LEGACY_BYTES.to_vec());
    read_goodbye(&mut cursor, 28).unwrap();
}

#[test]
fn test_read_goodbye_modern_valid() {
    let mut cursor = Cursor::new(vec![0x00]);
    read_goodbye(&mut cursor, 30).unwrap();
}

#[test]
fn test_read_goodbye_legacy_rejects_wrong_value() {
    let data = 42i32.to_le_bytes().to_vec();
    let mut cursor = Cursor::new(data);
    assert!(read_goodbye(&mut cursor, 29).is_err());
}

#[test]
fn test_read_goodbye_modern_rejects_wrong_value() {
    let mut cursor = Cursor::new(vec![0xFF]);
    assert!(read_goodbye(&mut cursor, 30).is_err());
}

#[test]
fn test_write_goodbye_roundtrip_all_versions() {
    for version in [28, 29, 30, 31, 32] {
        let mut buf = Vec::new();
        write_goodbye(&mut buf, version).unwrap();
        let mut cursor = Cursor::new(buf);
        read_goodbye(&mut cursor, version).unwrap();
    }
}
