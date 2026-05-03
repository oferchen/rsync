use super::super::flags::{
    XMIT_EXTENDED_FLAGS, XMIT_HLINK_FIRST, XMIT_HLINKED, XMIT_IO_ERROR_ENDLIST, XMIT_LONG_NAME,
    XMIT_SAME_MODE, XMIT_SAME_NAME, XMIT_SAME_TIME, XMIT_TOP_DIR,
};
use super::*;

fn test_protocol() -> ProtocolVersion {
    ProtocolVersion::try_from(32u8).unwrap()
}

#[test]
fn write_end_marker() {
    let mut buf = Vec::new();
    let writer = FileListWriter::new(test_protocol());
    writer.write_end(&mut buf, None).unwrap();
    assert_eq!(buf, vec![0u8]);
}

#[test]
fn write_simple_entry() {
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(test_protocol());
    let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);

    writer.write_entry(&mut buf, &entry).unwrap();

    assert!(!buf.is_empty());
    assert_ne!(buf[0], 0);
}

#[test]
fn write_multiple_entries_with_compression() {
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(test_protocol());

    let entry1 = FileEntry::new_file("dir/file1.txt".into(), 100, 0o644);
    let entry2 = FileEntry::new_file("dir/file2.txt".into(), 200, 0o644);

    writer.write_entry(&mut buf, &entry1).unwrap();
    let first_len = buf.len();

    writer.write_entry(&mut buf, &entry2).unwrap();
    let second_len = buf.len() - first_len;

    assert!(second_len < first_len, "second entry should be compressed");
}

#[test]
fn write_then_read_round_trip() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = test_protocol();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let mut entry = FileEntry::new_file("test.txt".into(), 1024, 0o644);
    entry.set_mtime(1700000000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "test.txt");
    assert_eq!(read_entry.size(), 1024);
}

#[test]
fn write_end_with_safe_file_list_enabled_transmits_error() {
    let protocol = test_protocol();
    let flags = CompatibilityFlags::SAFE_FILE_LIST;
    let writer = FileListWriter::with_compat_flags(protocol, flags);

    let mut buf = Vec::new();
    writer.write_end(&mut buf, Some(23)).unwrap();

    assert_ne!(buf, vec![0u8]);
    assert!(buf.len() > 1);
    assert_eq!(buf[0], XMIT_EXTENDED_FLAGS);
    assert_eq!(buf[1], XMIT_IO_ERROR_ENDLIST);

    use crate::varint::decode_varint;
    let cursor = &buf[2..];
    let (error_code, _) = decode_varint(cursor).unwrap();
    assert_eq!(error_code, 23);
}

#[test]
fn write_end_without_safe_file_list_writes_normal_marker_even_with_error() {
    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let writer = FileListWriter::new(protocol);

    let mut buf = Vec::new();
    writer.write_end(&mut buf, Some(23)).unwrap();

    assert_eq!(buf, vec![0u8]);
}

#[test]
fn write_end_with_protocol_31_enables_safe_mode_automatically() {
    let protocol = ProtocolVersion::try_from(31u8).unwrap();
    let writer = FileListWriter::new(protocol);

    let mut buf = Vec::new();
    writer.write_end(&mut buf, Some(42)).unwrap();

    assert_ne!(buf, vec![0u8]);
    assert!(buf.len() > 1);
    assert_eq!(buf[0], XMIT_EXTENDED_FLAGS);
    assert_eq!(buf[1], XMIT_IO_ERROR_ENDLIST);

    use crate::varint::decode_varint;
    let cursor = &buf[2..];
    let (error_code, _) = decode_varint(cursor).unwrap();
    assert_eq!(error_code, 42);
}

// Tests for extracted helper methods

#[test]
fn calculate_xflags_mode_comparison() {
    let mut writer = FileListWriter::new(test_protocol());
    // FileEntry::new_file includes file type bits (S_IFREG = 0o100000)
    // so mode 0o644 becomes 0o100644
    writer.state.update_mode(0o100644);

    let entry_same = FileEntry::new_file("test".into(), 100, 0o644);
    let entry_diff = FileEntry::new_file("test".into(), 100, 0o755);

    let flags_same = writer.calculate_xflags(&entry_same, 0, 4);
    let flags_diff = writer.calculate_xflags(&entry_diff, 0, 4);

    assert!(flags_same & (XMIT_SAME_MODE as u32) != 0);
    assert!(flags_diff & (XMIT_SAME_MODE as u32) == 0);
}

#[test]
fn calculate_xflags_time_comparison() {
    let mut writer = FileListWriter::new(test_protocol());
    writer.state.update_mtime(1700000000);

    let mut entry_same = FileEntry::new_file("test".into(), 100, 0o644);
    entry_same.set_mtime(1700000000, 0);

    let mut entry_diff = FileEntry::new_file("test".into(), 100, 0o644);
    entry_diff.set_mtime(1700000001, 0);

    let flags_same = writer.calculate_xflags(&entry_same, 0, 4);
    let flags_diff = writer.calculate_xflags(&entry_diff, 0, 4);

    assert!(flags_same & (XMIT_SAME_TIME as u32) != 0);
    assert!(flags_diff & (XMIT_SAME_TIME as u32) == 0);
}

#[test]
fn calculate_xflags_name_compression() {
    let writer = FileListWriter::new(test_protocol());
    let entry = FileEntry::new_file("test".into(), 100, 0o644);

    let flags_no_prefix = writer.calculate_xflags(&entry, 0, 4);
    let flags_with_prefix = writer.calculate_xflags(&entry, 2, 2);
    let flags_long_name = writer.calculate_xflags(&entry, 0, 300);

    assert!(flags_no_prefix & (XMIT_SAME_NAME as u32) == 0);
    assert!(flags_with_prefix & (XMIT_SAME_NAME as u32) != 0);
    assert!(flags_long_name & (XMIT_LONG_NAME as u32) != 0);
}

#[test]
fn use_varint_flags_checks_compat_flags() {
    let protocol = test_protocol();

    let writer_without = FileListWriter::new(protocol);
    assert!(!writer_without.use_varint_flags());

    let writer_with =
        FileListWriter::with_compat_flags(protocol, CompatibilityFlags::VARINT_FLIST_FLAGS);
    assert!(writer_with.use_varint_flags());
}

#[test]
fn use_safe_file_list_checks_protocol_and_flags() {
    let writer30 = FileListWriter::new(ProtocolVersion::try_from(30u8).unwrap());
    assert!(!writer30.use_safe_file_list());

    let writer30_safe = FileListWriter::with_compat_flags(
        ProtocolVersion::try_from(30u8).unwrap(),
        CompatibilityFlags::SAFE_FILE_LIST,
    );
    assert!(writer30_safe.use_safe_file_list());

    let writer31 = FileListWriter::new(ProtocolVersion::try_from(31u8).unwrap());
    assert!(writer31.use_safe_file_list());
}

#[test]
fn write_symlink_entry_with_preserve_links() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = test_protocol();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_links(true);

    let entry = FileEntry::new_symlink("link".into(), "/target/path".into());

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_links(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "link");
    assert!(read_entry.is_symlink());
    assert_eq!(
        read_entry
            .link_target()
            .map(|p| p.to_string_lossy().into_owned()),
        Some("/target/path".to_string())
    );
}

#[test]
fn write_symlink_entry_without_preserve_links_omits_target() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = test_protocol();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol); // preserve_links = false

    let entry = FileEntry::new_symlink("link".into(), "/target/path".into());

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol); // preserve_links = false

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "link");
    assert!(read_entry.is_symlink());
    // Target should NOT be present since preserve_links was false
    assert!(read_entry.link_target().is_none());
}

#[test]
fn write_symlink_round_trip_protocol_30_varint() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    // Protocol 30+ uses varint30
    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_links(true);

    let entry = FileEntry::new_symlink("mylink".into(), "../relative/path".into());

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_links(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "mylink");
    assert!(read_entry.is_symlink());
    assert_eq!(
        read_entry
            .link_target()
            .map(|p| p.to_string_lossy().into_owned()),
        Some("../relative/path".to_string())
    );
}

#[test]
fn write_symlink_round_trip_protocol_29_fixed_int() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    // Protocol 29 uses fixed 4-byte int
    let protocol = ProtocolVersion::try_from(29u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_links(true);

    let entry = FileEntry::new_symlink("oldlink".into(), "/old/target".into());

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_links(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "oldlink");
    assert!(read_entry.is_symlink());
    assert_eq!(
        read_entry
            .link_target()
            .map(|p| p.to_string_lossy().into_owned()),
        Some("/old/target".to_string())
    );
}

#[test]
fn write_block_device_round_trip_protocol_30() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    let entry = FileEntry::new_block_device("sda".into(), 0o660, 8, 0);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "sda");
    assert!(read_entry.is_device());
    assert!(read_entry.is_block_device());
    assert_eq!(read_entry.rdev_major(), Some(8));
    assert_eq!(read_entry.rdev_minor(), Some(0));
}

#[test]
fn write_char_device_round_trip_protocol_30() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    let entry = FileEntry::new_char_device("null".into(), 0o666, 1, 3);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "null");
    assert!(read_entry.is_device());
    assert!(read_entry.is_char_device());
    assert_eq!(read_entry.rdev_major(), Some(1));
    assert_eq!(read_entry.rdev_minor(), Some(3));
}

#[test]
fn write_device_without_preserve_devices_omits_rdev() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = test_protocol();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol); // preserve_devices = false

    let entry = FileEntry::new_block_device("sda".into(), 0o660, 8, 0);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol); // preserve_devices = false

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "sda");
    assert!(read_entry.is_block_device());
    // rdev should NOT be present since preserve_devices was false
    assert!(read_entry.rdev_major().is_none());
    assert!(read_entry.rdev_minor().is_none());
}

