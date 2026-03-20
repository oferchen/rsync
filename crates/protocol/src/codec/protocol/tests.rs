use super::*;
use std::io::Cursor;

#[test]
fn factory_creates_legacy_for_protocol_28() {
    let codec = create_protocol_codec(28);
    assert!(matches!(codec, ProtocolCodecEnum::Legacy(_)));
    assert_eq!(codec.protocol_version(), 28);
    assert!(codec.is_legacy());
}

#[test]
fn factory_creates_legacy_for_protocol_29() {
    let codec = create_protocol_codec(29);
    assert!(matches!(codec, ProtocolCodecEnum::Legacy(_)));
    assert_eq!(codec.protocol_version(), 29);
    assert!(codec.is_legacy());
}

#[test]
fn factory_creates_modern_for_protocol_30() {
    let codec = create_protocol_codec(30);
    assert!(matches!(codec, ProtocolCodecEnum::Modern(_)));
    assert_eq!(codec.protocol_version(), 30);
    assert!(!codec.is_legacy());
}

#[test]
fn factory_creates_modern_for_protocol_32() {
    let codec = create_protocol_codec(32);
    assert!(matches!(codec, ProtocolCodecEnum::Modern(_)));
    assert_eq!(codec.protocol_version(), 32);
    assert!(!codec.is_legacy());
}

#[test]
#[should_panic(expected = "LegacyProtocolCodec requires protocol < 30")]
fn legacy_codec_panics_for_protocol_30() {
    let _ = LegacyProtocolCodec::new(30);
}

#[test]
#[should_panic(expected = "ModernProtocolCodec requires protocol >= 30")]
fn modern_codec_panics_for_protocol_29() {
    let _ = ModernProtocolCodec::new(29);
}

#[test]
fn legacy_file_size_small_value() {
    let codec = create_protocol_codec(29);
    let mut buf = Vec::new();
    codec.write_file_size(&mut buf, 1000).unwrap();

    assert_eq!(buf.len(), 4);
    assert_eq!(buf, vec![0xe8, 0x03, 0x00, 0x00]); // 1000 in LE

    let mut cursor = Cursor::new(&buf);
    let value = codec.read_file_size(&mut cursor).unwrap();
    assert_eq!(value, 1000);
}

#[test]
fn legacy_file_size_large_value() {
    let codec = create_protocol_codec(29);
    let mut buf = Vec::new();
    let large_value = 0x1_0000_0000i64;
    codec.write_file_size(&mut buf, large_value).unwrap();

    assert_eq!(buf.len(), 12);
    assert_eq!(&buf[0..4], &[0xff, 0xff, 0xff, 0xff]); // marker

    let mut cursor = Cursor::new(&buf);
    let value = codec.read_file_size(&mut cursor).unwrap();
    assert_eq!(value, large_value);
}

#[test]
fn modern_file_size_small_value() {
    let codec = create_protocol_codec(32);
    let mut buf = Vec::new();
    codec.write_file_size(&mut buf, 1000).unwrap();

    assert!(buf.len() <= 4);

    let mut cursor = Cursor::new(&buf);
    let value = codec.read_file_size(&mut cursor).unwrap();
    assert_eq!(value, 1000);
}

#[test]
fn modern_file_size_large_value() {
    let codec = create_protocol_codec(32);
    let mut buf = Vec::new();
    let large_value = 0x1_0000_0000i64;
    codec.write_file_size(&mut buf, large_value).unwrap();

    let mut cursor = Cursor::new(&buf);
    let value = codec.read_file_size(&mut cursor).unwrap();
    assert_eq!(value, large_value);
}

#[test]
fn legacy_mtime_encoding() {
    let codec = create_protocol_codec(29);
    let mut buf = Vec::new();
    let mtime = 1700000000i64;

    codec.write_mtime(&mut buf, mtime).unwrap();
    assert_eq!(buf.len(), 4);

    let mut cursor = Cursor::new(&buf);
    let value = codec.read_mtime(&mut cursor).unwrap();
    assert_eq!(value, mtime);
}

