use super::*;

#[test]
fn protocol_28_is_oldest_supported() {
    let protocol = ProtocolVersion::try_from(28u8).unwrap();
    assert!(
        protocol.supports_extended_flags(),
        "protocol 28 should support extended flags"
    );
}

#[test]
fn protocol_boundary_28_round_trip() {
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol28 = ProtocolVersion::try_from(28u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol28)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

    let mut entry = FileEntry::new_file("test.txt".into(), 1024, 0o644);
    entry.set_uid(1000);
    entry.set_gid(1000);
    entry.set_mtime(1700000000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol28)
        .with_preserve_uid(true)
        .with_preserve_gid(true);
    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "test.txt");
    assert_eq!(read_entry.size(), 1024);
    assert_eq!(read_entry.uid(), Some(1000));
    assert_eq!(read_entry.gid(), Some(1000));
}

#[test]
fn protocol_boundary_29_30_user_names() {
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol30 = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol30)
        .with_preserve_uid(true)
        .with_preserve_gid(true)
        .with_name_follows(true);

    let mut entry = FileEntry::new_file("test.txt".into(), 1024, 0o644);
    entry.set_uid(1000);
    entry.set_gid(1000);
    entry.set_user_name("testuser".to_string());
    entry.set_group_name("testgroup".to_string());
    entry.set_mtime(1700000000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol30)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "test.txt");
    assert_eq!(read_entry.user_name(), Some("testuser"));
    assert_eq!(read_entry.group_name(), Some("testgroup"));
}

#[test]
fn protocol_boundary_30_31_nanoseconds() {
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    // Protocol 31 adds XMIT_MOD_NSEC for nanosecond mtime support.
    let protocol31 = ProtocolVersion::try_from(31u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol31);

    let mut entry = FileEntry::new_file("test.txt".into(), 1024, 0o644);
    entry.set_mtime(1700000000, 123456789);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol31);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.mtime(), 1700000000);
    assert_eq!(read_entry.mtime_nsec(), 123456789);
}

#[test]
fn very_long_path_name_round_trip() {
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = test_protocol();

    // A path longer than 255 bytes forces XMIT_LONG_NAME.
    let long_component = "a".repeat(100);
    let long_path = format!(
        "{long_component}/{long_component}/{long_component}/{long_component}/{long_component}"
    );
    assert!(long_path.len() > 255, "path should be longer than 255");

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let entry = FileEntry::new_file(long_path.clone().into(), 1024, 0o644);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), long_path);
}

#[test]
fn very_long_path_name_with_compression() {
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = test_protocol();

    let prefix = "a".repeat(200);
    let path1 = format!("{prefix}/file1.txt");
    let path2 = format!("{prefix}/file2.txt");

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let entry1 = FileEntry::new_file(path1.clone().into(), 1024, 0o644);
    let entry2 = FileEntry::new_file(path2.clone().into(), 2048, 0o644);

    writer.write_entry(&mut buf, &entry1).unwrap();
    let len_after_first = buf.len();
    writer.write_entry(&mut buf, &entry2).unwrap();
    let len_after_second = buf.len();

    let second_entry_len = len_after_second - len_after_first;
    assert!(
        second_entry_len < len_after_first,
        "second entry should be compressed due to shared prefix"
    );

    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);

    let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();

    assert_eq!(read1.name(), path1);
    assert_eq!(read2.name(), path2);
}

#[test]
fn extreme_mtime_values() {
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = test_protocol();

    // Only non-negative mtimes are tested: negative timestamps are encoded as
    // unsigned on the wire.
    let test_cases = [
        0i64,
        1,
        i32::MAX as i64,     // Last 32-bit Unix second (2038-01-19).
        i32::MAX as i64 + 1, // First post-2038 value, exercises 64-bit path.
        1_000_000_000_000i64,
    ];

    for &mtime in &test_cases {
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        let mut entry = FileEntry::new_file("test.txt".into(), 1024, 0o644);
        entry.set_mtime(mtime, 0);

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(
            read_entry.mtime(),
            mtime,
            "mtime {mtime} should round-trip correctly"
        );
    }
}

/// Test encoding and decoding a 3GB file (above 2^31 = 2GB boundary).
/// This verifies that the varlong encoding correctly handles file sizes
/// that exceed the signed 32-bit integer range.
#[test]
fn large_file_size_3gb_round_trip() {
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    const SIZE_3GB: u64 = 3 * 1024 * 1024 * 1024;

    let protocol = test_protocol();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let mut entry = FileEntry::new_file("large_3gb.bin".into(), SIZE_3GB, 0o644);
    entry.set_mtime(1700000000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "large_3gb.bin");
    assert_eq!(
        read_entry.size(),
        SIZE_3GB,
        "3GB file size should round-trip correctly (above 2^31 boundary)"
    );
}

/// Test encoding and decoding a 5GB file (above 2^32 = 4GB boundary).
/// This verifies that the varlong encoding correctly handles file sizes
/// that exceed the unsigned 32-bit integer range.
#[test]
fn large_file_size_5gb_round_trip() {
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    const SIZE_5GB: u64 = 5 * 1024 * 1024 * 1024;

    let protocol = test_protocol();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let mut entry = FileEntry::new_file("large_5gb.bin".into(), SIZE_5GB, 0o644);
    entry.set_mtime(1700000000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "large_5gb.bin");
    assert_eq!(
        read_entry.size(),
        SIZE_5GB,
        "5GB file size should round-trip correctly (above 2^32 boundary)"
    );
}

/// Test multiple large file sizes to ensure consistent encoding/decoding
/// across the 2GB and 4GB boundaries.
#[test]
fn large_file_sizes_boundary_values_round_trip() {
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    // Boundary values spanning the 2^31 and 2^32 thresholds in varlong encoding.
    let test_sizes: &[(u64, &str)] = &[
        ((1u64 << 31) - 1, "just_below_2gb"),
        (1u64 << 31, "exactly_2gb"),
        ((1u64 << 31) + 1, "just_above_2gb"),
        ((1u64 << 32) - 1, "just_below_4gb"),
        (1u64 << 32, "exactly_4gb"),
        ((1u64 << 32) + 1, "just_above_4gb"),
        (3 * 1024 * 1024 * 1024, "3gb"),
        (5 * 1024 * 1024 * 1024, "5gb"),
        (1024 * 1024 * 1024 * 1024, "1tb"),
    ];

    let protocol = test_protocol();

    for (size, name) in test_sizes {
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        let filename = format!("{name}.bin");
        let mut entry = FileEntry::new_file(filename.clone().into(), *size, 0o644);
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), &filename);
        assert_eq!(
            read_entry.size(),
            *size,
            "File size {size} ({name}) should round-trip correctly"
        );
    }
}

/// Test large file sizes with legacy protocol (< 30) which uses longint encoding.
/// The longint format uses 4 bytes for values <= 0x7FFFFFFF and 12 bytes for larger.
#[test]
fn large_file_size_legacy_protocol_round_trip() {
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    const SIZE_3GB: u64 = 3 * 1024 * 1024 * 1024;
    const SIZE_5GB: u64 = 5 * 1024 * 1024 * 1024;

    // Protocol 29 uses the legacy longint size encoding.
    let protocol = ProtocolVersion::try_from(29u8).unwrap();

    for size in [SIZE_3GB, SIZE_5GB] {
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        let mut entry = FileEntry::new_file("large_legacy.bin".into(), size, 0o644);
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(
            read_entry.size(),
            size,
            "Legacy protocol should handle {size} byte files correctly"
        );
    }
}