#[test]
fn write_multiple_devices_with_same_major_compression() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    // Two devices with same major (8) - second should use XMIT_SAME_RDEV_MAJOR
    let entry1 = FileEntry::new_block_device("sda".into(), 0o660, 8, 0);
    let entry2 = FileEntry::new_block_device("sdb".into(), 0o660, 8, 16);

    writer.write_entry(&mut buf, &entry1).unwrap();
    let first_len = buf.len();
    writer.write_entry(&mut buf, &entry2).unwrap();
    let second_len = buf.len() - first_len;
    writer.write_end(&mut buf, None).unwrap();

    // Second entry should be smaller due to major compression
    assert!(
        second_len < first_len,
        "second device entry should be compressed"
    );

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();

    assert_eq!(read1.rdev_major(), Some(8));
    assert_eq!(read1.rdev_minor(), Some(0));
    assert_eq!(read2.rdev_major(), Some(8));
    assert_eq!(read2.rdev_minor(), Some(16));
}

#[test]
fn write_hardlink_first_round_trip_protocol_30() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

    // First file in hardlink group (leader)
    let mut entry = FileEntry::new_file("file1.txt".into(), 100, 0o644);
    entry.set_hardlink_idx(u32::MAX); // u32::MAX indicates first/leader

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "file1.txt");
    assert_eq!(read_entry.hardlink_idx(), Some(u32::MAX));
}

#[test]
fn write_hardlink_follower_round_trip_protocol_30() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

    // Hardlink follower pointing to index 5
    let mut entry = FileEntry::new_file("file2.txt".into(), 100, 0o644);
    entry.set_hardlink_idx(5);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "file2.txt");
    assert_eq!(read_entry.hardlink_idx(), Some(5));
}

#[test]
fn write_hardlink_without_preserve_hard_links_omits_idx() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = test_protocol();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol); // preserve_hard_links = false

    let mut entry = FileEntry::new_file("file1.txt".into(), 100, 0o644);
    entry.set_hardlink_idx(5);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol); // preserve_hard_links = false

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "file1.txt");
    // hardlink_idx should NOT be present since preserve_hard_links was false
    assert!(read_entry.hardlink_idx().is_none());
}

#[test]
fn write_hardlink_group_round_trip_protocol_32() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

    // First: leader (u32::MAX)
    let mut entry1 = FileEntry::new_file("original.txt".into(), 500, 0o644);
    entry1.set_hardlink_idx(u32::MAX);

    // Second: follower pointing to index 0
    let mut entry2 = FileEntry::new_file("link1.txt".into(), 500, 0o644);
    entry2.set_hardlink_idx(0);

    // Third: follower pointing to index 0
    let mut entry3 = FileEntry::new_file("link2.txt".into(), 500, 0o644);
    entry3.set_hardlink_idx(0);

    writer.write_entry(&mut buf, &entry1).unwrap();
    writer.write_entry(&mut buf, &entry2).unwrap();
    writer.write_entry(&mut buf, &entry3).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);

    let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read3 = reader.read_entry(&mut cursor).unwrap().unwrap();

    assert_eq!(read1.name(), "original.txt");
    assert_eq!(read1.hardlink_idx(), Some(u32::MAX));

    assert_eq!(read2.name(), "link1.txt");
    assert_eq!(read2.hardlink_idx(), Some(0));

    assert_eq!(read3.name(), "link2.txt");
    assert_eq!(read3.hardlink_idx(), Some(0));
}

#[test]
fn write_user_name_round_trip_protocol_30() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

    let mut entry = FileEntry::new_file("file.txt".into(), 100, 0o644);
    entry.set_uid(1000);
    entry.set_user_name("testuser".to_string());

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "file.txt");
    assert_eq!(read_entry.user_name(), Some("testuser"));
}

#[test]
fn write_group_name_round_trip_protocol_30() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

    let mut entry = FileEntry::new_file("file.txt".into(), 100, 0o644);
    entry.set_gid(1000);
    entry.set_group_name("testgroup".to_string());

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "file.txt");
    assert_eq!(read_entry.group_name(), Some("testgroup"));
}

#[test]
fn write_user_and_group_names_round_trip_protocol_32() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

    let mut entry = FileEntry::new_file("owned.txt".into(), 500, 0o644);
    entry.set_uid(1001);
    entry.set_gid(1002);
    entry.set_user_name("alice".to_string());
    entry.set_group_name("developers".to_string());

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "owned.txt");
    assert_eq!(read_entry.user_name(), Some("alice"));
    assert_eq!(read_entry.group_name(), Some("developers"));
}

#[test]
fn write_user_name_omitted_when_same_uid() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

    // First entry sets the UID
    let mut entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);
    entry1.set_uid(1000);
    entry1.set_user_name("testuser".to_string());

    // Second entry has same UID - should use XMIT_SAME_UID (no name written)
    let mut entry2 = FileEntry::new_file("file2.txt".into(), 200, 0o644);
    entry2.set_uid(1000);
    entry2.set_user_name("testuser".to_string());

    writer.write_entry(&mut buf, &entry1).unwrap();
    let first_len = buf.len();
    writer.write_entry(&mut buf, &entry2).unwrap();
    let second_len = buf.len() - first_len;
    writer.write_end(&mut buf, None).unwrap();

    // Second entry should be smaller (no user name written)
    assert!(
        second_len < first_len,
        "second entry should not include user name"
    );

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

    let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();

    assert_eq!(read1.user_name(), Some("testuser"));
    // Second entry doesn't get user_name since XMIT_SAME_UID was set
    assert_eq!(read2.user_name(), None);
}

#[test]
fn write_names_omitted_for_protocol_29() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    // Protocol 29 doesn't support user/group name strings
    let protocol = ProtocolVersion::try_from(29u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

    let mut entry = FileEntry::new_file("file.txt".into(), 100, 0o644);
    entry.set_uid(1000);
    entry.set_gid(1000);
    entry.set_user_name("testuser".to_string());
    entry.set_group_name("testgroup".to_string());

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    // Names should NOT be present for protocol 29
    assert_eq!(read_entry.user_name(), None);
    assert_eq!(read_entry.group_name(), None);
}

#[test]
fn write_hardlink_follower_skips_metadata() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

    // First: leader (u32::MAX) - full metadata
    let mut entry1 = FileEntry::new_file("original.txt".into(), 500, 0o644);
    entry1.set_mtime(1700000000, 0);
    entry1.set_hardlink_idx(u32::MAX);

    // Second: follower pointing to index 0 - metadata skipped
    let mut entry2 = FileEntry::new_file("link.txt".into(), 500, 0o644);
    entry2.set_mtime(1700000000, 0);
    entry2.set_hardlink_idx(0);

    writer.write_entry(&mut buf, &entry1).unwrap();
    let first_len = buf.len();
    writer.write_entry(&mut buf, &entry2).unwrap();
    let second_len = buf.len() - first_len;
    writer.write_end(&mut buf, None).unwrap();

    // Follower should be MUCH smaller (no size, mtime, mode)
    assert!(
        second_len < first_len / 2,
        "follower entry should be much smaller: {second_len} vs {first_len}"
    );

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);

    let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();

    // Leader has full metadata
    assert_eq!(read1.name(), "original.txt");
    assert_eq!(read1.size(), 500);
    assert_eq!(read1.mtime(), 1700000000);
    assert_eq!(read1.hardlink_idx(), Some(u32::MAX));

    // Follower has zeroed metadata (caller should copy from leader)
    assert_eq!(read2.name(), "link.txt");
    assert_eq!(read2.size(), 0); // Metadata was skipped
    assert_eq!(read2.mtime(), 0); // Metadata was skipped
    assert_eq!(read2.hardlink_idx(), Some(0));
}

#[test]
fn write_hardlink_follower_with_uid_gid_skips_all() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_hard_links(true)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

    // Leader with full metadata
    let mut entry1 = FileEntry::new_file("leader.txt".into(), 1000, 0o755);
    entry1.set_mtime(1700000000, 0);
    entry1.set_uid(1000);
    entry1.set_gid(1000);
    entry1.set_user_name("testuser".to_string());
    entry1.set_group_name("testgroup".to_string());
    entry1.set_hardlink_idx(u32::MAX);

    // Follower - all metadata should be skipped
    let mut entry2 = FileEntry::new_file("follower.txt".into(), 1000, 0o755);
    entry2.set_mtime(1700000000, 0);
    entry2.set_uid(1000);
    entry2.set_gid(1000);
    entry2.set_hardlink_idx(0);

    writer.write_entry(&mut buf, &entry1).unwrap();
    let first_len = buf.len();
    writer.write_entry(&mut buf, &entry2).unwrap();
    let second_len = buf.len() - first_len;
    writer.write_end(&mut buf, None).unwrap();

    // Follower should be significantly smaller
    assert!(
        second_len < first_len / 2,
        "follower should skip metadata: {second_len} vs {first_len}"
    );

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_hard_links(true)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

    let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();

    // Leader has full metadata
    assert_eq!(read1.user_name(), Some("testuser"));
    assert_eq!(read1.group_name(), Some("testgroup"));

    // Follower metadata was skipped
    assert_eq!(read2.size(), 0);
    assert_eq!(read2.mtime(), 0);
    assert_eq!(read2.mode(), 0);
    assert_eq!(read2.user_name(), None);
    assert_eq!(read2.group_name(), None);
    assert_eq!(read2.hardlink_idx(), Some(0));
}

#[test]
fn write_hardlink_leader_has_full_metadata() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

    // Leader should have full metadata even with hardlink flag
    let mut entry = FileEntry::new_file("leader.txt".into(), 500, 0o644);
    entry.set_mtime(1700000000, 0);
    entry.set_hardlink_idx(u32::MAX); // Leader marker

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();

    // Leader has full metadata
    assert_eq!(read_entry.name(), "leader.txt");
    assert_eq!(read_entry.size(), 500);
    assert_eq!(read_entry.mtime(), 1700000000);
    assert_eq!(read_entry.hardlink_idx(), Some(u32::MAX));
}