#[test]
fn modern_mtime_encoding() {
    let codec = create_protocol_codec(32);
    let mut buf = Vec::new();
    let mtime = 1700000000i64;

    codec.write_mtime(&mut buf, mtime).unwrap();

    let mut cursor = Cursor::new(&buf);
    let value = codec.read_mtime(&mut cursor).unwrap();
    assert_eq!(value, mtime);
}

#[test]
fn legacy_long_name_len_encoding() {
    let codec = create_protocol_codec(29);
    let mut buf = Vec::new();
    let len = 300usize;

    codec.write_long_name_len(&mut buf, len).unwrap();
    assert_eq!(buf.len(), 4);

    let mut cursor = Cursor::new(&buf);
    let value = codec.read_long_name_len(&mut cursor).unwrap();
    assert_eq!(value, len);
}

#[test]
fn modern_long_name_len_encoding() {
    let codec = create_protocol_codec(32);
    let mut buf = Vec::new();
    let len = 300usize;

    codec.write_long_name_len(&mut buf, len).unwrap();
    assert!(buf.len() <= 2);

    let mut cursor = Cursor::new(&buf);
    let value = codec.read_long_name_len(&mut cursor).unwrap();
    assert_eq!(value, len);
}

#[test]
fn write_int_is_always_4_bytes() {
    for version in [28, 29, 30, 31, 32] {
        let codec = create_protocol_codec(version);
        let mut buf = Vec::new();
        codec.write_int(&mut buf, 12345).unwrap();
        assert_eq!(buf.len(), 4);
        assert_eq!(buf, vec![0x39, 0x30, 0x00, 0x00]); // 12345 in LE
    }
}

#[test]
fn read_int_round_trip() {
    for version in [28, 29, 30, 31, 32] {
        let codec = create_protocol_codec(version);
        let mut buf = Vec::new();
        codec.write_int(&mut buf, -1).unwrap();

        let mut cursor = Cursor::new(&buf);
        let value = codec.read_int(&mut cursor).unwrap();
        assert_eq!(value, -1);
    }
}

#[test]
fn file_size_round_trip_all_versions() {
    let test_sizes = [0i64, 1, 255, 256, 65535, 65536, 0x7FFF_FFFF, 0x1_0000_0000];

    for version in [28, 29, 30, 31, 32] {
        let codec = create_protocol_codec(version);
        for &size in &test_sizes {
            let mut buf = Vec::new();
            codec.write_file_size(&mut buf, size).unwrap();

            let mut cursor = Cursor::new(&buf);
            let value = codec.read_file_size(&mut cursor).unwrap();
            assert_eq!(
                value, size,
                "Round-trip failed for size={size} protocol={version}"
            );
        }
    }
}

#[test]
fn mtime_round_trip_all_versions() {
    let test_mtimes = [0i64, 1, 1700000000, 0x7FFF_FFFF, 0xFFFF_FFFF];

    for version in [28, 29, 30, 31, 32] {
        let codec = create_protocol_codec(version);
        for &mtime in &test_mtimes {
            let mut buf = Vec::new();
            codec.write_mtime(&mut buf, mtime).unwrap();

            let mut cursor = Cursor::new(&buf);
            let value = codec.read_mtime(&mut cursor).unwrap();
            assert_eq!(
                value, mtime,
                "Round-trip failed for mtime={mtime} protocol={version}"
            );
        }
    }
}

#[test]
fn protocol_28_does_not_support_sender_receiver_modifiers() {
    let codec = create_protocol_codec(28);
    assert!(!codec.supports_sender_receiver_modifiers());
}

#[test]
fn protocol_29_supports_sender_receiver_modifiers() {
    let codec = create_protocol_codec(29);
    assert!(codec.supports_sender_receiver_modifiers());
}

#[test]
fn protocol_30_supports_sender_receiver_modifiers() {
    let codec = create_protocol_codec(30);
    assert!(codec.supports_sender_receiver_modifiers());
}

#[test]
fn protocol_32_supports_sender_receiver_modifiers() {
    let codec = create_protocol_codec(32);
    assert!(codec.supports_sender_receiver_modifiers());
}

#[test]
fn protocol_28_does_not_support_perishable() {
    let codec = create_protocol_codec(28);
    assert!(!codec.supports_perishable_modifier());
}

#[test]
fn protocol_29_does_not_support_perishable() {
    let codec = create_protocol_codec(29);
    assert!(!codec.supports_perishable_modifier());
}

#[test]
fn protocol_30_supports_perishable() {
    let codec = create_protocol_codec(30);
    assert!(codec.supports_perishable_modifier());
}

#[test]
fn protocol_31_supports_perishable() {
    let codec = create_protocol_codec(31);
    assert!(codec.supports_perishable_modifier());
}

#[test]
fn protocol_32_supports_perishable() {
    let codec = create_protocol_codec(32);
    assert!(codec.supports_perishable_modifier());
}

#[test]
fn protocol_28_uses_old_prefixes() {
    let codec = create_protocol_codec(28);
    assert!(codec.uses_old_prefixes());
}

#[test]
fn protocol_29_does_not_use_old_prefixes() {
    let codec = create_protocol_codec(29);
    assert!(!codec.uses_old_prefixes());
}

#[test]
fn protocol_30_does_not_use_old_prefixes() {
    let codec = create_protocol_codec(30);
    assert!(!codec.uses_old_prefixes());
}

#[test]
fn protocol_32_does_not_use_old_prefixes() {
    let codec = create_protocol_codec(32);
    assert!(!codec.uses_old_prefixes());
}

#[test]
fn filter_modifier_support_boundary_at_29() {
    let codec_28 = create_protocol_codec(28);
    assert!(!codec_28.supports_sender_receiver_modifiers());
    assert!(!codec_28.supports_perishable_modifier());
    assert!(codec_28.uses_old_prefixes());

    let codec_29 = create_protocol_codec(29);
    assert!(codec_29.supports_sender_receiver_modifiers());
    assert!(!codec_29.supports_perishable_modifier());
    assert!(!codec_29.uses_old_prefixes());
}

#[test]
fn filter_modifier_support_boundary_at_30() {
    let codec_29 = create_protocol_codec(29);
    assert!(codec_29.supports_sender_receiver_modifiers());
    assert!(!codec_29.supports_perishable_modifier());

    let codec_30 = create_protocol_codec(30);
    assert!(codec_30.supports_sender_receiver_modifiers());
    assert!(codec_30.supports_perishable_modifier());
}

#[test]
fn protocol_28_does_not_support_flist_times() {
    let codec = create_protocol_codec(28);
    assert!(!codec.supports_flist_times());
}

#[test]
fn protocol_29_supports_flist_times() {
    let codec = create_protocol_codec(29);
    assert!(codec.supports_flist_times());
}

#[test]
fn protocol_30_supports_flist_times() {
    let codec = create_protocol_codec(30);
    assert!(codec.supports_flist_times());
}

#[test]
fn protocol_32_supports_flist_times() {
    let codec = create_protocol_codec(32);
    assert!(codec.supports_flist_times());
}

#[test]
fn flist_times_support_boundary_at_29() {
    let codec_28 = create_protocol_codec(28);
    assert!(!codec_28.supports_flist_times());

    let codec_29 = create_protocol_codec(29);
    assert!(codec_29.supports_flist_times());
}

#[test]
fn write_stat_uses_file_size_encoding() {
    let codec = create_protocol_codec(29);
    let mut stat_buf = Vec::new();
    let mut size_buf = Vec::new();

    codec.write_stat(&mut stat_buf, 12345).unwrap();
    codec.write_file_size(&mut size_buf, 12345).unwrap();

    assert_eq!(stat_buf, size_buf);
}