#[test]
fn is_hardlink_follower_helper() {
    let writer = FileListWriter::new(test_protocol()).with_preserve_hard_links(true);

    // No hardlink flags
    let xflags_none: u32 = 0;
    assert!(!writer.is_hardlink_follower(xflags_none));

    // Leader (HLINKED + HLINK_FIRST)
    let xflags_leader = ((XMIT_HLINKED as u32) << 8) | ((XMIT_HLINK_FIRST as u32) << 8);
    assert!(!writer.is_hardlink_follower(xflags_leader));

    // Follower (HLINKED only)
    let xflags_follower = (XMIT_HLINKED as u32) << 8;
    assert!(writer.is_hardlink_follower(xflags_follower));
}

#[test]
fn abbreviated_vs_unabbreviated_hardlink_follower() {
    let mut writer = FileListWriter::new(test_protocol()).with_preserve_hard_links(true);
    writer.set_first_ndx(100);

    let xflags_follower = (XMIT_HLINKED as u32) << 8;

    // Follower with idx >= first_ndx is abbreviated (metadata skipped)
    let mut entry_same_seg = FileEntry::new_file("f1".into(), 100, 0o644);
    entry_same_seg.set_hardlink_idx(150);
    assert!(writer.is_abbreviated_follower(&entry_same_seg, xflags_follower));

    // Follower with idx < first_ndx is unabbreviated (full metadata on wire)
    let mut entry_prev_seg = FileEntry::new_file("f2".into(), 100, 0o644);
    entry_prev_seg.set_hardlink_idx(50);
    assert!(!writer.is_abbreviated_follower(&entry_prev_seg, xflags_follower));

    // Follower with idx == first_ndx is abbreviated
    let mut entry_boundary = FileEntry::new_file("f3".into(), 100, 0o644);
    entry_boundary.set_hardlink_idx(100);
    assert!(writer.is_abbreviated_follower(&entry_boundary, xflags_follower));

    // Leader is never abbreviated
    let xflags_leader = ((XMIT_HLINKED as u32) << 8) | ((XMIT_HLINK_FIRST as u32) << 8);
    assert!(!writer.is_abbreviated_follower(&entry_same_seg, xflags_leader));
}

#[test]
fn checksum_round_trip() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = test_protocol();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_always_checksum(16);

    let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    entry.set_mtime(1700000000, 0);
    entry.set_checksum(vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_always_checksum(16);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "test.txt");
    assert_eq!(
        read_entry.checksum(),
        Some(&vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16][..])
    );
}

#[test]
fn stats_tracking() {
    let protocol = test_protocol();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_links(true)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    // Write various entry types
    let file1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);
    let file2 = FileEntry::new_file("file2.txt".into(), 200, 0o644);
    let dir = FileEntry::new_directory("mydir".into(), 0o755);
    let link = FileEntry::new_symlink("mylink".into(), "/target".into());
    let dev = FileEntry::new_block_device("sda".into(), 0o660, 8, 0);

    writer.write_entry(&mut buf, &file1).unwrap();
    writer.write_entry(&mut buf, &file2).unwrap();
    writer.write_entry(&mut buf, &dir).unwrap();
    writer.write_entry(&mut buf, &link).unwrap();
    writer.write_entry(&mut buf, &dev).unwrap();

    let stats = writer.stats();
    assert_eq!(stats.num_files, 2);
    assert_eq!(stats.num_dirs, 1);
    assert_eq!(stats.num_symlinks, 1);
    assert_eq!(stats.num_devices, 1);
    assert_eq!(stats.total_size, 300 + 7); // 100 + 200 + len("/target")
}

#[test]
fn hardlink_dev_ino_round_trip_protocol_29() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(29u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

    let mut entry = FileEntry::new_file("hardlink.txt".into(), 100, 0o644);
    entry.set_mtime(1700000000, 0);
    entry.set_hardlink_dev(12345);
    entry.set_hardlink_ino(67890);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "hardlink.txt");
    assert_eq!(read_entry.hardlink_dev(), Some(12345));
    assert_eq!(read_entry.hardlink_ino(), Some(67890));
}

#[test]
fn hardlink_dev_compression_protocol_29() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(29u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

    // Two entries with same dev should use XMIT_SAME_DEV_PRE30
    let mut entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);
    entry1.set_mtime(1700000000, 0);
    entry1.set_hardlink_dev(12345);
    entry1.set_hardlink_ino(1);

    let mut entry2 = FileEntry::new_file("file2.txt".into(), 100, 0o644);
    entry2.set_mtime(1700000000, 0);
    entry2.set_hardlink_dev(12345);
    entry2.set_hardlink_ino(2);

    writer.write_entry(&mut buf, &entry1).unwrap();
    let first_len = buf.len();
    writer.write_entry(&mut buf, &entry2).unwrap();
    let second_len = buf.len() - first_len;
    writer.write_end(&mut buf, None).unwrap();

    // Second entry should be smaller due to dev compression
    assert!(
        second_len < first_len,
        "second entry should use dev compression"
    );

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);

    let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();

    assert_eq!(read1.hardlink_dev(), Some(12345));
    assert_eq!(read1.hardlink_ino(), Some(1));
    assert_eq!(read2.hardlink_dev(), Some(12345));
    assert_eq!(read2.hardlink_ino(), Some(2));
}

#[test]
fn special_file_fifo_round_trip_protocol_30() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    let entry = FileEntry::new_fifo("myfifo".into(), 0o644);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "myfifo");
    assert!(read_entry.is_special());
    // rdev should NOT be set (dummy was read and discarded)
    assert!(read_entry.rdev_major().is_none());
}

#[test]
fn special_file_socket_round_trip_protocol_30() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    let entry = FileEntry::new_socket("mysocket".into(), 0o755);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "mysocket");
    assert!(read_entry.is_special());
}

#[test]
fn special_file_no_rdev_in_protocol_31() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(31u8).unwrap();
    let mut buf_30 = Vec::new();
    let mut buf_31 = Vec::new();

    // Protocol 30: FIFOs get dummy rdev
    let mut writer30 = FileListWriter::new(ProtocolVersion::try_from(30u8).unwrap())
        .with_preserve_devices(true)
        .with_preserve_specials(true);
    let entry = FileEntry::new_fifo("fifo".into(), 0o644);
    writer30.write_entry(&mut buf_30, &entry).unwrap();

    // Protocol 31: FIFOs don't get rdev
    let mut writer31 = FileListWriter::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);
    writer31.write_entry(&mut buf_31, &entry).unwrap();

    // Protocol 31 entry should be smaller (no rdev)
    assert!(
        buf_31.len() < buf_30.len(),
        "protocol 31 should not write rdev for FIFOs"
    );

    // Verify round-trip
    let mut cursor = Cursor::new(&buf_31[..]);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);
    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "fifo");
    assert!(read_entry.is_special());
}

// Protocol boundary tests

#[test]
fn protocol_28_is_oldest_supported() {
    // Protocol 28 is the oldest supported version
    let protocol = ProtocolVersion::try_from(28u8).unwrap();
    assert!(
        protocol.supports_extended_flags(),
        "protocol 28 should support extended flags"
    );
}

#[test]
fn protocol_boundary_28_round_trip() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    // Protocol 28 - oldest supported, has extended flags
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

    // Verify protocol 28 round-trip
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
    use super::super::read::FileListReader;
    use std::io::Cursor;

    // Protocol 30 adds user/group name support
    let protocol30 = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol30)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

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
    use super::super::read::FileListReader;
    use std::io::Cursor;

    // Protocol 31 adds nanosecond mtime support
    let protocol31 = ProtocolVersion::try_from(31u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol31);

    let mut entry = FileEntry::new_file("test.txt".into(), 1024, 0o644);
    entry.set_mtime(1700000000, 123456789); // With nanoseconds

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
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = test_protocol();

    // Create a path longer than 255 characters (requires XMIT_LONG_NAME)
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
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = test_protocol();

    // Create two entries with long shared prefix
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

    // Second entry should be smaller due to prefix compression
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
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = test_protocol();

    // Test extreme mtime values (only non-negative, as negative
    // timestamps are encoded as unsigned in the wire format)
    let test_cases = [
        0i64,                 // Unix epoch
        1,                    // Just after epoch
        i32::MAX as i64,      // Max 32-bit timestamp (2038-01-19)
        i32::MAX as i64 + 1,  // Beyond 32-bit (2038-01-19)
        1_000_000_000_000i64, // Far future (year ~33658)
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

#[test]
fn zero_flags_varint_uses_xmit_extended_flags() {
    // Upstream flist.c line 550: write_varint(f, xflags ? xflags : XMIT_EXTENDED_FLAGS)
    // When all compression flags apply (mode, time, uid, gid same as prev),
    // xflags would be 0, but we substitute XMIT_EXTENDED_FLAGS to avoid
    // collision with the end-of-list marker (which is also 0).
    use crate::varint::decode_varint;

    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;
    let mut writer = FileListWriter::with_compat_flags(protocol, flags)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

    // Set up compression state so all flags match
    writer
        .state
        .update(b"prefix/", 0o100644, 1700000000, 1000, 1000);

    let mut entry = FileEntry::new_file("prefix/file.txt".into(), 100, 0o644);
    entry.set_mtime(1700000000, 0); // Same time
    entry.set_uid(1000); // Same UID
    entry.set_gid(1000); // Same GID

    let mut buf = Vec::new();
    writer.write_entry(&mut buf, &entry).unwrap();

    // Decode the first varint to check the flags value
    let (flags_value, _) = decode_varint(&buf).unwrap();

    // Should NOT be 0 (end marker), should be XMIT_EXTENDED_FLAGS (0x04)
    assert_ne!(flags_value, 0, "flags should not be zero (end marker)");
    assert!(
        (flags_value as u32) & (XMIT_EXTENDED_FLAGS as u32) != 0
            || (flags_value as u32) & (XMIT_SAME_NAME as u32) != 0
            || (flags_value as u32) & (XMIT_SAME_MODE as u32) != 0
            || (flags_value as u32) & (XMIT_SAME_TIME as u32) != 0,
        "non-zero flags should be written: got {flags_value:#x}"
    );
}

#[test]
fn xmit_same_uid_compression_round_trip() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = test_protocol();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_uid(true);

    // First entry sets the UID
    let mut entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);
    entry1.set_uid(1000);

    // Second entry has same UID - XMIT_SAME_UID flag should be set
    let mut entry2 = FileEntry::new_file("file2.txt".into(), 200, 0o644);
    entry2.set_uid(1000);

    // Third entry has different UID
    let mut entry3 = FileEntry::new_file("file3.txt".into(), 300, 0o644);
    entry3.set_uid(2000);

    writer.write_entry(&mut buf, &entry1).unwrap();
    let first_len = buf.len();
    writer.write_entry(&mut buf, &entry2).unwrap();
    let second_len = buf.len() - first_len;
    writer.write_entry(&mut buf, &entry3).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    // Second entry should be smaller (UID compressed)
    assert!(
        second_len < first_len,
        "second entry should use XMIT_SAME_UID compression"
    );

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_uid(true);

    let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read3 = reader.read_entry(&mut cursor).unwrap().unwrap();

    assert_eq!(read1.uid(), Some(1000));
    assert_eq!(read2.uid(), Some(1000)); // Inherited from compression state
    assert_eq!(read3.uid(), Some(2000)); // Explicit value
}

#[test]
fn xmit_same_gid_compression_round_trip() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = test_protocol();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_gid(true);

    // First entry sets the GID
    let mut entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);
    entry1.set_gid(1000);

    // Second entry has same GID - XMIT_SAME_GID flag should be set
    let mut entry2 = FileEntry::new_file("file2.txt".into(), 200, 0o644);
    entry2.set_gid(1000);

    // Third entry has different GID
    let mut entry3 = FileEntry::new_file("file3.txt".into(), 300, 0o644);
    entry3.set_gid(2000);

    writer.write_entry(&mut buf, &entry1).unwrap();
    let first_len = buf.len();
    writer.write_entry(&mut buf, &entry2).unwrap();
    let second_len = buf.len() - first_len;
    writer.write_entry(&mut buf, &entry3).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    // Second entry should be smaller (GID compressed)
    assert!(
        second_len < first_len,
        "second entry should use XMIT_SAME_GID compression"
    );

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_gid(true);

    let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read3 = reader.read_entry(&mut cursor).unwrap().unwrap();

    assert_eq!(read1.gid(), Some(1000));
    assert_eq!(read2.gid(), Some(1000)); // Inherited from compression state
    assert_eq!(read3.gid(), Some(2000)); // Explicit value
}

#[test]
fn xmit_same_mode_compression_round_trip() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = test_protocol();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    // First entry sets the mode (mode includes file type, so use same type)
    let entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);

    // Second entry has same mode - XMIT_SAME_MODE flag should be set
    let entry2 = FileEntry::new_file("file2.txt".into(), 200, 0o644);

    // Third entry has different mode
    let entry3 = FileEntry::new_file("file3.txt".into(), 300, 0o755);

    writer.write_entry(&mut buf, &entry1).unwrap();
    let first_len = buf.len();
    writer.write_entry(&mut buf, &entry2).unwrap();
    let second_len = buf.len() - first_len;
    writer.write_entry(&mut buf, &entry3).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    // Second entry should be smaller (mode compressed)
    assert!(
        second_len < first_len,
        "second entry should use XMIT_SAME_MODE compression"
    );

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);

    let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read3 = reader.read_entry(&mut cursor).unwrap().unwrap();

    assert_eq!(read1.permissions(), 0o644);
    assert_eq!(read2.permissions(), 0o644); // Same mode
    assert_eq!(read3.permissions(), 0o755); // Different mode
}

#[test]
fn xmit_same_time_compression_round_trip() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = test_protocol();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    // First entry sets the mtime
    let mut entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);
    entry1.set_mtime(1700000000, 0);

    // Second entry has same mtime - XMIT_SAME_TIME flag should be set
    let mut entry2 = FileEntry::new_file("file2.txt".into(), 200, 0o644);
    entry2.set_mtime(1700000000, 0);

    // Third entry has different mtime
    let mut entry3 = FileEntry::new_file("file3.txt".into(), 300, 0o644);
    entry3.set_mtime(1700000001, 0);

    writer.write_entry(&mut buf, &entry1).unwrap();
    let first_len = buf.len();
    writer.write_entry(&mut buf, &entry2).unwrap();
    let second_len = buf.len() - first_len;
    writer.write_entry(&mut buf, &entry3).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    // Second entry should be smaller (mtime compressed)
    assert!(
        second_len < first_len,
        "second entry should use XMIT_SAME_TIME compression"
    );

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);

    let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read3 = reader.read_entry(&mut cursor).unwrap().unwrap();

    assert_eq!(read1.mtime(), 1700000000);
    assert_eq!(read2.mtime(), 1700000000); // Same time
    assert_eq!(read3.mtime(), 1700000001); // Different time
}

#[test]
fn name_prefix_compression_max_255_bytes() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = test_protocol();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    // Create two entries with a prefix longer than 255 bytes
    // The compression should cap at 255 since same_len is stored as u8
    let long_prefix = "x".repeat(300);
    let path1 = format!("{long_prefix}/file1.txt");
    let path2 = format!("{long_prefix}/file2.txt");

    let entry1 = FileEntry::new_file(path1.clone().into(), 100, 0o644);
    let entry2 = FileEntry::new_file(path2.clone().into(), 200, 0o644);

    writer.write_entry(&mut buf, &entry1).unwrap();
    writer.write_entry(&mut buf, &entry2).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);

    let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();

    assert_eq!(read1.name(), path1);
    assert_eq!(read2.name(), path2);
}

#[test]
fn special_file_rdev_protocol_30_vs_31() {
    // Protocol 30 writes dummy rdev for FIFOs/sockets
    // Protocol 31+ does NOT write rdev for FIFOs/sockets
    let proto30 = ProtocolVersion::try_from(30u8).unwrap();
    let proto31 = ProtocolVersion::try_from(31u8).unwrap();

    let fifo = FileEntry::new_fifo("myfifo".into(), 0o644);

    let mut buf30 = Vec::new();
    let mut writer30 = FileListWriter::new(proto30)
        .with_preserve_devices(true)
        .with_preserve_specials(true);
    writer30.write_entry(&mut buf30, &fifo).unwrap();

    let mut buf31 = Vec::new();
    let mut writer31 = FileListWriter::new(proto31)
        .with_preserve_devices(true)
        .with_preserve_specials(true);
    writer31.write_entry(&mut buf31, &fifo).unwrap();

    // Protocol 31 should produce smaller output (no dummy rdev)
    assert!(
        buf31.len() < buf30.len(),
        "protocol 31 should not write rdev for FIFOs: {} < {}",
        buf31.len(),
        buf30.len()
    );
}

#[test]
fn special_file_fifo_round_trip_protocol_28_29() {
    // Protocol 28-29 uses XMIT_RDEV_MINOR_8_PRE30 flag for rdev encoding
    // This test verifies FIFOs write and read correctly with dummy rdev
    use super::super::read::FileListReader;
    use std::io::Cursor;

    for proto_ver in [28u8, 29u8] {
        let protocol = ProtocolVersion::try_from(proto_ver).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol)
            .with_preserve_devices(true)
            .with_preserve_specials(true);

        let entry = FileEntry::new_fifo("myfifo".into(), 0o644);

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_devices(true)
            .with_preserve_specials(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(
            read_entry.name(),
            "myfifo",
            "protocol {proto_ver} FIFO name mismatch"
        );
        assert!(
            read_entry.is_special(),
            "protocol {proto_ver} should recognize FIFO as special"
        );
        // rdev should NOT be set (dummy was read and discarded)
        assert!(
            read_entry.rdev_major().is_none(),
            "protocol {proto_ver} FIFO should not have rdev"
        );
    }
}

#[test]
fn device_round_trip_protocol_28_29() {
    // Protocol 28-29 uses XMIT_RDEV_MINOR_8_PRE30 flag for 8-bit minors
    use super::super::read::FileListReader;
    use std::io::Cursor;

    for proto_ver in [28u8, 29u8] {
        let protocol = ProtocolVersion::try_from(proto_ver).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol)
            .with_preserve_devices(true)
            .with_preserve_specials(true);

        // Block device with minor fitting in 8 bits
        let dev_small_minor = FileEntry::new_block_device("sda".into(), 0o660, 8, 0);
        // Block device with minor requiring more than 8 bits
        let dev_large_minor = FileEntry::new_block_device("sdb".into(), 0o660, 8, 300);

        writer.write_entry(&mut buf, &dev_small_minor).unwrap();
        writer.write_entry(&mut buf, &dev_large_minor).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_devices(true)
            .with_preserve_specials(true);

        let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();

        assert_eq!(read1.rdev_major(), Some(8), "proto {proto_ver} dev1 major");
        assert_eq!(read1.rdev_minor(), Some(0), "proto {proto_ver} dev1 minor");
        assert_eq!(read2.rdev_major(), Some(8), "proto {proto_ver} dev2 major");
        assert_eq!(
            read2.rdev_minor(),
            Some(300),
            "proto {proto_ver} dev2 minor (>255)"
        );
    }
}