#[test]
fn read_stat_uses_file_size_encoding() {
    let codec = create_protocol_codec(30);
    let mut buf = Vec::new();
    codec.write_stat(&mut buf, 999999).unwrap();

    let mut cursor1 = Cursor::new(&buf);
    let mut cursor2 = Cursor::new(&buf);

    let stat_value = codec.read_stat(&mut cursor1).unwrap();
    let size_value = codec.read_file_size(&mut cursor2).unwrap();

    assert_eq!(stat_value, size_value);
    assert_eq!(stat_value, 999999);
}

#[test]
fn stat_round_trip_legacy() {
    let codec = create_protocol_codec(29);
    let test_values = [0i64, 1, 1000, 65535, 0x7FFF_FFFF, 0x1_0000_0000];

    for &value in &test_values {
        let mut buf = Vec::new();
        codec.write_stat(&mut buf, value).unwrap();

        let mut cursor = Cursor::new(&buf);
        let read_value = codec.read_stat(&mut cursor).unwrap();
        assert_eq!(
            read_value, value,
            "Stat round-trip failed for value={value} (legacy)"
        );
    }
}

#[test]
fn stat_round_trip_modern() {
    let codec = create_protocol_codec(32);
    let test_values = [0i64, 1, 1000, 65535, 0x7FFF_FFFF, 0x1_0000_0000];

    for &value in &test_values {
        let mut buf = Vec::new();
        codec.write_stat(&mut buf, value).unwrap();

        let mut cursor = Cursor::new(&buf);
        let read_value = codec.read_stat(&mut cursor).unwrap();
        assert_eq!(
            read_value, value,
            "Stat round-trip failed for value={value} (modern)"
        );
    }
}

#[test]
fn version_27_not_supported_use_28_as_minimum() {
    let codec = create_protocol_codec(28);
    assert!(codec.is_legacy());
    assert_eq!(codec.protocol_version(), 28);
}

#[test]
fn all_supported_versions_create_valid_codecs() {
    for version in 28..=32 {
        let codec = create_protocol_codec(version);
        assert_eq!(codec.protocol_version(), version);
    }
}

#[test]
fn version_boundary_at_30_encoding_changes() {
    let legacy = create_protocol_codec(29);
    let mut legacy_buf = Vec::new();
    legacy.write_file_size(&mut legacy_buf, 100).unwrap();
    assert_eq!(legacy_buf.len(), 4, "legacy version uses 4-byte fixed");

    let modern = create_protocol_codec(30);
    let mut modern_buf = Vec::new();
    modern.write_file_size(&mut modern_buf, 100).unwrap();
    assert!(modern_buf.len() <= 4, "modern version uses varlong");
}

#[test]
fn legacy_encoding_matches_upstream_byte_patterns() {
    let codec = create_protocol_codec(29);

    let mut buf = Vec::new();
    codec.write_file_size(&mut buf, 0).unwrap();
    assert_eq!(buf, [0x00, 0x00, 0x00, 0x00]);

    buf.clear();
    codec.write_file_size(&mut buf, 255).unwrap();
    assert_eq!(buf, [0xff, 0x00, 0x00, 0x00]);

    buf.clear();
    codec.write_file_size(&mut buf, 1000).unwrap();
    assert_eq!(buf, [0xe8, 0x03, 0x00, 0x00]);

    buf.clear();
    codec.write_file_size(&mut buf, 0x7FFF_FFFF).unwrap();
    assert_eq!(buf, [0xff, 0xff, 0xff, 0x7f]);
}

#[test]
fn legacy_large_file_uses_12_byte_longint() {
    let codec = create_protocol_codec(29);
    let mut buf = Vec::new();

    codec
        .write_file_size(&mut buf, 0x1_0000_0000i64)
        .unwrap();
    assert_eq!(buf.len(), 12);
    assert_eq!(&buf[0..4], [0xff, 0xff, 0xff, 0xff]);
}

#[test]
fn modern_encoding_efficient_for_small_values() {
    let codec = create_protocol_codec(30);

    let mut buf = Vec::new();
    codec.write_file_size(&mut buf, 0).unwrap();
    assert!(buf.len() <= 4);

    let mut cursor = Cursor::new(&buf);
    let value = codec.read_file_size(&mut cursor).unwrap();
    assert_eq!(value, 0);
}