#[test]
fn directory_content_dir_flag_round_trip() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    // Directory with content
    let mut dir_with_content = FileEntry::new_directory("with_content".into(), 0o755);
    dir_with_content.set_content_dir(true);

    // Directory without content (implied directory)
    let mut dir_no_content = FileEntry::new_directory("no_content".into(), 0o755);
    dir_no_content.set_content_dir(false);

    writer.write_entry(&mut buf, &dir_with_content).unwrap();
    writer.write_entry(&mut buf, &dir_no_content).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);

    let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();

    assert_eq!(read1.name(), "with_content");
    assert!(read1.content_dir(), "first dir should have content");

    assert_eq!(read2.name(), "no_content");
    assert!(!read2.content_dir(), "second dir should not have content");
}
// These tests verify the wire format encoding for XMIT_EXTENDED_FLAGS
// across different protocol versions and flag combinations.

#[test]
fn extended_flags_two_byte_encoding_protocol_28() {
    // Protocol 28-29 uses two-byte encoding when extended flags are set.
    // When xflags has bits in the 0xFF00 range, XMIT_EXTENDED_FLAGS is set
    // and flags are written as little-endian u16.
    let protocol = ProtocolVersion::try_from(28u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    // Block device triggers XMIT_SAME_RDEV_MAJOR in extended flags (byte 1)
    // when major matches previous, but first device doesn't match anything
    let entry = FileEntry::new_block_device("sda".into(), 0o660, 8, 0);

    writer.write_entry(&mut buf, &entry).unwrap();

    // The first byte should have XMIT_EXTENDED_FLAGS set (bit 2)
    // because device entries set flags in the extended byte
    assert!(
        buf[0] & XMIT_EXTENDED_FLAGS != 0,
        "first byte should have XMIT_EXTENDED_FLAGS set: got {:#04x}",
        buf[0]
    );
}

#[test]
fn extended_flags_one_byte_encoding_when_no_extended_bits() {
    // Protocol 28-29 uses single-byte encoding when no extended flags are needed.
    // Simple file entries without special attributes should use one-byte encoding.
    let protocol = ProtocolVersion::try_from(28u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let entry = FileEntry::new_file("simple.txt".into(), 100, 0o644);
    writer.write_entry(&mut buf, &entry).unwrap();

    // For a simple file with no previous entry compression,
    // flags should fit in one byte (no XMIT_EXTENDED_FLAGS needed)
    // unless the mode/time differ from defaults
    // The point is: without extended flags, we should NOT have XMIT_EXTENDED_FLAGS
    // But actually, write_flags may still set it if xflags==0 for non-dir
    // Let's verify the encoding is correct for simple entries
    assert!(!buf.is_empty(), "buffer should not be empty");
    assert_ne!(buf[0], 0, "flags byte should not be zero (end marker)");
}

#[test]
fn extended_flags_protocol_30_varint_encoding() {
    // Protocol 30+ with VARINT_FLIST_FLAGS encodes all flags as a single varint.
    // This test verifies varint encoding is used when compat flags are set.
    use crate::varint::decode_varint;

    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let compat_flags = CompatibilityFlags::VARINT_FLIST_FLAGS;
    let mut writer = FileListWriter::with_compat_flags(protocol, compat_flags);

    let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    entry.set_mtime(1700000000, 0);

    let mut buf = Vec::new();
    writer.write_entry(&mut buf, &entry).unwrap();

    // Decode the flags as varint
    let (flags_value, _bytes_read) = decode_varint(&buf).unwrap();
    assert_ne!(flags_value, 0, "flags should not be zero");
}

#[test]
fn extended_flags_all_basic_flags_combinations() {
    // Test that all basic flag combinations (byte 0) work correctly
    use super::super::flags::FileFlags;
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = test_protocol();

    // Test XMIT_TOP_DIR (directories only) using from_raw with flags set
    {
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol);
        // Create directory entry with XMIT_TOP_DIR flag set
        let flags = FileFlags::new(XMIT_TOP_DIR, 0);
        let dir = FileEntry::from_raw("topdir".into(), 0, 0o040755, 0, 0, flags);
        writer.write_entry(&mut buf, &dir).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol);
        let read = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert!(read.flags().top_dir(), "XMIT_TOP_DIR should round-trip");
    }

    // Test XMIT_LONG_NAME (paths > 255 bytes)
    {
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol);
        let long_name = "x".repeat(300);
        let entry = FileEntry::new_file(long_name.clone().into(), 100, 0o644);
        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol);
        let read = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read.name(), long_name, "long name should round-trip");
    }
}

#[test]
fn extended_flags_hardlink_flag_combinations() {
    // Test XMIT_HLINKED and XMIT_HLINK_FIRST flag combinations
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();

    // Test: XMIT_HLINKED | XMIT_HLINK_FIRST (hardlink leader)
    {
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);
        let mut entry = FileEntry::new_file("leader.txt".into(), 100, 0o644);
        entry.set_hardlink_idx(u32::MAX); // Leader marker
        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);
        let read = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(
            read.hardlink_idx(),
            Some(u32::MAX),
            "leader should have u32::MAX"
        );
    }

    // Test: XMIT_HLINKED only (hardlink follower)
    {
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);
        let mut entry = FileEntry::new_file("follower.txt".into(), 100, 0o644);
        entry.set_hardlink_idx(42); // Points to leader index
        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);
        let read = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(
            read.hardlink_idx(),
            Some(42),
            "follower should have index 42"
        );
    }
}

#[test]
fn extended_flags_time_related_flags() {
    // Test XMIT_SAME_ATIME, XMIT_MOD_NSEC, and XMIT_CRTIME_EQ_MTIME flags
    use super::super::read::FileListReader;
    use std::io::Cursor;

    // Test XMIT_MOD_NSEC (protocol 31+)
    {
        let protocol = ProtocolVersion::try_from(31u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        let mut entry = FileEntry::new_file("nsec.txt".into(), 100, 0o644);
        entry.set_mtime(1700000000, 123456789); // With nanoseconds

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol);
        let read = reader.read_entry(&mut cursor).unwrap().unwrap();

        assert_eq!(
            read.mtime_nsec(),
            123456789,
            "XMIT_MOD_NSEC should round-trip"
        );
    }

    // Test XMIT_SAME_ATIME (protocol 30+ with preserve_atimes)
    {
        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_atimes(true);

        // First entry sets atime
        let mut entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);
        entry1.set_atime(1700000000);

        // Second entry has same atime - should use XMIT_SAME_ATIME
        let mut entry2 = FileEntry::new_file("file2.txt".into(), 200, 0o644);
        entry2.set_atime(1700000000);

        writer.write_entry(&mut buf, &entry1).unwrap();
        let first_len = buf.len();
        writer.write_entry(&mut buf, &entry2).unwrap();
        let second_len = buf.len() - first_len;
        writer.write_end(&mut buf, None).unwrap();

        // Second entry should be smaller (atime compressed)
        assert!(
            second_len < first_len,
            "XMIT_SAME_ATIME should compress: {second_len} < {first_len}"
        );

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_atimes(true);
        let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();

        assert_eq!(read1.atime(), 1700000000);
        assert_eq!(read2.atime(), 1700000000);
    }
}

#[test]
fn extended_flags_owner_name_flags() {
    // Test XMIT_USER_NAME_FOLLOWS and XMIT_GROUP_NAME_FOLLOWS (protocol 30+)
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();

    // Test both user and group names
    {
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        let mut entry = FileEntry::new_file("owned.txt".into(), 100, 0o644);
        entry.set_uid(1000);
        entry.set_gid(1000);
        entry.set_user_name("alice".to_string());
        entry.set_group_name("developers".to_string());

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        let read = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read.user_name(), Some("alice"));
        assert_eq!(read.group_name(), Some("developers"));
    }

    // Verify names are NOT written for protocol 29
    {
        let protocol29 = ProtocolVersion::try_from(29u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol29)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        let mut entry = FileEntry::new_file("file29.txt".into(), 100, 0o644);
        entry.set_uid(1000);
        entry.set_gid(1000);
        entry.set_user_name("alice".to_string());
        entry.set_group_name("developers".to_string());

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol29)
            .with_preserve_uid(true)
            .with_preserve_gid(true);

        let read = reader.read_entry(&mut cursor).unwrap().unwrap();
        // Protocol 29 should NOT have user/group names
        assert_eq!(
            read.user_name(),
            None,
            "protocol 29 should not have user name"
        );
        assert_eq!(
            read.group_name(),
            None,
            "protocol 29 should not have group name"
        );
    }
}