#[test]
fn mtime_encoding_differs_between_versions() {
    let legacy = create_protocol_codec(29);
    let modern = create_protocol_codec(30);
    let mtime = 1700000000i64;

    let mut legacy_buf = Vec::new();
    legacy.write_mtime(&mut legacy_buf, mtime).unwrap();
    assert_eq!(legacy_buf.len(), 4, "legacy mtime is 4 bytes");

    let mut modern_buf = Vec::new();
    modern.write_mtime(&mut modern_buf, mtime).unwrap();

    let mut cursor = Cursor::new(&legacy_buf);
    assert_eq!(legacy.read_mtime(&mut cursor).unwrap(), mtime);

    let mut cursor = Cursor::new(&modern_buf);
    assert_eq!(modern.read_mtime(&mut cursor).unwrap(), mtime);
}

#[test]
fn compatibility_flags_progressive_enablement() {
    struct VersionFeatures {
        sender_receiver: bool,
        perishable: bool,
        flist_times: bool,
        old_prefixes: bool,
    }

    let expected = [
        (
            28,
            VersionFeatures {
                sender_receiver: false,
                perishable: false,
                flist_times: false,
                old_prefixes: true,
            },
        ),
        (
            29,
            VersionFeatures {
                sender_receiver: true,
                perishable: false,
                flist_times: true,
                old_prefixes: false,
            },
        ),
        (
            30,
            VersionFeatures {
                sender_receiver: true,
                perishable: true,
                flist_times: true,
                old_prefixes: false,
            },
        ),
        (
            31,
            VersionFeatures {
                sender_receiver: true,
                perishable: true,
                flist_times: true,
                old_prefixes: false,
            },
        ),
        (
            32,
            VersionFeatures {
                sender_receiver: true,
                perishable: true,
                flist_times: true,
                old_prefixes: false,
            },
        ),
    ];

    for (version, features) in expected {
        let codec = create_protocol_codec(version);
        assert_eq!(
            codec.supports_sender_receiver_modifiers(),
            features.sender_receiver,
            "v{version} sender_receiver mismatch"
        );
        assert_eq!(
            codec.supports_perishable_modifier(),
            features.perishable,
            "v{version} perishable mismatch"
        );
        assert_eq!(
            codec.supports_flist_times(),
            features.flist_times,
            "v{version} flist_times mismatch"
        );
        assert_eq!(
            codec.uses_old_prefixes(),
            features.old_prefixes,
            "v{version} old_prefixes mismatch"
        );
    }
}

#[test]
fn feature_flags_never_disable_in_newer_versions() {
    let mut prev_sr = false;
    let mut prev_perishable = false;
    let mut prev_flist = false;

    for version in 28..=32 {
        let codec = create_protocol_codec(version);

        if prev_sr {
            assert!(
                codec.supports_sender_receiver_modifiers(),
                "sender_receiver must stay enabled at v{version}"
            );
        }
        if prev_perishable {
            assert!(
                codec.supports_perishable_modifier(),
                "perishable must stay enabled at v{version}"
            );
        }
        if prev_flist {
            assert!(
                codec.supports_flist_times(),
                "flist_times must stay enabled at v{version}"
            );
        }

        prev_sr = codec.supports_sender_receiver_modifiers();
        prev_perishable = codec.supports_perishable_modifier();
        prev_flist = codec.supports_flist_times();
    }
}

#[test]
fn read_file_size_handles_truncated_input() {
    let legacy = create_protocol_codec(29);
    let modern = create_protocol_codec(30);

    let truncated = [0u8, 0, 0];
    let mut cursor = Cursor::new(&truncated[..]);
    assert!(legacy.read_file_size(&mut cursor).is_err());

    let truncated = [0u8, 0];
    let mut cursor = Cursor::new(&truncated[..]);
    assert!(modern.read_file_size(&mut cursor).is_err());
}

#[test]
fn read_mtime_handles_truncated_input() {
    let legacy = create_protocol_codec(29);

    let truncated = [0u8, 0];
    let mut cursor = Cursor::new(&truncated[..]);
    assert!(legacy.read_mtime(&mut cursor).is_err());
}

#[test]
fn read_int_handles_truncated_input() {
    let codec = create_protocol_codec(30);

    let truncated = [0u8, 0];
    let mut cursor = Cursor::new(&truncated[..]);
    assert!(codec.read_int(&mut cursor).is_err());
}

#[test]
fn read_long_name_len_handles_truncated_input() {
    let legacy = create_protocol_codec(29);

    let truncated = [0u8, 0, 0];
    let mut cursor = Cursor::new(&truncated[..]);
    assert!(legacy.read_long_name_len(&mut cursor).is_err());
}

#[test]
fn empty_input_returns_error() {
    let codec = create_protocol_codec(30);
    let empty: [u8; 0] = [];

    let mut cursor = Cursor::new(&empty[..]);
    assert!(codec.read_file_size(&mut cursor).is_err());

    let mut cursor = Cursor::new(&empty[..]);
    assert!(codec.read_mtime(&mut cursor).is_err());

    let mut cursor = Cursor::new(&empty[..]);
    assert!(codec.read_int(&mut cursor).is_err());

    let mut cursor = Cursor::new(&empty[..]);
    assert!(codec.read_long_name_len(&mut cursor).is_err());
}

#[test]
fn write_int_consistent_across_all_versions() {
    let mut prev_buf: Option<Vec<u8>> = None;

    for version in 28..=32 {
        let codec = create_protocol_codec(version);
        let mut buf = Vec::new();
        codec.write_int(&mut buf, 12345).unwrap();

        if let Some(ref prev) = prev_buf {
            assert_eq!(&buf, prev, "write_int should be same across versions");
        }
        prev_buf = Some(buf);
    }
}

#[test]
fn write_varint_available_in_all_versions() {
    for version in [28, 29, 30, 31, 32] {
        let codec = create_protocol_codec(version);
        let mut buf = Vec::new();
        codec.write_varint(&mut buf, 1000).unwrap();

        let mut cursor = Cursor::new(&buf);
        let value = codec.read_varint(&mut cursor).unwrap();
        assert_eq!(value, 1000);
    }
}

#[test]
fn file_size_extreme_values_roundtrip() {
    let test_values = [
        0i64,
        1,
        i8::MAX as i64,
        u8::MAX as i64,
        i16::MAX as i64,
        u16::MAX as i64,
        i32::MAX as i64,
        u32::MAX as i64,
        0x1_0000_0000i64,
        0xFFFF_FFFF_FFFFi64,
    ];

    for version in [28, 29, 30, 31, 32] {
        let codec = create_protocol_codec(version);
        for &size in &test_values {
            let mut buf = Vec::new();
            codec.write_file_size(&mut buf, size).unwrap();

            let mut cursor = Cursor::new(&buf);
            let read = codec.read_file_size(&mut cursor).unwrap();
            assert_eq!(read, size, "v{version} roundtrip failed for {size}");
        }
    }
}

#[test]
fn negative_int_roundtrip() {
    for version in [28, 29, 30, 31, 32] {
        let codec = create_protocol_codec(version);
        for value in [-1i32, -100, i32::MIN, i32::MIN + 1] {
            let mut buf = Vec::new();
            codec.write_int(&mut buf, value).unwrap();

            let mut cursor = Cursor::new(&buf);
            let read = codec.read_int(&mut cursor).unwrap();
            assert_eq!(read, value, "v{version} int roundtrip failed for {value}");
        }
    }
}

#[test]
fn write_file_size_propagates_io_error() {
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

    let codec = create_protocol_codec(30);
    let result = codec.write_file_size(&mut FailWriter, 1000);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
}