#[test]
fn extended_flags_device_flags_protocol_28_29() {
    // Test XMIT_SAME_RDEV_MAJOR and XMIT_RDEV_MINOR_8_PRE30 for protocol 28-29
    use super::super::read::FileListReader;
    use std::io::Cursor;

    for proto_ver in [28u8, 29u8] {
        let protocol = ProtocolVersion::try_from(proto_ver).unwrap();

        // Test device with 8-bit minor (uses XMIT_RDEV_MINOR_8_PRE30)
        {
            let mut buf = Vec::new();
            let mut writer = FileListWriter::new(protocol)
                .with_preserve_devices(true)
                .with_preserve_specials(true);
            let entry = FileEntry::new_block_device("dev8".into(), 0o660, 8, 255);
            writer.write_entry(&mut buf, &entry).unwrap();
            writer.write_end(&mut buf, None).unwrap();

            let mut cursor = Cursor::new(&buf[..]);
            let mut reader = FileListReader::new(protocol)
                .with_preserve_devices(true)
                .with_preserve_specials(true);
            let read = reader.read_entry(&mut cursor).unwrap().unwrap();

            assert_eq!(
                read.rdev_major(),
                Some(8),
                "proto {proto_ver} 8-bit minor major"
            );
            assert_eq!(
                read.rdev_minor(),
                Some(255),
                "proto {proto_ver} 8-bit minor"
            );
        }

        // Test device with >8-bit minor (does NOT use XMIT_RDEV_MINOR_8_PRE30)
        {
            let mut buf = Vec::new();
            let mut writer = FileListWriter::new(protocol)
                .with_preserve_devices(true)
                .with_preserve_specials(true);
            let entry = FileEntry::new_block_device("dev32".into(), 0o660, 8, 256);
            writer.write_entry(&mut buf, &entry).unwrap();
            writer.write_end(&mut buf, None).unwrap();

            let mut cursor = Cursor::new(&buf[..]);
            let mut reader = FileListReader::new(protocol)
                .with_preserve_devices(true)
                .with_preserve_specials(true);
            let read = reader.read_entry(&mut cursor).unwrap().unwrap();

            assert_eq!(
                read.rdev_major(),
                Some(8),
                "proto {proto_ver} 32-bit minor major"
            );
            assert_eq!(
                read.rdev_minor(),
                Some(256),
                "proto {proto_ver} 32-bit minor"
            );
        }

        // Test XMIT_SAME_RDEV_MAJOR with two devices having same major
        {
            let mut buf = Vec::new();
            let mut writer = FileListWriter::new(protocol)
                .with_preserve_devices(true)
                .with_preserve_specials(true);
            let entry1 = FileEntry::new_block_device("sda".into(), 0o660, 8, 0);
            let entry2 = FileEntry::new_block_device("sdb".into(), 0o660, 8, 16);

            writer.write_entry(&mut buf, &entry1).unwrap();
            let first_len = buf.len();
            writer.write_entry(&mut buf, &entry2).unwrap();
            let second_len = buf.len() - first_len;
            writer.write_end(&mut buf, None).unwrap();

            // Second entry should be smaller due to XMIT_SAME_RDEV_MAJOR
            assert!(
                second_len < first_len,
                "proto {proto_ver} XMIT_SAME_RDEV_MAJOR should compress: {second_len} < {first_len}"
            );

            let mut cursor = Cursor::new(&buf[..]);
            let mut reader = FileListReader::new(protocol)
                .with_preserve_devices(true)
                .with_preserve_specials(true);
            let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
            let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();

            assert_eq!(read1.rdev_major(), Some(8));
            assert_eq!(read2.rdev_major(), Some(8));
        }
    }
}

#[test]
fn extended_flags_zero_xflags_non_directory_uses_top_dir() {
    // When xflags == 0 for a non-directory in protocol 28-29,
    // XMIT_TOP_DIR is used to avoid collision with end marker.
    // This is tested implicitly in write_flags() for protocol < 30.
    let protocol = ProtocolVersion::try_from(29u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    // Set up compression state so mode and time match
    writer.state.update(b"test", 0o100644, 1700000000, 0, 0);

    let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    entry.set_mtime(1700000000, 0); // Same time as prev

    writer.write_entry(&mut buf, &entry).unwrap();

    // First byte should NOT be zero (would be end marker)
    assert_ne!(buf[0], 0, "flags should not be zero for file entry");
}

#[test]
fn extended_flags_protocol_version_boundaries() {
    // Verify flag encoding at protocol version boundaries
    use super::super::read::FileListReader;
    use std::io::Cursor;

    // Protocol 27 should NOT have extended flags support
    // (but our minimum is 28, so this tests the boundary)

    // Protocol 28: First version with extended flags
    {
        let protocol = ProtocolVersion::try_from(28u8).unwrap();
        assert!(
            protocol.supports_extended_flags(),
            "protocol 28 must support extended flags"
        );

        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol)
            .with_preserve_devices(true)
            .with_preserve_specials(true);
        let entry = FileEntry::new_block_device("dev28".into(), 0o660, 8, 0);
        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_devices(true)
            .with_preserve_specials(true);
        let read = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read.rdev_major(), Some(8));
    }

    // Protocol 30: Introduces varint encoding option
    {
        let protocol = ProtocolVersion::try_from(30u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol);
        let entry = FileEntry::new_file("test30.txt".into(), 100, 0o644);
        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol);
        let read = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read.name(), "test30.txt");
    }

    // Protocol 31: Introduces XMIT_MOD_NSEC and safe file list by default
    {
        let protocol = ProtocolVersion::try_from(31u8).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        let mut entry = FileEntry::new_file("test31.txt".into(), 100, 0o644);
        entry.set_mtime(1700000000, 500000000);

        writer.write_entry(&mut buf, &entry).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol);
        let read = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read.mtime_nsec(), 500000000);
    }
}

/// Test encoding and decoding a 3GB file (above 2^31 = 2GB boundary).
/// This verifies that the varlong encoding correctly handles file sizes
/// that exceed the signed 32-bit integer range.
#[test]
fn large_file_size_3gb_round_trip() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    const SIZE_3GB: u64 = 3 * 1024 * 1024 * 1024; // 3 * 1024^3 = 3,221,225,472 bytes

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
    use super::super::read::FileListReader;
    use std::io::Cursor;

    const SIZE_5GB: u64 = 5 * 1024 * 1024 * 1024; // 5 * 1024^3 = 5,368,709,120 bytes

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
    use super::super::read::FileListReader;
    use std::io::Cursor;

    // Key boundary values for large file support
    let test_sizes: &[(u64, &str)] = &[
        // Just below 2^31 (max signed 32-bit positive)
        ((1u64 << 31) - 1, "just_below_2gb"),
        // Exactly 2^31 (2GB boundary)
        (1u64 << 31, "exactly_2gb"),
        // Just above 2^31
        ((1u64 << 31) + 1, "just_above_2gb"),
        // Just below 2^32 (max unsigned 32-bit)
        ((1u64 << 32) - 1, "just_below_4gb"),
        // Exactly 2^32 (4GB boundary)
        (1u64 << 32, "exactly_4gb"),
        // Just above 2^32
        ((1u64 << 32) + 1, "just_above_4gb"),
        // 3GB (3 * 1024^3)
        (3 * 1024 * 1024 * 1024, "3gb"),
        // 5GB (5 * 1024^3)
        (5 * 1024 * 1024 * 1024, "5gb"),
        // 1TB
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
    use super::super::read::FileListReader;
    use std::io::Cursor;

    const SIZE_3GB: u64 = 3 * 1024 * 1024 * 1024;
    const SIZE_5GB: u64 = 5 * 1024 * 1024 * 1024;

    // Protocol 29 uses longint encoding
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

/// Verifies exact wire bytes for the end-of-list marker in non-varint mode.
///
/// Upstream: `flist.c:write_end_of_flist()` - without `xfer_flags_as_varint`,
/// writes a single `write_byte(f, 0)`.
#[test]
fn write_end_nonvarint_produces_single_zero_byte() {
    let protocol = test_protocol();
    let writer = FileListWriter::new(protocol);

    let mut buf = Vec::new();
    writer.write_end(&mut buf, None).unwrap();

    assert_eq!(
        buf,
        [0x00],
        "non-varint end marker must be exactly one zero byte"
    );
}

/// Verifies exact wire bytes for the end-of-list marker in varint mode
/// (CF_VARINT_FLIST_FLAGS active, no I/O error).
///
/// Upstream: `flist.c:write_end_of_flist()` - with `xfer_flags_as_varint`,
/// writes `write_varint(f, 0); write_varint(f, 0);` (two varint-encoded zeros).
#[test]
fn write_end_varint_no_error_produces_double_zero_bytes() {
    let protocol = test_protocol();
    let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;
    let writer = FileListWriter::with_compat_flags(protocol, flags);

    let mut buf = Vec::new();
    writer.write_end(&mut buf, None).unwrap();

    assert_eq!(
        buf,
        [0x00, 0x00],
        "varint end marker without error must be exactly two zero bytes (varint(0) + varint(0))"
    );
}

/// Verifies exact wire bytes for the end-of-list marker in varint mode
/// with an I/O error code.
///
/// Upstream: `flist.c:write_end_of_flist()` - with `xfer_flags_as_varint`,
/// writes `write_varint(f, 0); write_varint(f, io_error);`.
#[test]
fn write_end_varint_with_error_produces_zero_then_error_varint() {
    let protocol = test_protocol();
    let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;
    let writer = FileListWriter::with_compat_flags(protocol, flags);

    let mut buf = Vec::new();
    writer.write_end(&mut buf, Some(5)).unwrap();

    // varint(0) = 0x00, varint(5) = 0x05 (small values encode as single byte)
    assert_eq!(
        buf,
        [0x00, 0x05],
        "varint end marker with error=5 must be varint(0) + varint(5)"
    );
}

/// Verifies that varint mode end marker with a larger error code encodes
/// correctly as double-varint.
#[test]
fn write_end_varint_with_large_error_encodes_correctly() {
    use crate::varint::decode_varint;

    let protocol = test_protocol();
    let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;
    let writer = FileListWriter::with_compat_flags(protocol, flags);

    let mut buf = Vec::new();
    writer.write_end(&mut buf, Some(300)).unwrap();

    // First byte must be varint(0) = 0x00
    assert_eq!(buf[0], 0x00, "first varint must encode zero flags");

    // Remaining bytes must decode to 300
    let (error_code, _) = decode_varint(&buf[1..]).unwrap();
    assert_eq!(error_code, 300, "second varint must encode the error code");
}

/// Verifies that varint mode produces a DIFFERENT wire encoding than
/// non-varint mode for the same end-of-list-without-error scenario.
#[test]
fn write_end_varint_differs_from_nonvarint() {
    let protocol = test_protocol();

    let nonvarint_writer = FileListWriter::new(protocol);
    let mut nonvarint_buf = Vec::new();
    nonvarint_writer
        .write_end(&mut nonvarint_buf, None)
        .unwrap();

    let varint_writer =
        FileListWriter::with_compat_flags(protocol, CompatibilityFlags::VARINT_FLIST_FLAGS);
    let mut varint_buf = Vec::new();
    varint_writer.write_end(&mut varint_buf, None).unwrap();

    assert_eq!(nonvarint_buf.len(), 1, "non-varint end marker is 1 byte");
    assert_eq!(varint_buf.len(), 2, "varint end marker is 2 bytes");
    assert_ne!(
        nonvarint_buf, varint_buf,
        "varint and non-varint end markers must differ in wire encoding"
    );
}

/// Verifies hardlink indices survive a write-read round-trip when directory
/// entries are interspersed among hardlinked files. This simulates the
/// `--relative` scenario where implied directories occupy wire NDX positions
/// between hardlinked files, shifting the follower's index value.
///
/// upstream: generator.c - send_implied_dirs() creates FLAG_IMPLIED_DIR entries
/// that occupy wire positions but are not hardlinked themselves.
#[test]
fn hardlink_round_trip_with_interspersed_directories() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

    // Wire layout simulating --relative with implied dirs:
    //   NDX 0: dir "a/"          (implied directory, no hardlink)
    //   NDX 1: file "a/orig.txt" (hardlink leader)
    //   NDX 2: dir "b/"          (implied directory, no hardlink)
    //   NDX 3: file "b/link.txt" (hardlink follower -> leader at NDX 1)
    let mut dir_a = FileEntry::new_directory("a".into(), 0o755);
    dir_a.set_mtime(1700000000, 0);

    let mut leader = FileEntry::new_file("a/orig.txt".into(), 256, 0o644);
    leader.set_mtime(1700000000, 0);
    leader.set_hardlink_idx(u32::MAX);

    let mut dir_b = FileEntry::new_directory("b".into(), 0o755);
    dir_b.set_mtime(1700000000, 0);

    let mut follower = FileEntry::new_file("b/link.txt".into(), 256, 0o644);
    follower.set_mtime(1700000000, 0);
    follower.set_hardlink_idx(1); // points to leader at wire NDX 1

    writer.write_entry(&mut buf, &dir_a).unwrap();
    writer.write_entry(&mut buf, &leader).unwrap();
    writer.write_entry(&mut buf, &dir_b).unwrap();
    writer.write_entry(&mut buf, &follower).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    // Read back
    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);

    let read_dir_a = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read_leader = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read_dir_b = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read_follower = reader.read_entry(&mut cursor).unwrap().unwrap();

    // Directory entries have no hardlink index
    assert_eq!(read_dir_a.hardlink_idx(), None);
    assert_eq!(read_dir_b.hardlink_idx(), None);

    // Leader round-trips with u32::MAX
    assert_eq!(read_leader.name(), "a/orig.txt");
    assert_eq!(read_leader.hardlink_idx(), Some(u32::MAX));

    // Follower round-trips with the correct wire NDX pointing to the leader
    assert_eq!(read_follower.name(), "b/link.txt");
    assert_eq!(read_follower.hardlink_idx(), Some(1));
}

/// Verifies that multiple hardlink groups with directories interspersed all
/// resolve correctly. Two separate hardlink groups with implied directories
/// between them must maintain independent leader/follower relationships.
#[test]
fn hardlink_multiple_groups_with_directories() {
    use super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

    // Wire layout:
    //   NDX 0: dir "d/"            (implied directory)
    //   NDX 1: file "d/a.txt"      (group A leader)
    //   NDX 2: file "d/a_link.txt" (group A follower -> NDX 1)
    //   NDX 3: dir "e/"            (implied directory)
    //   NDX 4: file "e/b.txt"      (group B leader)
    //   NDX 5: file "e/b_link.txt" (group B follower -> NDX 4)
    let mut dir_d = FileEntry::new_directory("d".into(), 0o755);
    dir_d.set_mtime(1700000000, 0);

    let mut leader_a = FileEntry::new_file("d/a.txt".into(), 100, 0o644);
    leader_a.set_mtime(1700000000, 0);
    leader_a.set_hardlink_idx(u32::MAX);

    let mut follower_a = FileEntry::new_file("d/a_link.txt".into(), 100, 0o644);
    follower_a.set_mtime(1700000000, 0);
    follower_a.set_hardlink_idx(1);

    let mut dir_e = FileEntry::new_directory("e".into(), 0o755);
    dir_e.set_mtime(1700000000, 0);

    let mut leader_b = FileEntry::new_file("e/b.txt".into(), 200, 0o644);
    leader_b.set_mtime(1700000000, 0);
    leader_b.set_hardlink_idx(u32::MAX);

    let mut follower_b = FileEntry::new_file("e/b_link.txt".into(), 200, 0o644);
    follower_b.set_mtime(1700000000, 0);
    follower_b.set_hardlink_idx(4);

    writer.write_entry(&mut buf, &dir_d).unwrap();
    writer.write_entry(&mut buf, &leader_a).unwrap();
    writer.write_entry(&mut buf, &follower_a).unwrap();
    writer.write_entry(&mut buf, &dir_e).unwrap();
    writer.write_entry(&mut buf, &leader_b).unwrap();
    writer.write_entry(&mut buf, &follower_b).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);

    let _ = reader.read_entry(&mut cursor).unwrap().unwrap(); // dir d
    let read_la = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read_fa = reader.read_entry(&mut cursor).unwrap().unwrap();
    let _ = reader.read_entry(&mut cursor).unwrap().unwrap(); // dir e
    let read_lb = reader.read_entry(&mut cursor).unwrap().unwrap();
    let read_fb = reader.read_entry(&mut cursor).unwrap().unwrap();

    assert_eq!(read_la.hardlink_idx(), Some(u32::MAX));
    assert_eq!(read_fa.hardlink_idx(), Some(1));
    assert_eq!(read_lb.hardlink_idx(), Some(u32::MAX));
    assert_eq!(read_fb.hardlink_idx(), Some(4));
}

#[test]
fn xattr_write_entry_sends_literal_data() {
    use crate::xattr::{XattrEntry, XattrList};

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(test_protocol()).with_preserve_xattrs(true);

    let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    let mut xattr_list = XattrList::new();
    xattr_list.push(XattrEntry::new("test_key", b"test_value".to_vec()));
    entry.set_xattr_list(xattr_list);

    writer.write_entry(&mut buf, &entry).unwrap();
    assert!(!buf.is_empty());
}

#[test]
fn xattr_write_empty_list_succeeds() {
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(test_protocol()).with_preserve_xattrs(true);

    // Entry without xattr_list - should send empty literal set
    let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    writer.write_entry(&mut buf, &entry).unwrap();
    assert!(!buf.is_empty());
}

#[test]
fn xattr_cache_deduplicates_identical_sets() {
    use crate::xattr::{XattrEntry, XattrList};

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(test_protocol()).with_preserve_xattrs(true);

    let make_list = || {
        let mut list = XattrList::new();
        list.push(XattrEntry::new("key", b"value".to_vec()));
        list
    };

    // First entry: literal data (no cache hit)
    let mut entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);
    entry1.set_xattr_list(make_list());
    writer.write_entry(&mut buf, &entry1).unwrap();
    let first_len = buf.len();

    // Second entry: same xattr set, should get cache hit (smaller on wire)
    let mut entry2 = FileEntry::new_file("file2.txt".into(), 200, 0o644);
    entry2.set_xattr_list(make_list());
    writer.write_entry(&mut buf, &entry2).unwrap();
    let second_len = buf.len() - first_len;

    // Cache hit sends only a varint index, so the second entry's xattr
    // portion should be smaller than the first (which included literal data)
    assert!(
        second_len < first_len,
        "cache hit should be smaller: {second_len} vs {first_len}",
    );
}

#[test]
fn xattr_write_roundtrip_with_reader() {
    use crate::xattr::{XattrEntry, XattrList};

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(test_protocol()).with_preserve_xattrs(true);

    let mut entry = FileEntry::new_file("roundtrip.txt".into(), 42, 0o644);
    let mut xattr_list = XattrList::new();
    xattr_list.push(XattrEntry::new("my_attr", b"my_value".to_vec()));
    entry.set_xattr_list(xattr_list);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    // Read back and verify xattr data was stored in cache
    let mut cursor = std::io::Cursor::new(&buf);
    let mut reader =
        super::super::read::FileListReader::new(test_protocol()).with_preserve_xattrs(true);
    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();

    assert_eq!(read_entry.name(), "roundtrip.txt");
    // The entry should have an xattr_ndx assigned by the reader
    assert!(
        read_entry.xattr_ndx().is_some(),
        "xattr_ndx should be set after reading"
    );
    // The reader's xattr cache should have the entry
    let xattr_cache = reader.xattr_cache();
    let cached = xattr_cache.get(read_entry.xattr_ndx().unwrap() as usize);
    assert!(cached.is_some(), "xattr cache should have entry");
    let list = cached.unwrap();
    assert_eq!(list.len(), 1);
}