#[test]
fn write_mtime_propagates_io_error() {
    use std::io::{self, Write};

    struct FailWriter;
    impl Write for FailWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "write failed",
            ))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let codec = create_protocol_codec(29);
    let result = codec.write_mtime(&mut FailWriter, 1700000000);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::ConnectionReset);
}

#[test]
fn write_int_propagates_io_error() {
    use std::io::{self, Write};

    struct FailWriter;
    impl Write for FailWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "write failed",
            ))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let codec = create_protocol_codec(32);
    let result = codec.write_int(&mut FailWriter, 42);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
}

#[test]
fn codec_enum_dispatches_to_correct_implementation() {
    let legacy = create_protocol_codec(29);
    let modern = create_protocol_codec(30);

    let value = 1000i64;

    let mut legacy_buf = Vec::new();
    legacy.write_file_size(&mut legacy_buf, value).unwrap();

    let mut modern_buf = Vec::new();
    modern.write_file_size(&mut modern_buf, value).unwrap();

    assert_eq!(legacy_buf.len(), 4);

    let mut cursor = Cursor::new(&legacy_buf);
    assert_eq!(legacy.read_file_size(&mut cursor).unwrap(), value);

    let mut cursor = Cursor::new(&modern_buf);
    assert_eq!(modern.read_file_size(&mut cursor).unwrap(), value);
}

#[test]
fn is_legacy_correctly_identifies_version() {
    for version in 28..=32 {
        let codec = create_protocol_codec(version);
        let expected_legacy = version < 30;
        assert_eq!(
            codec.is_legacy(),
            expected_legacy,
            "v{version} is_legacy mismatch"
        );
    }
}

#[test]
fn legacy_longint_format_for_large_values() {
    let codec = create_protocol_codec(29);
    let mut buf = Vec::new();

    let large = 0x1_ABCD_EF01i64;
    codec.write_file_size(&mut buf, large).unwrap();

    assert_eq!(buf.len(), 12);
    assert_eq!(&buf[0..4], &[0xFF, 0xFF, 0xFF, 0xFF]);

    let mut cursor = Cursor::new(&buf);
    let read = codec.read_file_size(&mut cursor).unwrap();
    assert_eq!(read, large);
}

#[test]
fn modern_varlong_efficient_encoding() {
    let codec = create_protocol_codec(30);

    let mut buf = Vec::new();
    codec.write_file_size(&mut buf, 100).unwrap();

    let mut cursor = Cursor::new(&buf);
    let read = codec.read_file_size(&mut cursor).unwrap();
    assert_eq!(read, 100);
}

#[test]
fn write_long_name_len_encoding_difference() {
    let legacy = create_protocol_codec(29);
    let modern = create_protocol_codec(30);
    let len = 300usize;

    let mut legacy_buf = Vec::new();
    legacy.write_long_name_len(&mut legacy_buf, len).unwrap();
    assert_eq!(legacy_buf.len(), 4);

    let mut modern_buf = Vec::new();
    modern.write_long_name_len(&mut modern_buf, len).unwrap();

    let mut cursor = Cursor::new(&legacy_buf);
    assert_eq!(legacy.read_long_name_len(&mut cursor).unwrap(), len);

    let mut cursor = Cursor::new(&modern_buf);
    assert_eq!(modern.read_long_name_len(&mut cursor).unwrap(), len);
}

#[test]
fn encoding_is_deterministic() {
    for version in [28, 29, 30, 31, 32] {
        let codec = create_protocol_codec(version);
        let value = 12345i64;

        let mut buf1 = Vec::new();
        let mut buf2 = Vec::new();

        codec.write_file_size(&mut buf1, value).unwrap();
        codec.write_file_size(&mut buf2, value).unwrap();

        assert_eq!(buf1, buf2, "v{version} encoding should be deterministic");
    }
}

#[test]
fn sequential_writes_independent() {
    let codec = create_protocol_codec(30);
    let mut buf = Vec::new();

    let values = [100i64, 200, 300, 1000000];
    for &val in &values {
        codec.write_file_size(&mut buf, val).unwrap();
    }

    let mut cursor = Cursor::new(&buf);
    for &expected in &values {
        let read = codec.read_file_size(&mut cursor).unwrap();
        assert_eq!(read, expected);
    }
}

#[test]
fn boundary_values_at_byte_boundaries() {
    let test_values = [
        0i64,
        0x7F,
        0x80,
        0xFF,
        0x100,
        0x7FFF,
        0x8000,
        0xFFFF,
        0x10000,
        0x7FFF_FFFF,
        0x8000_0000,
        0xFFFF_FFFF,
    ];

    for version in [28, 29, 30, 31, 32] {
        let codec = create_protocol_codec(version);
        for &value in &test_values {
            let mut buf = Vec::new();
            codec.write_file_size(&mut buf, value).unwrap();

            let mut cursor = Cursor::new(&buf);
            let read = codec.read_file_size(&mut cursor).unwrap();
            assert_eq!(
                read, value,
                "v{version} boundary test failed for {value:#X}"
            );
        }
    }
}

#[test]
fn mtime_boundary_values() {
    // upstream: proto < 30 uses read_uint/write_uint (unsigned u32)
    let legacy_mtimes = [
        0i64,
        1,
        1700000000,
        i32::MAX as i64,
        i32::MAX as i64 + 1,
        u32::MAX as i64,
    ];

    for version in [28, 29, 30, 31, 32] {
        let codec = create_protocol_codec(version);
        for &mtime in &legacy_mtimes {
            let mut buf = Vec::new();
            codec.write_mtime(&mut buf, mtime).unwrap();

            let mut cursor = Cursor::new(&buf);
            let read = codec.read_mtime(&mut cursor).unwrap();
            assert_eq!(read, mtime, "v{version} mtime boundary failed for {mtime}");
        }
    }
}

#[test]
fn mtime_large_values_modern_only() {
    let large_mtimes = [u32::MAX as i64 + 1, 0x1_0000_0000i64];

    for version in [30, 31, 32] {
        let codec = create_protocol_codec(version);
        for &mtime in &large_mtimes {
            let mut buf = Vec::new();
            codec.write_mtime(&mut buf, mtime).unwrap();

            let mut cursor = Cursor::new(&buf);
            let read = codec.read_mtime(&mut cursor).unwrap();
            assert_eq!(read, mtime, "v{version} mtime large failed for {mtime}");
        }
    }
}

#[test]
fn codec_debug_format() {
    let legacy = create_protocol_codec(29);
    let modern = create_protocol_codec(30);

    let legacy_debug = format!("{:?}", legacy);
    let modern_debug = format!("{:?}", modern);

    assert!(legacy_debug.contains("Legacy") || legacy_debug.contains("29"));
    assert!(modern_debug.contains("Modern") || modern_debug.contains("30"));
}

#[test]
fn varint_roundtrip_all_versions() {
    let test_values = [0i32, 1, 127, 128, 255, 256, 16383, 16384, 0x7FFF_FFFF];

    for version in [28, 29, 30, 31, 32] {
        let codec = create_protocol_codec(version);
        for &value in &test_values {
            let mut buf = Vec::new();
            codec.write_varint(&mut buf, value).unwrap();

            let mut cursor = Cursor::new(&buf);
            let read = codec.read_varint(&mut cursor).unwrap();
            assert_eq!(read, value, "v{version} varint failed for {value}");
        }
    }
}

#[test]
fn varlong_roundtrip_modern_only() {
    let codec = create_protocol_codec(30);
    let test_values = [0i64, 1, 0x7FFF_FFFF, 0x1_0000_0000, 0xFFFF_FFFF_FFFF];

    for &value in &test_values {
        let mut buf = Vec::new();
        codec.write_file_size(&mut buf, value).unwrap();

        let mut cursor = Cursor::new(&buf);
        let read = codec.read_file_size(&mut cursor).unwrap();
        assert_eq!(read, value, "varlong failed for {value:#X}");
    }
}