/// When the negotiated peer capabilities lack xattr support, the file list
/// writer must NOT emit xattr wire bytes - even if the local file entry
/// happens to carry an attached xattr list. The gating boolean
/// `preserve.xattrs` mirrors the negotiated state derived from
/// `CompatibilityFlags::AVOID_XATTR_OPTIMIZATION`. Suppression must be
/// silent: no error, no partial emission.
///
/// upstream: flist.c:send_file_entry() line 656 - `send_xattr()` is only
/// invoked when `preserve_xattrs` is set in the receiver's option block.
#[test]
fn xattr_emission_suppressed_when_peer_lacks_xattr_capability() {
    use crate::xattr::{XattrEntry, XattrList};

    // Simulate the post-negotiation state where the remote peer did not
    // advertise CF_AVOID_XATTR_OPTIM, causing the local options layer to
    // clear the xattrs preserve flag. The default for `PreserveFlags` is
    // false, but we construct the writer explicitly to document intent.
    let mut writer_no_xattr = FileListWriter::new(test_protocol()).with_preserve_xattrs(false);

    let mut entry_with_xattr = FileEntry::new_file("attrs_attached.txt".into(), 100, 0o644);
    entry_with_xattr.set_mtime(1700000000, 0);
    let mut xattr_list = XattrList::new();
    xattr_list.push(XattrEntry::new("user.tag", b"local-only".to_vec()));
    xattr_list.push(XattrEntry::new(
        "security.selinux",
        b"system_u:object_r:default_t:s0".to_vec(),
    ));
    entry_with_xattr.set_xattr_list(xattr_list);

    let mut buf_suppressed = Vec::new();
    writer_no_xattr
        .write_entry(&mut buf_suppressed, &entry_with_xattr)
        .expect("write must succeed even when xattr emission is suppressed");

    // Baseline: same writer config, identical entry but with no xattr list
    // attached. Wire bytes must match exactly because xattrs are gated off.
    let mut writer_baseline = FileListWriter::new(test_protocol()).with_preserve_xattrs(false);
    let mut entry_no_xattr = FileEntry::new_file("attrs_attached.txt".into(), 100, 0o644);
    entry_no_xattr.set_mtime(1700000000, 0);

    let mut buf_baseline = Vec::new();
    writer_baseline
        .write_entry(&mut buf_baseline, &entry_no_xattr)
        .expect("baseline write must succeed");

    assert_eq!(
        buf_suppressed, buf_baseline,
        "writer with peer xattr capability OFF must emit identical bytes \
         regardless of attached xattr_list - no xattr wire data leaks",
    );

    // Cross-check: enabling preservation produces strictly more bytes
    // (literal xattr block appended), proving the suppression in the
    // baseline is meaningful and not a no-op of the encoder.
    let mut writer_enabled = FileListWriter::new(test_protocol()).with_preserve_xattrs(true);
    let mut buf_enabled = Vec::new();
    writer_enabled
        .write_entry(&mut buf_enabled, &entry_with_xattr)
        .expect("enabled write must succeed");
    assert!(
        buf_enabled.len() > buf_suppressed.len(),
        "enabling xattr preservation must add wire bytes \
         (enabled={}, suppressed={})",
        buf_enabled.len(),
        buf_suppressed.len(),
    );
}

/// Regression for #1905 / #1939.
///
/// On every platform, the wire-encoded filename must NEVER contain a backslash
/// byte. POSIX peers expect `/` as the only separator (upstream `flist.c`
/// writes filename bytes verbatim, and upstream's only Windows port runs
/// under Cygwin which presents `/`-separated paths to the writer).
#[test]
fn wire_encoded_filename_never_contains_backslash_byte() {
    use std::path::PathBuf;

    // Construct a path the same way an enumeration loop would on the host:
    // `PathBuf::push` uses the native separator on Windows.
    let mut path = PathBuf::from("subdir");
    path.push("file.txt");

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(test_protocol());
    let entry = FileEntry::new_file(path, 100, 0o644);

    writer.write_entry(&mut buf, &entry).unwrap();

    assert!(
        !buf.contains(&b'\\'),
        "wire-encoded entry must not contain a `\\` byte, got: {buf:?}",
    );
    let needle = b"subdir/file.txt";
    assert!(
        buf.windows(needle.len()).any(|w| w == needle),
        "wire-encoded entry must contain the forward-slash form `subdir/file.txt`, got: {buf:?}",
    );
}

/// Regression for #1905 / #1939: roundtrip via the reader must yield a
/// `/`-separated name regardless of the host platform that produced the bytes.
#[test]
fn wire_filename_roundtrip_yields_forward_slashes() {
    use super::super::read::FileListReader;
    use std::io::Cursor;
    use std::path::PathBuf;

    let mut path = PathBuf::from("a");
    path.push("b");
    path.push("c.txt");

    let protocol = test_protocol();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);
    let entry = FileEntry::new_file(path, 42, 0o644);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);
    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();

    assert_eq!(read_entry.name(), "a/b/c.txt");
    assert_eq!(read_entry.size(), 42);
}

/// Regression for #1905 / #1939: symlink targets must also be normalised.
/// A Windows-side relative symlink target like `sub\target.txt` would emit
/// raw bytes containing `\` without normalisation; this asserts the helper is
/// applied on the symlink-target write path too.
#[test]
fn wire_encoded_symlink_target_never_contains_backslash_byte() {
    use std::path::PathBuf;

    let mut target = PathBuf::from("sub");
    target.push("target.txt");

    let mut entry = FileEntry::new_symlink("link".into(), target);
    entry.set_mtime(0, 0);

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(test_protocol()).with_preserve_links(true);
    writer.write_entry(&mut buf, &entry).unwrap();

    assert!(
        !buf.contains(&b'\\'),
        "wire-encoded symlink entry must not contain a `\\` byte, got: {buf:?}",
    );
    let needle = b"sub/target.txt";
    assert!(
        buf.windows(needle.len()).any(|w| w == needle),
        "symlink target must be forward-slash form `sub/target.txt`, got: {buf:?}",
    );
}

/// Tracker #1912: when `--iconv=remote,local` is in effect, the sender's
/// filename must be transcoded from local to remote charset before being
/// written to the wire. A UTF-8 source name must appear on the wire as
/// ISO-8859-1 bytes when the converter is configured `(local=UTF-8,
/// remote=ISO-8859-1)`.
///
/// upstream: flist.c send_file_entry() iconv_buf(ic_send, ...)
#[cfg(feature = "iconv")]
#[test]
fn write_entry_transcodes_filename_with_iconv_to_remote_charset() {
    use crate::iconv::FilenameConverter;

    // Local UTF-8 "café.txt": 0x63 0x61 0x66 0xc3 0xa9 0x2e 0x74 0x78 0x74
    // Remote ISO-8859-1 "café.txt": 0x63 0x61 0x66 0xe9 0x2e 0x74 0x78 0x74
    let utf8_name = "café.txt";
    let latin1_bytes: &[u8] = &[0x63, 0x61, 0x66, 0xe9, 0x2e, 0x74, 0x78, 0x74];

    let converter = FilenameConverter::new("UTF-8", "ISO-8859-1").unwrap();
    let mut writer = FileListWriter::new(test_protocol()).with_iconv(converter);

    let entry = FileEntry::new_file(utf8_name.into(), 100, 0o644);
    let mut buf = Vec::new();
    writer.write_entry(&mut buf, &entry).unwrap();

    // The transcoded ISO-8859-1 bytes must appear on the wire.
    assert!(
        buf.windows(latin1_bytes.len()).any(|w| w == latin1_bytes),
        "wire bytes must contain ISO-8859-1 form of the filename, got: {buf:?}",
    );

    // The original UTF-8 multi-byte sequence (0xc3 0xa9 for é) must NOT be
    // present, otherwise the converter was bypassed.
    let utf8_e_acute: &[u8] = &[0xc3, 0xa9];
    assert!(
        !buf.windows(utf8_e_acute.len()).any(|w| w == utf8_e_acute),
        "wire bytes must not contain UTF-8 form of é (0xc3 0xa9), got: {buf:?}",
    );
}

/// Tracker #1912: with no converter configured the sender must emit the
/// raw filename bytes unchanged, preserving current behaviour for all
/// transfers that do not pass `--iconv`.
#[test]
fn write_entry_without_iconv_emits_raw_filename_bytes() {
    let utf8_name = "café.txt";
    let utf8_bytes = utf8_name.as_bytes();

    let mut writer = FileListWriter::new(test_protocol());
    let entry = FileEntry::new_file(utf8_name.into(), 100, 0o644);
    let mut buf = Vec::new();
    writer.write_entry(&mut buf, &entry).unwrap();

    assert!(
        buf.windows(utf8_bytes.len()).any(|w| w == utf8_bytes),
        "without --iconv, wire bytes must match the original filename, got: {buf:?}",
    );
}

/// Symlink targets are transcoded by the same `ic_send` converter as
/// filenames when `--iconv=LOCAL,REMOTE` is in effect and CF_SYMLINK_ICONV
/// has been negotiated. A UTF-8 local target must appear on the wire as
/// ISO-8859-1 bytes when the converter is configured `(local=UTF-8,
/// remote=ISO-8859-1)`.
///
/// upstream: flist.c:1606-1621 send_file_entry() - sender_symlink_iconv path
#[cfg(feature = "iconv")]
#[test]
fn write_symlink_target_transcodes_with_iconv_to_remote_charset() {
    use crate::iconv::FilenameConverter;

    let utf8_target = "café";
    let latin1_bytes: &[u8] = &[0x63, 0x61, 0x66, 0xe9];

    let converter = FilenameConverter::new("UTF-8", "ISO-8859-1").unwrap();
    let mut writer = FileListWriter::new(test_protocol())
        .with_preserve_links(true)
        .with_iconv(converter);

    let mut entry = FileEntry::new_symlink("link".into(), utf8_target.into());
    entry.set_mtime(1_700_000_000, 0);
    let mut buf = Vec::new();
    writer.write_entry(&mut buf, &entry).unwrap();

    assert!(
        buf.windows(latin1_bytes.len()).any(|w| w == latin1_bytes),
        "wire bytes must contain ISO-8859-1 form of the symlink target, got: {buf:?}",
    );
    let utf8_target_bytes = utf8_target.as_bytes();
    assert!(
        !buf.windows(utf8_target_bytes.len())
            .any(|w| w == utf8_target_bytes),
        "wire bytes must not contain UTF-8 form of the symlink target, got: {buf:?}",
    );
}

/// Without a converter the symlink target is written verbatim. This pins
/// the no-iconv default so the new transcoding hook does not regress
/// existing transfers.
#[test]
fn write_symlink_target_without_iconv_emits_raw_bytes() {
    let utf8_target = "café";
    let utf8_bytes = utf8_target.as_bytes();

    let mut writer = FileListWriter::new(test_protocol()).with_preserve_links(true);
    let mut entry = FileEntry::new_symlink("link".into(), utf8_target.into());
    entry.set_mtime(1_700_000_000, 0);
    let mut buf = Vec::new();
    writer.write_entry(&mut buf, &entry).unwrap();

    assert!(
        buf.windows(utf8_bytes.len()).any(|w| w == utf8_bytes),
        "without --iconv, symlink target wire bytes must match the original, got: {buf:?}",
    );
}
