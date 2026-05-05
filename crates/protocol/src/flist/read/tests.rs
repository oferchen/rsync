use super::*;
use std::io;
use std::io::Cursor;

use crate::CompatibilityFlags;
use crate::ProtocolVersion;
use crate::flist::entry::FileEntry;
use crate::flist::flags::{XMIT_EXTENDED_FLAGS, XMIT_IO_ERROR_ENDLIST};

fn test_protocol() -> ProtocolVersion {
    ProtocolVersion::try_from(32u8).unwrap()
}

#[test]
fn read_end_of_list_marker() {
    let data = [0u8];
    let mut cursor = Cursor::new(&data[..]);
    let mut reader = FileListReader::new(test_protocol());

    let result = reader.read_entry(&mut cursor).unwrap();
    assert!(result.is_none());
}

#[test]
fn read_simple_entry() {
    use crate::flist::write::FileListWriter;

    let protocol = test_protocol();
    let mut data = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let mut entry = FileEntry::new_file("test".into(), 100, 0o100644);
    entry.set_mtime(1700000000, 0);

    writer.write_entry(&mut data, &entry).unwrap();

    let mut cursor = Cursor::new(&data[..]);
    let mut reader = FileListReader::new(protocol);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "test");
    assert_eq!(read_entry.size(), 100);
    assert_eq!(read_entry.mode(), 0o100644);
    assert_eq!(read_entry.mtime(), 1700000000);
}

#[test]
fn read_entry_with_name_compression() {
    use crate::flist::write::FileListWriter;

    let protocol = test_protocol();
    let mut data = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let mut entry1 = FileEntry::new_file("dir/file".into(), 50, 0o100644);
    entry1.set_mtime(1700000000, 0);

    let mut entry2 = FileEntry::new_file("dir/other".into(), 75, 0o100644);
    entry2.set_mtime(1700000000, 0);

    writer.write_entry(&mut data, &entry1).unwrap();
    writer.write_entry(&mut data, &entry2).unwrap();

    let mut cursor = Cursor::new(&data[..]);
    let mut reader = FileListReader::new(protocol);

    let read_entry1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry1.name(), "dir/file");

    let read_entry2 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry2.name(), "dir/other");
}

#[test]
fn read_entry_detects_error_marker_with_safe_file_list() {
    use crate::varint::encode_varint_to_vec;

    let protocol = test_protocol();
    let flags = CompatibilityFlags::SAFE_FILE_LIST | CompatibilityFlags::VARINT_FLIST_FLAGS;
    let mut reader = FileListReader::with_compat_flags(protocol, flags);

    let error_marker = (XMIT_EXTENDED_FLAGS as i32) | ((XMIT_IO_ERROR_ENDLIST as i32) << 8);
    let error_code = 42;

    let mut data = Vec::new();
    encode_varint_to_vec(error_marker, &mut data);
    encode_varint_to_vec(error_code, &mut data);

    let mut cursor = Cursor::new(&data[..]);
    let result = reader.read_entry(&mut cursor);

    // io_error markers are now accumulated (upstream: flist.c io_error |= err)
    // rather than returned as hard errors.
    assert!(result.unwrap().is_none());
    assert_eq!(reader.io_error(), 42);
}

#[test]
fn read_entry_rejects_error_marker_without_safe_file_list() {
    use crate::varint::encode_varint_to_vec;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;
    let mut reader = FileListReader::with_compat_flags(protocol, flags);

    let error_marker = (XMIT_EXTENDED_FLAGS as i32) | ((XMIT_IO_ERROR_ENDLIST as i32) << 8);

    let mut data = Vec::new();
    encode_varint_to_vec(error_marker, &mut data);

    let mut cursor = Cursor::new(&data[..]);
    let result = reader.read_entry(&mut cursor);

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("Invalid flist flag"));
}

#[test]
fn read_entry_with_protocol_31_accepts_error_marker() {
    use crate::varint::encode_varint_to_vec;

    let protocol = ProtocolVersion::try_from(31u8).unwrap();
    let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;
    let mut reader = FileListReader::with_compat_flags(protocol, flags);

    let error_marker = (XMIT_EXTENDED_FLAGS as i32) | ((XMIT_IO_ERROR_ENDLIST as i32) << 8);
    let error_code = 99;

    let mut data = Vec::new();
    encode_varint_to_vec(error_marker, &mut data);
    encode_varint_to_vec(error_code, &mut data);

    let mut cursor = Cursor::new(&data[..]);
    let result = reader.read_entry(&mut cursor);

    assert!(result.unwrap().is_none());
    assert_eq!(reader.io_error(), 99);
}

#[test]
fn read_write_round_trip_with_safe_file_list_error_nonvarint() {
    use crate::flist::write::FileListWriter;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let flags = CompatibilityFlags::SAFE_FILE_LIST;

    let writer = FileListWriter::with_compat_flags(protocol, flags);
    let mut data = Vec::new();
    writer.write_end(&mut data, Some(123)).unwrap();

    let mut reader = FileListReader::with_compat_flags(protocol, flags);
    let mut cursor = Cursor::new(&data[..]);
    let result = reader.read_entry(&mut cursor);

    assert!(result.unwrap().is_none());
    assert_eq!(reader.io_error(), 123);
}

#[test]
fn read_write_round_trip_with_varint_end_marker() {
    use crate::flist::write::FileListWriter;

    let protocol = test_protocol();
    let flags = CompatibilityFlags::SAFE_FILE_LIST | CompatibilityFlags::VARINT_FLIST_FLAGS;

    // Test end marker with io_error=0 returns Ok(None)
    let writer = FileListWriter::with_compat_flags(protocol, flags);
    let mut data = Vec::new();
    writer.write_end(&mut data, Some(0)).unwrap();

    let mut reader = FileListReader::with_compat_flags(protocol, flags);
    let mut cursor = Cursor::new(&data[..]);
    let result = reader.read_entry(&mut cursor);
    assert!(result.is_ok());
    assert!(result.unwrap().is_none());
    assert_eq!(cursor.position() as usize, data.len());

    // Test end marker with non-zero error accumulates io_error
    let mut data2 = Vec::new();
    writer.write_end(&mut data2, Some(123)).unwrap();

    let mut reader2 = FileListReader::with_compat_flags(protocol, flags);
    let mut cursor2 = Cursor::new(&data2[..]);
    let result2 = reader2.read_entry(&mut cursor2);
    assert!(result2.unwrap().is_none());
    assert_eq!(reader2.io_error(), 123);
}

#[test]
fn use_varint_flags_checks_compat_flags() {
    let protocol = test_protocol();

    let reader_without = FileListReader::new(protocol);
    assert!(!reader_without.use_varint_flags());

    let reader_with =
        FileListReader::with_compat_flags(protocol, CompatibilityFlags::VARINT_FLIST_FLAGS);
    assert!(reader_with.use_varint_flags());
}

#[test]
fn use_safe_file_list_checks_protocol_and_flags() {
    // Protocol 30 without flag
    let reader30 = FileListReader::new(ProtocolVersion::try_from(30u8).unwrap());
    assert!(!reader30.use_safe_file_list());

    // Protocol 30 with flag
    let reader30_safe = FileListReader::with_compat_flags(
        ProtocolVersion::try_from(30u8).unwrap(),
        CompatibilityFlags::SAFE_FILE_LIST,
    );
    assert!(reader30_safe.use_safe_file_list());

    // Protocol 31+ automatically enables safe mode
    let reader31 = FileListReader::new(ProtocolVersion::try_from(31u8).unwrap());
    assert!(reader31.use_safe_file_list());
}

#[test]
fn read_flags_returns_end_of_list_for_zero() {
    let reader = FileListReader::new(test_protocol());
    let data = [0u8];
    let mut cursor = Cursor::new(&data[..]);

    match reader.read_flags(&mut cursor).unwrap() {
        FlagsResult::EndOfList => {}
        other => panic!("expected EndOfList, got {other:?}"),
    }
}

#[test]
fn read_flags_returns_io_error_in_varint_mode() {
    let reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS);

    use crate::varint::encode_varint_to_vec;
    let mut data = Vec::new();
    encode_varint_to_vec(0, &mut data); // flags = 0
    encode_varint_to_vec(42, &mut data); // error = 42

    let mut cursor = Cursor::new(&data[..]);

    match reader.read_flags(&mut cursor).unwrap() {
        FlagsResult::IoError(code) => assert_eq!(code, 42),
        other => panic!("expected IoError(42), got {other:?}"),
    }
}

#[test]
fn is_abbreviated_follower_helper() {
    use crate::flist::flags::{FileFlags, XMIT_HLINK_FIRST, XMIT_HLINKED};

    let mut reader = FileListReader::new(test_protocol()).with_preserve_hard_links(true);
    reader.set_ndx_start(100);

    let flags_none = FileFlags::new(0, 0);
    assert!(!reader.is_abbreviated_follower(flags_none, Some(150)));

    let flags_leader = FileFlags::new(0, XMIT_HLINKED | XMIT_HLINK_FIRST);
    assert!(!reader.is_abbreviated_follower(flags_leader, Some(150)));

    let flags_follower = FileFlags::new(0, XMIT_HLINKED);
    // Follower with idx >= ndx_start is abbreviated
    assert!(reader.is_abbreviated_follower(flags_follower, Some(150)));
    // Follower with idx < ndx_start is unabbreviated
    assert!(!reader.is_abbreviated_follower(flags_follower, Some(50)));
    // Follower with no idx is not abbreviated
    assert!(!reader.is_abbreviated_follower(flags_follower, None));
}

#[test]
fn read_write_round_trip_with_atime() {
    use crate::flist::write::FileListWriter;

    let protocol = test_protocol();
    let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

    let mut data = Vec::new();
    let mut writer = FileListWriter::with_compat_flags(protocol, flags).with_preserve_atimes(true);

    let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o100644);
    entry.set_mtime(1700000000, 0);
    entry.set_atime(1700001000);

    writer.write_entry(&mut data, &entry).unwrap();

    let mut cursor = Cursor::new(&data[..]);
    let mut reader = FileListReader::with_compat_flags(protocol, flags).with_preserve_atimes(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "test.txt");
    assert_eq!(read_entry.atime(), 1700001000);
}

#[test]
fn read_write_round_trip_with_same_atime() {
    use crate::flist::write::FileListWriter;

    let protocol = test_protocol();
    let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

    let mut data = Vec::new();
    let mut writer = FileListWriter::with_compat_flags(protocol, flags).with_preserve_atimes(true);

    let mut entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o100644);
    entry1.set_mtime(1700000000, 0);
    entry1.set_atime(1700001000);

    let mut entry2 = FileEntry::new_file("file2.txt".into(), 200, 0o100644);
    entry2.set_mtime(1700000000, 0);
    entry2.set_atime(1700001000);

    writer.write_entry(&mut data, &entry1).unwrap();
    writer.write_entry(&mut data, &entry2).unwrap();

    let mut cursor = Cursor::new(&data[..]);
    let mut reader = FileListReader::with_compat_flags(protocol, flags).with_preserve_atimes(true);

    let read_entry1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry1.atime(), 1700001000);

    let read_entry2 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry2.atime(), 1700001000);
}

#[test]
fn read_write_round_trip_with_crtime() {
    use crate::flist::write::FileListWriter;

    let protocol = test_protocol();
    let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

    let mut data = Vec::new();
    let mut writer = FileListWriter::with_compat_flags(protocol, flags).with_preserve_crtimes(true);

    let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o100644);
    entry.set_mtime(1700000000, 0);
    entry.set_crtime(1699999000);

    writer.write_entry(&mut data, &entry).unwrap();

    let mut cursor = Cursor::new(&data[..]);
    let mut reader = FileListReader::with_compat_flags(protocol, flags).with_preserve_crtimes(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "test.txt");
    assert_eq!(read_entry.crtime(), 1699999000);
}

#[test]
fn read_write_round_trip_with_crtime_eq_mtime() {
    use crate::flist::write::FileListWriter;

    let protocol = test_protocol();
    let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

    let mut data = Vec::new();
    let mut writer = FileListWriter::with_compat_flags(protocol, flags).with_preserve_crtimes(true);

    let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o100644);
    entry.set_mtime(1700000000, 0);
    entry.set_crtime(1700000000);

    writer.write_entry(&mut data, &entry).unwrap();

    let mut cursor = Cursor::new(&data[..]);
    let mut reader = FileListReader::with_compat_flags(protocol, flags).with_preserve_crtimes(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.crtime(), 1700000000);
    assert_eq!(read_entry.crtime(), read_entry.mtime());
}

#[test]
fn read_write_round_trip_directory_with_content() {
    use crate::flist::write::FileListWriter;

    let protocol = test_protocol();
    let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

    let mut data = Vec::new();
    let mut writer = FileListWriter::with_compat_flags(protocol, flags);

    let mut entry = FileEntry::new_directory("mydir".into(), 0o040755);
    entry.set_mtime(1700000000, 0);
    entry.set_content_dir(true);

    writer.write_entry(&mut data, &entry).unwrap();

    let mut cursor = Cursor::new(&data[..]);
    let mut reader = FileListReader::with_compat_flags(protocol, flags);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "mydir");
    assert!(read_entry.is_dir());
    assert!(read_entry.content_dir());
}

#[test]
fn read_write_round_trip_directory_without_content() {
    use crate::flist::write::FileListWriter;

    let protocol = test_protocol();
    let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

    let mut data = Vec::new();
    let mut writer = FileListWriter::with_compat_flags(protocol, flags);

    let mut entry = FileEntry::new_directory("implied_dir".into(), 0o040755);
    entry.set_mtime(1700000000, 0);
    entry.set_content_dir(false);

    writer.write_entry(&mut data, &entry).unwrap();

    let mut cursor = Cursor::new(&data[..]);
    let mut reader = FileListReader::with_compat_flags(protocol, flags);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "implied_dir");
    assert!(read_entry.is_dir());
    assert!(!read_entry.content_dir());
}

#[test]
fn read_write_round_trip_with_all_times() {
    use crate::flist::write::FileListWriter;

    let protocol = test_protocol();
    let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

    let mut data = Vec::new();
    let mut writer = FileListWriter::with_compat_flags(protocol, flags)
        .with_preserve_atimes(true)
        .with_preserve_crtimes(true);

    let mut entry = FileEntry::new_file("complete.txt".into(), 500, 0o100644);
    entry.set_mtime(1700000000, 0);
    entry.set_atime(1700001000);
    entry.set_crtime(1699990000);

    writer.write_entry(&mut data, &entry).unwrap();

    let mut cursor = Cursor::new(&data[..]);
    let mut reader = FileListReader::with_compat_flags(protocol, flags)
        .with_preserve_atimes(true)
        .with_preserve_crtimes(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "complete.txt");
    assert_eq!(read_entry.mtime(), 1700000000);
    assert_eq!(read_entry.atime(), 1700001000);
    assert_eq!(read_entry.crtime(), 1699990000);
}

#[test]
fn preserve_atimes_builder() {
    let reader = FileListReader::new(test_protocol()).with_preserve_atimes(true);
    assert!(reader.preserve_atimes);
}

#[test]
fn preserve_crtimes_builder() {
    let reader = FileListReader::new(test_protocol()).with_preserve_crtimes(true);
    assert!(reader.preserve_crtimes);
}

// Protocol 28/29 specific tests for rdev handling

#[test]
fn read_device_entry_protocol_29_byte_minor() {
    use crate::flist::write::FileListWriter;

    let protocol = ProtocolVersion::try_from(29u8).unwrap();
    let mut data = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    let mut entry = FileEntry::new_block_device("dev/sda".into(), 0o644, 8, 0);
    entry.set_mtime(1700000000, 0);

    writer.write_entry(&mut data, &entry).unwrap();

    let mut cursor = Cursor::new(&data[..]);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "dev/sda");
    assert_eq!(read_entry.rdev_major(), Some(8));
    assert_eq!(read_entry.rdev_minor(), Some(0));
}

#[test]
fn read_device_entry_protocol_29_int_minor() {
    use crate::flist::write::FileListWriter;

    let protocol = ProtocolVersion::try_from(29u8).unwrap();
    let mut data = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    let mut entry = FileEntry::new_block_device("dev/nvme0n1".into(), 0o644, 259, 65536);
    entry.set_mtime(1700000000, 0);

    writer.write_entry(&mut data, &entry).unwrap();

    let mut cursor = Cursor::new(&data[..]);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "dev/nvme0n1");
    assert_eq!(read_entry.rdev_major(), Some(259));
    assert_eq!(read_entry.rdev_minor(), Some(65536));
}

#[test]
fn read_device_entry_protocol_28_same_major_optimization() {
    use crate::flist::write::FileListWriter;

    let protocol = ProtocolVersion::try_from(28u8).unwrap();
    let mut data = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    let mut entry1 = FileEntry::new_block_device("dev/sda1".into(), 0o644, 8, 1);
    entry1.set_mtime(1700000000, 0);

    let mut entry2 = FileEntry::new_block_device("dev/sda2".into(), 0o644, 8, 2);
    entry2.set_mtime(1700000000, 0);

    writer.write_entry(&mut data, &entry1).unwrap();
    writer.write_entry(&mut data, &entry2).unwrap();

    let mut cursor = Cursor::new(&data[..]);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    let read1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read1.rdev_major(), Some(8));
    assert_eq!(read1.rdev_minor(), Some(1));

    let read2 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read2.rdev_major(), Some(8));
    assert_eq!(read2.rdev_minor(), Some(2));
}

#[test]
fn read_device_entry_protocol_30_uses_varint_minor() {
    use crate::flist::write::FileListWriter;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let mut data = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    let mut entry = FileEntry::new_block_device("dev/loop0".into(), 0o644, 7, 12345);
    entry.set_mtime(1700000000, 0);

    writer.write_entry(&mut data, &entry).unwrap();

    let mut cursor = Cursor::new(&data[..]);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.rdev_major(), Some(7));
    assert_eq!(read_entry.rdev_minor(), Some(12345));
}

#[test]
fn read_name_rejects_invalid_prefix_length() {
    use crate::flist::flags::XMIT_SAME_NAME;
    use crate::varint::encode_varint_to_vec;

    let protocol = test_protocol();
    let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

    let mut data = Vec::new();
    let xmit_flags = XMIT_SAME_NAME;
    encode_varint_to_vec(xmit_flags as i32, &mut data);
    data.push(5u8); // same_len = 5, but prev_name is empty
    data.push(4u8);
    data.extend_from_slice(b"test");

    let mut cursor = Cursor::new(&data[..]);
    let mut reader = FileListReader::with_compat_flags(protocol, flags);

    let result = reader.read_entry(&mut cursor);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("exceeds previous name length"));
}

#[test]
fn read_entry_truncated_name_fails() {
    use crate::varint::encode_varint_to_vec;

    let protocol = test_protocol();
    let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

    let mut data = Vec::new();
    encode_varint_to_vec(0x01, &mut data);
    data.push(100u8); // suffix_len: 100, but only 4 bytes follow
    data.extend_from_slice(b"test");

    let mut cursor = Cursor::new(&data[..]);
    let mut reader = FileListReader::with_compat_flags(protocol, flags);

    let result = reader.read_entry(&mut cursor);
    assert!(result.is_err(), "Expected error for truncated name data");
}

// Truncated wire format tests

/// Helper to assert UnexpectedEof error from truncated data.
fn assert_unexpected_eof(result: io::Result<Option<FileEntry>>, context: &str) {
    match result {
        Err(e) => {
            assert_eq!(
                e.kind(),
                io::ErrorKind::UnexpectedEof,
                "{}: expected UnexpectedEof, got {:?}",
                context,
                e.kind()
            );
        }
        Ok(entry) => {
            panic!(
                "{}: expected UnexpectedEof error, got Ok({:?})",
                context,
                entry.map(|e| e.name().to_string())
            );
        }
    }
}

#[test]
fn truncated_empty_input() {
    let data: &[u8] = &[];
    let mut cursor = Cursor::new(data);
    let mut reader = FileListReader::new(test_protocol());

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "empty input");
}

#[test]
fn truncated_flags_byte_nonvarint() {
    let data: &[u8] = &[];
    let mut cursor = Cursor::new(data);
    let mut reader = FileListReader::new(test_protocol());

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated flags byte (non-varint)");
}

#[test]
fn truncated_flags_varint_incomplete() {
    let data: &[u8] = &[0x80]; // Incomplete varint
    let mut cursor = Cursor::new(data);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated varint flags");
}

#[test]
fn truncated_extended_flags_byte() {
    use crate::flist::flags::XMIT_EXTENDED_FLAGS;

    let data: &[u8] = &[XMIT_EXTENDED_FLAGS];
    let mut cursor = Cursor::new(data);
    let mut reader = FileListReader::new(test_protocol());

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated extended flags byte");
}

#[test]
fn truncated_name_length_byte() {
    use crate::varint::encode_varint_to_vec;

    let mut data = Vec::new();
    encode_varint_to_vec(0x01, &mut data);

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated name length byte");
}

#[test]
fn truncated_name_data_partial() {
    use crate::varint::encode_varint_to_vec;

    let mut data = Vec::new();
    encode_varint_to_vec(0x01, &mut data);
    data.push(10u8);
    data.extend_from_slice(b"abc");

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated name data (partial)");
}

#[test]
fn truncated_same_name_prefix_byte() {
    use crate::flist::flags::XMIT_SAME_NAME;
    use crate::varint::encode_varint_to_vec;

    let mut data = Vec::new();
    encode_varint_to_vec(XMIT_SAME_NAME as i32, &mut data);

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated same_name prefix byte");
}

#[test]
fn truncated_size_field() {
    use crate::varint::encode_varint_to_vec;

    let mut data = Vec::new();
    encode_varint_to_vec(0x01, &mut data);
    data.push(4u8);
    data.extend_from_slice(b"test");

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated size field");
}

#[test]
fn truncated_size_field_partial_varlong() {
    use crate::varint::encode_varint_to_vec;

    let mut data = Vec::new();
    encode_varint_to_vec(0x01, &mut data);
    data.push(4u8);
    data.extend_from_slice(b"test");
    data.push(0xFF); // Incomplete varlong

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated size field (partial varlong)");
}

#[test]
fn truncated_mtime_field() {
    use crate::varint::encode_varint_to_vec;

    let mut data = Vec::new();
    encode_varint_to_vec(0x01, &mut data);
    data.push(4u8);
    data.extend_from_slice(b"test");
    data.push(100u8);

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated mtime field");
}

#[test]
fn truncated_mode_field() {
    use crate::varint::encode_varint_to_vec;

    let mut data = Vec::new();
    encode_varint_to_vec(0x01, &mut data);
    data.push(4u8);
    data.extend_from_slice(b"test");
    data.push(100u8);
    data.push(0u8);

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated mode field");
}

#[test]
fn truncated_mode_field_partial() {
    use crate::varint::encode_varint_to_vec;

    let mut data = Vec::new();
    encode_varint_to_vec(0x01, &mut data);
    data.push(4u8);
    data.extend_from_slice(b"test");
    data.push(100u8);
    data.push(0u8);
    data.extend_from_slice(&[0x44, 0x81]); // Partial mode (2 of 4 bytes)

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated mode field (partial)");
}

#[test]
fn truncated_uid_field_with_preserve_uid() {
    use crate::varint::encode_varint_to_vec;

    let mut data = Vec::new();
    encode_varint_to_vec(0x01, &mut data);
    data.push(4u8);
    data.extend_from_slice(b"test");
    data.push(100u8);
    data.push(0u8);
    data.extend_from_slice(&0o100644u32.to_le_bytes());

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS)
            .with_preserve_uid(true);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated uid field");
}

#[test]
fn truncated_gid_field_with_preserve_gid() {
    use crate::varint::encode_varint_to_vec;

    let mut data = Vec::new();
    encode_varint_to_vec(0x01, &mut data);
    data.push(4u8);
    data.extend_from_slice(b"test");
    data.push(100u8);
    data.push(0u8);
    data.extend_from_slice(&0o100644u32.to_le_bytes());

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS)
            .with_preserve_gid(true);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated gid field");
}

#[test]
fn truncated_symlink_target_length() {
    use crate::varint::encode_varint_to_vec;

    let mut data = Vec::new();
    encode_varint_to_vec(0x01, &mut data);
    data.push(4u8);
    data.extend_from_slice(b"link");
    data.push(0u8);
    data.push(0u8);
    data.extend_from_slice(&0o120777u32.to_le_bytes());

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS)
            .with_preserve_links(true);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated symlink target length");
}

#[test]
fn truncated_symlink_target_data() {
    use crate::varint::encode_varint_to_vec;

    let mut data = Vec::new();
    encode_varint_to_vec(0x01, &mut data);
    data.push(4u8);
    data.extend_from_slice(b"link");
    data.push(0u8);
    data.push(0u8);
    data.extend_from_slice(&0o120777u32.to_le_bytes());
    data.push(20u8);
    data.extend_from_slice(b"/etc"); // Only 4 of 20 bytes

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS)
            .with_preserve_links(true);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated symlink target data");
}

#[test]
fn truncated_device_major() {
    use crate::varint::encode_varint_to_vec;

    let mut data = Vec::new();
    encode_varint_to_vec(0x01, &mut data);
    data.push(7u8);
    data.extend_from_slice(b"dev/sda");
    data.push(0u8);
    data.push(0u8);
    data.extend_from_slice(&0o060644u32.to_le_bytes());

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS)
            .with_preserve_devices(true)
            .with_preserve_specials(true);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated device major");
}

#[test]
fn truncated_device_minor() {
    use crate::varint::encode_varint_to_vec;

    let mut data = Vec::new();
    encode_varint_to_vec(0x01, &mut data);
    data.push(7u8);
    data.extend_from_slice(b"dev/sda");
    data.push(0u8);
    data.push(0u8);
    data.extend_from_slice(&0o060644u32.to_le_bytes());
    data.push(8u8);

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS)
            .with_preserve_devices(true)
            .with_preserve_specials(true);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated device minor");
}

#[test]
fn truncated_atime_field() {
    use crate::varint::encode_varint_to_vec;

    let mut data = Vec::new();
    encode_varint_to_vec(0x01, &mut data);
    data.push(4u8);
    data.extend_from_slice(b"file");
    data.push(100u8);
    data.push(0u8);
    data.extend_from_slice(&0o100644u32.to_le_bytes());

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS)
            .with_preserve_atimes(true);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated atime field");
}

#[test]
fn truncated_checksum_field() {
    use crate::varint::encode_varint_to_vec;

    let mut data = Vec::new();
    encode_varint_to_vec(0x01, &mut data);
    data.push(4u8);
    data.extend_from_slice(b"file");
    data.push(100u8);
    data.push(0u8);
    data.extend_from_slice(&0o100644u32.to_le_bytes());

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS)
            .with_always_checksum(16);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated checksum field");
}

#[test]
fn truncated_checksum_field_partial() {
    use crate::varint::encode_varint_to_vec;

    let mut data = Vec::new();
    encode_varint_to_vec(0x01, &mut data);
    data.push(4u8);
    data.extend_from_slice(b"file");
    data.push(100u8);
    data.push(0u8);
    data.extend_from_slice(&0o100644u32.to_le_bytes());
    data.extend_from_slice(&[0xAB, 0xCD, 0xEF, 0x12]); // 4 of 16 bytes

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS)
            .with_always_checksum(16);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated checksum field (partial)");
}

#[test]
fn truncated_hardlink_index() {
    use crate::flist::flags::XMIT_HLINKED;
    use crate::varint::encode_varint_to_vec;

    let flags_value = (0x01) | ((XMIT_HLINKED as i32) << 8);
    let mut data = Vec::new();
    encode_varint_to_vec(flags_value, &mut data);
    data.push(4u8);
    data.extend_from_slice(b"link");

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS)
            .with_preserve_hard_links(true);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated hardlink index");
}

#[test]
fn truncated_user_name_length() {
    use crate::flist::flags::XMIT_USER_NAME_FOLLOWS;
    use crate::varint::encode_varint_to_vec;

    let flags_value = (0x01) | ((XMIT_USER_NAME_FOLLOWS as i32) << 8);
    let mut data = Vec::new();
    encode_varint_to_vec(flags_value, &mut data);
    data.push(4u8);
    data.extend_from_slice(b"file");
    data.push(100u8);
    data.push(0u8);
    data.extend_from_slice(&0o100644u32.to_le_bytes());
    data.push(100u8); // UID

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS)
            .with_preserve_uid(true);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated user name length");
}

#[test]
fn truncated_user_name_data() {
    use crate::flist::flags::XMIT_USER_NAME_FOLLOWS;
    use crate::varint::encode_varint_to_vec;

    let flags_value = (0x01) | ((XMIT_USER_NAME_FOLLOWS as i32) << 8);
    let mut data = Vec::new();
    encode_varint_to_vec(flags_value, &mut data);
    data.push(4u8);
    data.extend_from_slice(b"file");
    data.push(100u8);
    data.push(0u8);
    data.extend_from_slice(&0o100644u32.to_le_bytes());
    data.push(100u8); // UID
    data.push(10u8); // User name length: 10
    data.extend_from_slice(b"user"); // Only 4 of 10 bytes

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS)
            .with_preserve_uid(true);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated user name data");
}

#[test]
fn truncated_crtime_field() {
    use crate::varint::encode_varint_to_vec;

    let mut data = Vec::new();
    encode_varint_to_vec(0x01, &mut data);
    data.push(4u8);
    data.extend_from_slice(b"file");
    data.push(100u8);
    data.push(0u8);

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS)
            .with_preserve_crtimes(true);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated crtime field");
}

#[test]
fn truncated_nsec_field() {
    use crate::flist::flags::XMIT_MOD_NSEC;
    use crate::varint::encode_varint_to_vec;

    let protocol = ProtocolVersion::try_from(31u8).unwrap();
    let flags_value = (0x01) | ((XMIT_MOD_NSEC as i32) << 8);
    let mut data = Vec::new();
    encode_varint_to_vec(flags_value, &mut data);
    data.push(4u8);
    data.extend_from_slice(b"file");
    data.push(100u8);
    data.push(0u8);

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(protocol, CompatibilityFlags::VARINT_FLIST_FLAGS);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated nsec field");
}

#[test]
fn truncated_long_name_varint() {
    use crate::flist::flags::XMIT_LONG_NAME;
    use crate::varint::encode_varint_to_vec;

    let flags_value = XMIT_LONG_NAME as i32 | 0x01;
    let mut data = Vec::new();
    encode_varint_to_vec(flags_value, &mut data);
    data.push(0x80); // Incomplete varint

    let mut cursor = Cursor::new(&data[..]);
    let mut reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated long name varint");
}

#[test]
fn truncated_protocol_29_device_minor_int() {
    use crate::flist::write::FileListWriter;

    let protocol = ProtocolVersion::try_from(29u8).unwrap();
    let mut data = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    let mut entry = FileEntry::new_block_device("dev/nvme0n1".into(), 0o644, 259, 65536);
    entry.set_mtime(1700000000, 0);

    writer.write_entry(&mut data, &entry).unwrap();

    let truncated_data = &data[..data.len() - 2];

    let mut cursor = Cursor::new(truncated_data);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    let result = reader.read_entry(&mut cursor);
    assert_unexpected_eof(result, "truncated protocol 29 device minor (int)");
}

/// Verifies the reader correctly decodes varlong-encoded large file sizes
/// above the 2^31 (2 GB) boundary.
#[test]
fn read_large_file_size_3gb() {
    use crate::flist::write::FileListWriter;

    const SIZE_3GB: u64 = 3 * 1024 * 1024 * 1024;

    let protocol = test_protocol();
    let mut data = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let mut entry = FileEntry::new_file("huge_3gb.dat".into(), SIZE_3GB, 0o100644);
    entry.set_mtime(1700000000, 0);

    writer.write_entry(&mut data, &entry).unwrap();

    let mut cursor = Cursor::new(&data[..]);
    let mut reader = FileListReader::new(protocol);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "huge_3gb.dat");
    assert_eq!(
        read_entry.size(),
        SIZE_3GB,
        "Reader should correctly decode 3GB file size (above 2^31 boundary)"
    );
}

/// Verifies the reader correctly decodes varlong-encoded very large file sizes
/// above the 2^32 (4 GB) boundary.
#[test]
fn read_large_file_size_5gb() {
    use crate::flist::write::FileListWriter;

    const SIZE_5GB: u64 = 5 * 1024 * 1024 * 1024;

    let protocol = test_protocol();
    let mut data = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let mut entry = FileEntry::new_file("huge_5gb.dat".into(), SIZE_5GB, 0o100644);
    entry.set_mtime(1700000000, 0);

    writer.write_entry(&mut data, &entry).unwrap();

    let mut cursor = Cursor::new(&data[..]);
    let mut reader = FileListReader::new(protocol);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "huge_5gb.dat");
    assert_eq!(
        read_entry.size(),
        SIZE_5GB,
        "Reader should correctly decode 5GB file size (above 2^32 boundary)"
    );
}

/// Tests critical size boundaries (2^31, 2^32) that represent the limits
/// of 32-bit signed and unsigned integer ranges.
#[test]
fn read_large_file_sizes_at_boundaries() {
    use crate::flist::write::FileListWriter;

    let boundary_sizes: &[(u64, &str)] = &[
        ((1u64 << 31) - 1, "max_i32"),
        (1u64 << 31, "2gb"),
        ((1u64 << 31) + 1, "2gb_plus_1"),
        ((1u64 << 32) - 1, "max_u32"),
        (1u64 << 32, "4gb"),
        ((1u64 << 32) + 1, "4gb_plus_1"),
    ];

    let protocol = test_protocol();

    for (size, label) in boundary_sizes {
        let mut data = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        let filename = format!("boundary_{label}.bin");
        let mut entry = FileEntry::new_file(filename.clone().into(), *size, 0o100644);
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut data, &entry).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), &filename);
        assert_eq!(
            read_entry.size(),
            *size,
            "Reader should correctly decode size {size} at {label} boundary"
        );
    }
}

// Zero-length filename validation tests
// upstream: flist.c:1873 - sender rejects empty names. These tests verify
// that the receiver also rejects zero-length filenames as defense-in-depth.

#[test]
fn read_entry_rejects_zero_length_filename() {
    use crate::varint::encode_varint_to_vec;

    let protocol = test_protocol();
    let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

    let mut data = Vec::new();
    encode_varint_to_vec(0x01, &mut data);
    data.push(0u8); // suffix_len = 0

    let mut cursor = Cursor::new(&data[..]);
    let mut reader = FileListReader::with_compat_flags(protocol, flags);

    let result = reader.read_entry(&mut cursor);
    assert!(result.is_err(), "zero-length filename should be rejected");
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(
        err.to_string().contains("zero-length filename"),
        "error message should mention zero-length filename, got: {err}"
    );
}

#[test]
fn read_entry_accepts_non_empty_filename() {
    use crate::flist::write::FileListWriter;

    let protocol = test_protocol();
    let mut data = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let mut entry = FileEntry::new_file("a".into(), 1, 0o100644);
    entry.set_mtime(1700000000, 0);

    writer.write_entry(&mut data, &entry).unwrap();

    let mut cursor = Cursor::new(&data[..]);
    let mut reader = FileListReader::new(protocol);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "a");
}

#[test]
fn read_entry_rejects_zero_length_filename_nonvarint() {
    let data: &[u8] = &[
        0x01, // flags byte
        0x00, // suffix_len = 0
    ];

    let mut cursor = Cursor::new(data);
    let mut reader = FileListReader::new(test_protocol());

    let result = reader.read_entry(&mut cursor);
    assert!(result.is_err(), "zero-length filename should be rejected");
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("zero-length filename"));
}

/// Tests large file sizes with legacy protocol 29 (longint encoding).
#[test]
fn read_large_file_size_legacy_protocol() {
    use crate::flist::write::FileListWriter;

    const SIZE_3GB: u64 = 3 * 1024 * 1024 * 1024;
    const SIZE_5GB: u64 = 5 * 1024 * 1024 * 1024;

    let protocol = ProtocolVersion::try_from(29u8).unwrap();

    for (size, label) in [(SIZE_3GB, "3GB"), (SIZE_5GB, "5GB")] {
        let mut data = Vec::new();
        let mut writer = FileListWriter::new(protocol);

        let mut entry = FileEntry::new_file("legacy_large.bin".into(), size, 0o100644);
        entry.set_mtime(1700000000, 0);

        writer.write_entry(&mut data, &entry).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(
            read_entry.size(),
            size,
            "Legacy protocol reader should correctly decode {label} file size"
        );
    }
}

/// Verifies that non-varint mode consumes exactly one zero byte for end-of-list.
///
/// Upstream: `flist.c:recv_file_list()` - without `xfer_flags_as_varint`,
/// `read_byte(f)` returns 0 and the loop breaks immediately.
#[test]
fn read_end_of_list_nonvarint_consumes_single_byte() {
    let reader = FileListReader::new(test_protocol());
    let data = [0x00, 0xFF];
    let mut cursor = Cursor::new(&data[..]);

    let result = reader.read_flags(&mut cursor).unwrap();
    assert!(matches!(result, FlagsResult::EndOfList));
    assert_eq!(
        cursor.position(),
        1,
        "non-varint end-of-list must consume exactly 1 byte"
    );
}

/// Verifies that varint mode consumes exactly two zero bytes for end-of-list
/// (varint(0) for flags + varint(0) for error code).
///
/// Upstream: `flist.c:recv_file_list()` - with `xfer_flags_as_varint`,
/// `read_varint(f)` returns 0 for flags, then `read_varint(f)` returns 0 for error.
#[test]
fn read_end_of_list_varint_consumes_two_bytes() {
    let reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS);
    let data = [0x00, 0x00, 0xFF];
    let mut cursor = Cursor::new(&data[..]);

    let result = reader.read_flags(&mut cursor).unwrap();
    assert!(matches!(result, FlagsResult::EndOfList));
    assert_eq!(
        cursor.position(),
        2,
        "varint end-of-list must consume exactly 2 bytes (flags=0 + error=0)"
    );
}

/// Verifies that varint mode with non-zero error code consumes the second
/// varint and returns IoError.
///
/// Upstream: `flist.c:recv_file_list()` - flags varint = 0, error varint != 0
/// causes `io_error |= err`.
#[test]
fn read_end_of_list_varint_with_error_returns_io_error() {
    use crate::varint::encode_varint_to_vec;

    let reader =
        FileListReader::with_compat_flags(test_protocol(), CompatibilityFlags::VARINT_FLIST_FLAGS);

    let mut data = Vec::new();
    encode_varint_to_vec(0, &mut data);
    encode_varint_to_vec(7, &mut data);
    data.push(0xFF); // sentinel

    let mut cursor = Cursor::new(&data[..]);
    let result = reader.read_flags(&mut cursor).unwrap();

    match result {
        FlagsResult::IoError(code) => assert_eq!(code, 7),
        other => panic!("expected IoError(7), got {other:?}"),
    }
    assert_eq!(
        cursor.position() as usize,
        data.len() - 1,
        "must consume flags varint + error varint but not the sentinel"
    );
}

/// Verifies round-trip: varint write_end -> read_flags produces EndOfList
/// and consumes exactly the written bytes.
#[test]
fn read_write_end_of_list_varint_round_trip_exact_bytes() {
    use crate::flist::write::FileListWriter;

    let protocol = test_protocol();
    let flags = CompatibilityFlags::VARINT_FLIST_FLAGS;

    let writer = FileListWriter::with_compat_flags(protocol, flags);
    let mut buf = Vec::new();
    writer.write_end(&mut buf, None).unwrap();

    assert_eq!(buf, [0x00, 0x00], "varint end marker without error");

    let reader = FileListReader::with_compat_flags(protocol, flags);
    let mut cursor = Cursor::new(&buf[..]);
    let result = reader.read_flags(&mut cursor).unwrap();

    assert!(matches!(result, FlagsResult::EndOfList));
    assert_eq!(
        cursor.position() as usize,
        buf.len(),
        "must consume all written bytes"
    );
}

/// Verifies round-trip: non-varint write_end -> read_flags produces EndOfList
/// and consumes exactly one byte.
#[test]
fn read_write_end_of_list_nonvarint_round_trip_exact_bytes() {
    use crate::flist::write::FileListWriter;

    let protocol = test_protocol();

    let writer = FileListWriter::new(protocol);
    let mut buf = Vec::new();
    writer.write_end(&mut buf, None).unwrap();

    assert_eq!(buf, [0x00], "non-varint end marker");

    let reader = FileListReader::new(protocol);
    let mut cursor = Cursor::new(&buf[..]);
    let result = reader.read_flags(&mut cursor).unwrap();

    assert!(matches!(result, FlagsResult::EndOfList));
    assert_eq!(
        cursor.position() as usize,
        buf.len(),
        "must consume all written bytes"
    );
}

/// Tests for ACL integration in the flist read path.
mod acl_integration {
    use super::*;
    use crate::acl::{AclCache, AclType, RsyncAcl, send_acl, send_rsync_acl};

    #[test]
    fn read_entry_with_access_acl() {
        use crate::flist::write::FileListWriter;

        let protocol = test_protocol();
        let mut data = Vec::new();

        let mut writer = FileListWriter::new(protocol);
        let mut entry = FileEntry::new_file("test_acl.txt".into(), 200, 0o100644);
        entry.set_mtime(1700000000, 0);
        writer.write_entry(&mut data, &entry).unwrap();

        // upstream: flist.c send_acl() is called after send_file_entry()
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x06;
        acl.group_obj = 0x04;
        acl.other_obj = 0x04;
        let mut acl_cache = AclCache::new();
        send_rsync_acl(&mut data, &acl, AclType::Access, &mut acl_cache, false).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_acls(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "test_acl.txt");
        assert_eq!(read_entry.size(), 200);
        assert_eq!(read_entry.acl_ndx(), Some(0));
        assert!(read_entry.def_acl_ndx().is_none());

        let cached = reader.acl_cache().get_access(0).unwrap();
        assert_eq!(cached.user_obj, 0x06);
        assert_eq!(cached.group_obj, 0x04);
        assert_eq!(cached.other_obj, 0x04);

        assert_eq!(cursor.position() as usize, data.len());
    }

    #[test]
    fn read_directory_entry_with_access_and_default_acl() {
        use crate::flist::write::FileListWriter;

        let protocol = test_protocol();
        let mut data = Vec::new();

        let mut writer = FileListWriter::new(protocol);
        let mut entry = FileEntry::new_directory("mydir".into(), 0o755);
        entry.set_mtime(1700000000, 0);
        writer.write_entry(&mut data, &entry).unwrap();

        let access_acl = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x07;
            a.group_obj = 0x05;
            a.other_obj = 0x05;
            a
        };
        let default_acl = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x07;
            a.group_obj = 0x05;
            a.other_obj = 0x00;
            a
        };
        let mut acl_cache = AclCache::new();
        send_acl(
            &mut data,
            &access_acl,
            Some(&default_acl),
            true,
            &mut acl_cache,
        )
        .unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_acls(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "mydir");
        assert!(read_entry.is_dir());
        assert_eq!(read_entry.acl_ndx(), Some(0));
        assert_eq!(read_entry.def_acl_ndx(), Some(0));

        let cached_access = reader.acl_cache().get_access(0).unwrap();
        assert_eq!(cached_access.user_obj, 0x07);
        assert_eq!(cached_access.other_obj, 0x05);

        let cached_default = reader.acl_cache().get_default(0).unwrap();
        assert_eq!(cached_default.user_obj, 0x07);
        assert_eq!(cached_default.other_obj, 0x00);

        assert_eq!(cursor.position() as usize, data.len());
    }

    /// ACLs are NOT read for symlink entries (matching upstream behavior).
    #[test]
    fn read_symlink_entry_skips_acl() {
        use crate::flist::write::FileListWriter;

        let protocol = test_protocol();
        let mut data = Vec::new();

        let mut writer = FileListWriter::new(protocol).with_preserve_links(true);
        let mut entry = FileEntry::new_symlink("link".into(), "target".into());
        entry.set_mtime(1700000000, 0);
        writer.write_entry(&mut data, &entry).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_acls(true)
            .with_preserve_links(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert!(read_entry.is_symlink());
        assert!(read_entry.acl_ndx().is_none());

        assert_eq!(cursor.position() as usize, data.len());
    }

    #[test]
    fn read_entry_without_preserve_acls_skips_acl() {
        use crate::flist::write::FileListWriter;

        let protocol = test_protocol();
        let mut data = Vec::new();

        let mut writer = FileListWriter::new(protocol);
        let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o100644);
        entry.set_mtime(1700000000, 0);
        writer.write_entry(&mut data, &entry).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert!(read_entry.acl_ndx().is_none());
        assert_eq!(cursor.position() as usize, data.len());
    }

    #[test]
    fn multiple_entries_share_cached_acls() {
        use crate::flist::write::FileListWriter;

        let protocol = test_protocol();
        let mut data = Vec::new();
        let mut acl_cache = AclCache::new();

        let acl = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x07;
            a.group_obj = 0x05;
            a.other_obj = 0x04;
            a
        };

        let mut writer = FileListWriter::new(protocol);
        for name in &["file1.txt", "file2.txt"] {
            let mut entry = FileEntry::new_file((*name).into(), 100, 0o100644);
            entry.set_mtime(1700000000, 0);
            writer.write_entry(&mut data, &entry).unwrap();
            send_rsync_acl(&mut data, &acl, AclType::Access, &mut acl_cache, false).unwrap();
        }

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_acls(true);

        let entry1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let entry2 = reader.read_entry(&mut cursor).unwrap().unwrap();

        assert_eq!(entry1.name(), "file1.txt");
        assert_eq!(entry1.acl_ndx(), Some(0));
        assert_eq!(entry2.name(), "file2.txt");
        assert_eq!(entry2.acl_ndx(), Some(0));

        assert_eq!(reader.acl_cache().access_count(), 1);
    }

    #[test]
    fn different_acls_get_distinct_indices() {
        use crate::flist::write::FileListWriter;

        let protocol = test_protocol();
        let mut data = Vec::new();
        let mut acl_cache = AclCache::new();

        let acl1 = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x07;
            a.group_obj = 0x05;
            a.other_obj = 0x04;
            a
        };
        let acl2 = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x06;
            a.group_obj = 0x04;
            a.other_obj = 0x00;
            a
        };

        let mut writer = FileListWriter::new(protocol);

        let mut entry = FileEntry::new_file("a.txt".into(), 100, 0o100644);
        entry.set_mtime(1700000000, 0);
        writer.write_entry(&mut data, &entry).unwrap();
        send_rsync_acl(&mut data, &acl1, AclType::Access, &mut acl_cache, false).unwrap();

        let mut entry = FileEntry::new_file("b.txt".into(), 200, 0o100600);
        entry.set_mtime(1700000000, 0);
        writer.write_entry(&mut data, &entry).unwrap();
        send_rsync_acl(&mut data, &acl2, AclType::Access, &mut acl_cache, false).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_acls(true);

        let e1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let e2 = reader.read_entry(&mut cursor).unwrap().unwrap();

        assert_eq!(e1.acl_ndx(), Some(0));
        assert_eq!(e2.acl_ndx(), Some(1));
        assert_eq!(reader.acl_cache().access_count(), 2);

        let c1 = reader.acl_cache().get_access(0).unwrap();
        assert_eq!(c1.user_obj, 0x07);
        let c2 = reader.acl_cache().get_access(1).unwrap();
        assert_eq!(c2.user_obj, 0x06);

        assert_eq!(cursor.position() as usize, data.len());
    }

    #[test]
    fn acl_with_named_entries() {
        use crate::acl::IdAccess;
        use crate::flist::write::FileListWriter;

        let protocol = test_protocol();
        let mut data = Vec::new();

        let mut writer = FileListWriter::new(protocol);
        let mut entry = FileEntry::new_file("named_acl.txt".into(), 300, 0o100664);
        entry.set_mtime(1700000000, 0);
        writer.write_entry(&mut data, &entry).unwrap();

        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.group_obj = 0x05;
        acl.mask_obj = 0x07;
        acl.other_obj = 0x04;
        acl.names.push(IdAccess::user(1000, 0x07));
        acl.names.push(IdAccess::group(100, 0x05));

        let mut acl_cache = AclCache::new();
        send_rsync_acl(&mut data, &acl, AclType::Access, &mut acl_cache, false).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_acls(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.acl_ndx(), Some(0));

        let cached = reader.acl_cache().get_access(0).unwrap();
        assert_eq!(cached.user_obj, 0x07);
        assert_eq!(cached.mask_obj, 0x07);
        assert_eq!(cached.names.len(), 2);

        assert_eq!(cursor.position() as usize, data.len());
    }

    #[test]
    fn directory_default_acl_cache_hit() {
        use crate::flist::write::FileListWriter;

        let protocol = test_protocol();
        let mut data = Vec::new();
        let mut acl_cache = AclCache::new();

        let access_acl = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x07;
            a.group_obj = 0x05;
            a.other_obj = 0x05;
            a
        };
        let default_acl = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x07;
            a.group_obj = 0x05;
            a.other_obj = 0x00;
            a
        };

        let mut writer = FileListWriter::new(protocol);
        for name in &["dir1", "dir2"] {
            let mut entry = FileEntry::new_directory((*name).into(), 0o755);
            entry.set_mtime(1700000000, 0);
            writer.write_entry(&mut data, &entry).unwrap();
            send_acl(
                &mut data,
                &access_acl,
                Some(&default_acl),
                true,
                &mut acl_cache,
            )
            .unwrap();
        }

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_acls(true);

        let d1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let d2 = reader.read_entry(&mut cursor).unwrap().unwrap();

        assert_eq!(d1.acl_ndx(), Some(0));
        assert_eq!(d1.def_acl_ndx(), Some(0));
        assert_eq!(d2.acl_ndx(), Some(0));
        assert_eq!(d2.def_acl_ndx(), Some(0));

        assert_eq!(reader.acl_cache().access_count(), 1);
        assert_eq!(reader.acl_cache().default_count(), 1);

        assert_eq!(cursor.position() as usize, data.len());
    }

    /// Combined ACL + xattr reading respects upstream wire order.
    /// upstream: flist.c:1205-1212 - ACLs before xattrs on wire.
    #[test]
    fn combined_acl_and_xattr_reading() {
        use crate::flist::write::FileListWriter;
        use crate::varint::write_varint;

        let protocol = test_protocol();
        let mut data = Vec::new();

        let mut writer = FileListWriter::new(protocol);
        let mut entry = FileEntry::new_file("both.txt".into(), 500, 0o100644);
        entry.set_mtime(1700000000, 0);
        writer.write_entry(&mut data, &entry).unwrap();

        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x06;
        acl.group_obj = 0x04;
        acl.other_obj = 0x04;
        let mut acl_cache = AclCache::new();
        send_rsync_acl(&mut data, &acl, AclType::Access, &mut acl_cache, false).unwrap();

        // Wire format uses names without the "user." namespace prefix
        write_varint(&mut data, 0).unwrap();
        write_varint(&mut data, 1).unwrap();
        write_varint(&mut data, 5).unwrap();
        write_varint(&mut data, 5).unwrap();
        data.extend_from_slice(b"test\0");
        data.extend_from_slice(b"hello");

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_acls(true)
            .with_preserve_xattrs(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "both.txt");
        assert_eq!(read_entry.acl_ndx(), Some(0));
        assert_eq!(read_entry.xattr_ndx(), Some(0));

        let cached_acl = reader.acl_cache().get_access(0).unwrap();
        assert_eq!(cached_acl.user_obj, 0x06);

        let cached_xattr = reader.xattr_cache().get(0).unwrap();
        assert_eq!(cached_xattr.len(), 1);
        #[cfg(target_os = "linux")]
        assert_eq!(cached_xattr.entries()[0].name(), b"user.test");
        #[cfg(not(target_os = "linux"))]
        assert_eq!(cached_xattr.entries()[0].name(), b"test");
        assert_eq!(cached_xattr.entries()[0].datum(), b"hello");

        assert_eq!(cursor.position() as usize, data.len());
    }

    /// Directory with ACL + xattr: access ACL, default ACL, then xattr.
    #[test]
    fn directory_acl_and_xattr_combined() {
        use crate::flist::write::FileListWriter;
        use crate::varint::write_varint;

        let protocol = test_protocol();
        let mut data = Vec::new();

        let mut writer = FileListWriter::new(protocol);
        let mut entry = FileEntry::new_directory("mydir".into(), 0o755);
        entry.set_mtime(1700000000, 0);
        writer.write_entry(&mut data, &entry).unwrap();

        let access_acl = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x07;
            a.group_obj = 0x05;
            a.other_obj = 0x05;
            a
        };
        let default_acl = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x07;
            a.group_obj = 0x07;
            a.other_obj = 0x00;
            a
        };
        let mut acl_cache = AclCache::new();
        send_acl(
            &mut data,
            &access_acl,
            Some(&default_acl),
            true,
            &mut acl_cache,
        )
        .unwrap();

        write_varint(&mut data, 0).unwrap();
        write_varint(&mut data, 1).unwrap();
        write_varint(&mut data, 6).unwrap();
        write_varint(&mut data, 3).unwrap();
        data.extend_from_slice(b"label\0");
        data.extend_from_slice(b"foo");

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_acls(true)
            .with_preserve_xattrs(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert!(read_entry.is_dir());
        assert_eq!(read_entry.acl_ndx(), Some(0));
        assert_eq!(read_entry.def_acl_ndx(), Some(0));
        assert_eq!(read_entry.xattr_ndx(), Some(0));

        let cached_def = reader.acl_cache().get_default(0).unwrap();
        assert_eq!(cached_def.other_obj, 0x00);

        let cached_xattr = reader.xattr_cache().get(0).unwrap();
        assert_eq!(cached_xattr.entries()[0].datum(), b"foo");

        assert_eq!(cursor.position() as usize, data.len());
    }

    /// Symlink with xattrs but no ACLs - verifies symlinks skip ACL
    /// reading but still receive xattr data.
    #[test]
    fn symlink_skips_acl_but_reads_xattr() {
        use crate::flist::write::FileListWriter;
        use crate::varint::write_varint;

        let protocol = test_protocol();
        let mut data = Vec::new();

        let mut writer = FileListWriter::new(protocol).with_preserve_links(true);
        let mut entry = FileEntry::new_symlink("link".into(), "target".into());
        entry.set_mtime(1700000000, 0);
        writer.write_entry(&mut data, &entry).unwrap();

        write_varint(&mut data, 0).unwrap();
        write_varint(&mut data, 1).unwrap();
        write_varint(&mut data, 6).unwrap();
        write_varint(&mut data, 4).unwrap();
        data.extend_from_slice(b"label\0");
        data.extend_from_slice(b"test");

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_acls(true)
            .with_preserve_xattrs(true)
            .with_preserve_links(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert!(read_entry.is_symlink());
        assert!(read_entry.acl_ndx().is_none());
        assert_eq!(read_entry.xattr_ndx(), Some(0));

        assert_eq!(cursor.position() as usize, data.len());
    }
}

/// Tests for xattr integration in the flist read path.
mod xattr_integration {
    use super::*;
    use crate::varint::write_varint;

    /// Returns the expected local name after wire-to-local translation.
    fn expected_local_name(wire_name: &[u8]) -> Vec<u8> {
        #[cfg(target_os = "linux")]
        {
            let mut local = b"user.".to_vec();
            local.extend_from_slice(wire_name);
            local
        }
        #[cfg(not(target_os = "linux"))]
        {
            wire_name.to_vec()
        }
    }

    /// Helper to append literal xattr data to a buffer in wire format.
    fn write_literal_xattr(buf: &mut Vec<u8>, entries: &[(&[u8], &[u8])]) {
        write_varint(buf, 0).unwrap();
        write_varint(buf, entries.len() as i32).unwrap();
        for &(name, value) in entries {
            write_varint(buf, (name.len() + 1) as i32).unwrap();
            write_varint(buf, value.len() as i32).unwrap();
            buf.extend_from_slice(name);
            buf.push(0);
            buf.extend_from_slice(value);
        }
    }

    /// Helper to append a cache-hit xattr reference.
    fn write_xattr_cache_hit(buf: &mut Vec<u8>, index: u32) {
        write_varint(buf, (index + 1) as i32).unwrap();
    }

    #[test]
    fn read_entry_with_xattr() {
        use crate::flist::write::FileListWriter;

        let protocol = test_protocol();
        let mut data = Vec::new();

        let mut writer = FileListWriter::new(protocol);
        let mut entry = FileEntry::new_file("test_xattr.txt".into(), 300, 0o100644);
        entry.set_mtime(1700000000, 0);
        writer.write_entry(&mut data, &entry).unwrap();

        write_literal_xattr(
            &mut data,
            &[(b"mime_type", b"text/plain"), (b"tag", b"test")],
        );

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_xattrs(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "test_xattr.txt");
        assert_eq!(read_entry.size(), 300);
        assert_eq!(read_entry.xattr_ndx(), Some(0));

        let cached = reader.xattr_cache().get(0).unwrap();
        assert_eq!(cached.len(), 2);
        assert_eq!(
            cached.entries()[0].name(),
            expected_local_name(b"mime_type")
        );
        assert_eq!(cached.entries()[0].datum(), b"text/plain");
        assert_eq!(cached.entries()[1].name(), expected_local_name(b"tag"));
        assert_eq!(cached.entries()[1].datum(), b"test");

        assert_eq!(cursor.position() as usize, data.len());
    }

    #[test]
    fn read_entry_without_preserve_xattrs_skips_xattr() {
        use crate::flist::write::FileListWriter;

        let protocol = test_protocol();
        let mut data = Vec::new();

        let mut writer = FileListWriter::new(protocol);
        let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o100644);
        entry.set_mtime(1700000000, 0);
        writer.write_entry(&mut data, &entry).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert!(read_entry.xattr_ndx().is_none());
        assert_eq!(cursor.position() as usize, data.len());
    }

    #[test]
    fn multiple_entries_share_cached_xattrs() {
        use crate::flist::write::FileListWriter;

        let protocol = test_protocol();
        let mut data = Vec::new();

        let mut writer = FileListWriter::new(protocol);
        let mut entry = FileEntry::new_file("file1.txt".into(), 100, 0o100644);
        entry.set_mtime(1700000000, 0);
        writer.write_entry(&mut data, &entry).unwrap();
        write_literal_xattr(&mut data, &[(b"attr", b"value")]);

        let mut entry = FileEntry::new_file("file2.txt".into(), 200, 0o100644);
        entry.set_mtime(1700000000, 0);
        writer.write_entry(&mut data, &entry).unwrap();
        write_xattr_cache_hit(&mut data, 0);

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_xattrs(true);

        let entry1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let entry2 = reader.read_entry(&mut cursor).unwrap().unwrap();

        assert_eq!(entry1.name(), "file1.txt");
        assert_eq!(entry1.xattr_ndx(), Some(0));
        assert_eq!(entry2.name(), "file2.txt");
        assert_eq!(entry2.xattr_ndx(), Some(0));

        assert_eq!(reader.xattr_cache().len(), 1);

        assert_eq!(cursor.position() as usize, data.len());
    }

    #[test]
    fn read_directory_entry_with_xattr() {
        use crate::flist::write::FileListWriter;

        let protocol = test_protocol();
        let mut data = Vec::new();

        let mut writer = FileListWriter::new(protocol);
        let mut entry = FileEntry::new_directory("mydir".into(), 0o755);
        entry.set_mtime(1700000000, 0);
        writer.write_entry(&mut data, &entry).unwrap();

        write_literal_xattr(
            &mut data,
            &[(b"selinux_context", b"system_u:object_r:default_t:s0")],
        );

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_xattrs(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert!(read_entry.is_dir());
        assert_eq!(read_entry.xattr_ndx(), Some(0));

        let cached = reader.xattr_cache().get(0).unwrap();
        assert_eq!(cached.len(), 1);
        assert_eq!(
            cached.entries()[0].name(),
            expected_local_name(b"selinux_context")
        );

        assert_eq!(cursor.position() as usize, data.len());
    }

    /// Symlink entries also receive xattr data (unlike ACLs, xattrs apply
    /// to all file types). Upstream: xattrs.c does not exclude symlinks.
    #[test]
    fn read_symlink_entry_with_xattr() {
        use crate::flist::write::FileListWriter;

        let protocol = test_protocol();
        let mut data = Vec::new();

        let mut writer = FileListWriter::new(protocol).with_preserve_links(true);
        let mut entry = FileEntry::new_symlink("link".into(), "target".into());
        entry.set_mtime(1700000000, 0);
        writer.write_entry(&mut data, &entry).unwrap();

        write_literal_xattr(&mut data, &[(b"symattr", b"symval")]);

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_xattrs(true)
            .with_preserve_links(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert!(read_entry.is_symlink());
        assert_eq!(read_entry.xattr_ndx(), Some(0));

        assert_eq!(cursor.position() as usize, data.len());
    }

    /// When both ACLs and xattrs are enabled, ACLs are read first
    /// then xattrs, matching upstream wire order.
    #[test]
    fn read_entry_with_acl_and_xattr() {
        use crate::acl::{AclCache, AclType, RsyncAcl, send_rsync_acl};
        use crate::flist::write::FileListWriter;

        let protocol = test_protocol();
        let mut data = Vec::new();

        let mut writer = FileListWriter::new(protocol);
        let mut entry = FileEntry::new_file("both.txt".into(), 150, 0o100644);
        entry.set_mtime(1700000000, 0);
        writer.write_entry(&mut data, &entry).unwrap();

        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x06;
        acl.group_obj = 0x04;
        acl.other_obj = 0x04;
        let mut acl_cache = AclCache::new();
        send_rsync_acl(&mut data, &acl, AclType::Access, &mut acl_cache, false).unwrap();

        write_literal_xattr(&mut data, &[(b"key", b"val")]);

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol)
            .with_preserve_acls(true)
            .with_preserve_xattrs(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.name(), "both.txt");
        assert_eq!(read_entry.acl_ndx(), Some(0));
        assert_eq!(read_entry.xattr_ndx(), Some(0));

        let cached_acl = reader.acl_cache().get_access(0).unwrap();
        assert_eq!(cached_acl.user_obj, 0x06);

        let cached_xattr = reader.xattr_cache().get(0).unwrap();
        assert_eq!(cached_xattr.len(), 1);
        assert_eq!(
            cached_xattr.entries()[0].name(),
            expected_local_name(b"key")
        );
        assert_eq!(cached_xattr.entries()[0].datum(), b"val");

        assert_eq!(cursor.position() as usize, data.len());
    }

    #[test]
    fn read_entry_with_empty_xattr_set() {
        use crate::flist::write::FileListWriter;

        let protocol = test_protocol();
        let mut data = Vec::new();

        let mut writer = FileListWriter::new(protocol);
        let mut entry = FileEntry::new_file("empty_xattr.txt".into(), 50, 0o100644);
        entry.set_mtime(1700000000, 0);
        writer.write_entry(&mut data, &entry).unwrap();

        write_literal_xattr(&mut data, &[]);

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_xattrs(true);

        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(read_entry.xattr_ndx(), Some(0));

        let cached = reader.xattr_cache().get(0).unwrap();
        assert!(cached.is_empty());

        assert_eq!(cursor.position() as usize, data.len());
    }

    #[test]
    fn multiple_distinct_xattr_sets() {
        use crate::flist::write::FileListWriter;

        let protocol = test_protocol();
        let mut data = Vec::new();

        let mut writer = FileListWriter::new(protocol);

        let mut entry = FileEntry::new_file("a.txt".into(), 100, 0o100644);
        entry.set_mtime(1700000000, 0);
        writer.write_entry(&mut data, &entry).unwrap();
        write_literal_xattr(&mut data, &[(b"color", b"red")]);

        let mut entry = FileEntry::new_file("b.txt".into(), 200, 0o100644);
        entry.set_mtime(1700000000, 0);
        writer.write_entry(&mut data, &entry).unwrap();
        write_literal_xattr(&mut data, &[(b"color", b"blue")]);

        let mut entry = FileEntry::new_file("c.txt".into(), 300, 0o100644);
        entry.set_mtime(1700000000, 0);
        writer.write_entry(&mut data, &entry).unwrap();
        write_xattr_cache_hit(&mut data, 0);

        let mut cursor = Cursor::new(&data[..]);
        let mut reader = FileListReader::new(protocol).with_preserve_xattrs(true);

        let e1 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let e2 = reader.read_entry(&mut cursor).unwrap().unwrap();
        let e3 = reader.read_entry(&mut cursor).unwrap().unwrap();

        assert_eq!(e1.xattr_ndx(), Some(0));
        assert_eq!(e2.xattr_ndx(), Some(1));
        assert_eq!(e3.xattr_ndx(), Some(0)); // cache hit

        assert_eq!(reader.xattr_cache().len(), 2);

        let first = reader.xattr_cache().get(0).unwrap();
        assert_eq!(first.entries()[0].datum(), b"red");

        let second = reader.xattr_cache().get(1).unwrap();
        assert_eq!(second.entries()[0].datum(), b"blue");

        assert_eq!(cursor.position() as usize, data.len());
    }
}

/// Tests for filename encoding conversion (--iconv) on the receiver flist
/// ingest path.
///
/// Gated to unix: every helper and test inside builds non-UTF8 bytes via
/// `OsStrExt::from_bytes`, which has no Windows equivalent. Without the unix
/// gate the inner `use` items become unused on Windows under `-D warnings`.
///
/// upstream: flist.c recv_file_entry() iconv_buf(ic_recv, ...)
#[cfg(all(feature = "iconv", unix))]
mod iconv_integration {
    use super::*;
    use crate::flist::write::FileListWriter;
    use crate::iconv::FilenameConverter;

    /// Constructs wire bytes for a single file entry whose on-wire filename is
    /// the supplied raw byte sequence. Uses a writer with no iconv configured
    /// so the bytes hit the wire unchanged.
    #[cfg(unix)]
    fn write_entry_with_raw_name(wire_name: &[u8]) -> Vec<u8> {
        use std::os::unix::ffi::OsStrExt;
        use std::path::PathBuf;

        let path = PathBuf::from(std::ffi::OsStr::from_bytes(wire_name));
        let mut entry = FileEntry::new_file(path, 100, 0o100644);
        entry.set_mtime(1_700_000_000, 0);

        let protocol = test_protocol();
        let mut writer = FileListWriter::new(protocol);
        let mut data = Vec::new();
        writer.write_entry(&mut data, &entry).unwrap();
        data
    }

    /// Receiver decodes ISO-8859-1 wire bytes into UTF-8 local bytes when an
    /// `--iconv=UTF-8,ISO-8859-1` converter is wired into the file list reader.
    #[cfg(unix)]
    #[test]
    fn read_entry_converts_latin1_wire_bytes_to_utf8() {
        // "café" in ISO-8859-1: c(0x63) a(0x61) f(0x66) é(0xe9)
        let latin1_wire: &[u8] = &[0x63, 0x61, 0x66, 0xe9];
        // "café" in UTF-8: c(0x63) a(0x61) f(0x66) é(0xc3 0xa9)
        let utf8_local: &[u8] = &[0x63, 0x61, 0x66, 0xc3, 0xa9];

        let data = write_entry_with_raw_name(latin1_wire);

        let converter = FilenameConverter::new("UTF-8", "ISO-8859-1").unwrap();
        let protocol = test_protocol();
        let mut reader = FileListReader::new(protocol).with_iconv(converter);

        let mut cursor = io::Cursor::new(&data[..]);
        let entry = reader.read_entry(&mut cursor).unwrap().unwrap();

        // Stored filename bytes must be local (UTF-8), not the wire bytes.
        assert_eq!(entry.name_bytes().as_ref(), utf8_local);
    }

    /// Without a converter, wire bytes pass through unchanged.
    #[cfg(unix)]
    #[test]
    fn read_entry_without_iconv_preserves_wire_bytes() {
        let wire: &[u8] = &[0x63, 0x61, 0x66, 0xe9];
        let data = write_entry_with_raw_name(wire);

        let protocol = test_protocol();
        let mut reader = FileListReader::new(protocol);
        let mut cursor = io::Cursor::new(&data[..]);
        let entry = reader.read_entry(&mut cursor).unwrap().unwrap();

        assert_eq!(entry.name_bytes().as_ref(), wire);
    }

    /// An identity converter (UTF-8 <-> UTF-8) is a no-op.
    #[cfg(unix)]
    #[test]
    fn read_entry_with_identity_converter_is_noop() {
        let utf8_wire: &[u8] = &[0x63, 0x61, 0x66, 0xc3, 0xa9];
        let data = write_entry_with_raw_name(utf8_wire);

        let converter = FilenameConverter::identity();
        let protocol = test_protocol();
        let mut reader = FileListReader::new(protocol).with_iconv(converter);
        let mut cursor = io::Cursor::new(&data[..]);
        let entry = reader.read_entry(&mut cursor).unwrap().unwrap();

        assert_eq!(entry.name_bytes().as_ref(), utf8_wire);
    }

    /// Constructs wire bytes for a single symlink entry whose on-wire target is
    /// the supplied raw byte sequence. Writer has no iconv configured so the
    /// bytes hit the wire unchanged.
    #[cfg(unix)]
    fn write_symlink_with_raw_target(target_wire: &[u8]) -> Vec<u8> {
        use std::os::unix::ffi::OsStrExt;
        use std::path::PathBuf;

        let target = PathBuf::from(std::ffi::OsStr::from_bytes(target_wire));
        let mut entry = FileEntry::new_symlink("link".into(), target);
        entry.set_mtime(1_700_000_000, 0);

        let protocol = test_protocol();
        let mut writer = FileListWriter::new(protocol).with_preserve_links(true);
        let mut data = Vec::new();
        writer.write_entry(&mut data, &entry).unwrap();
        data
    }

    /// Symlink targets are decoded by `ic_recv` when `--iconv=LOCAL,REMOTE`
    /// is in effect and CF_SYMLINK_ICONV has been negotiated, mirroring the
    /// upstream `sender_symlink_iconv` path.
    ///
    /// upstream: flist.c:1127-1150 recv_file_entry() - sender_symlink_iconv
    #[cfg(unix)]
    #[test]
    fn read_symlink_target_converts_latin1_wire_bytes_to_utf8() {
        use std::os::unix::ffi::OsStrExt;

        // "café" target on the wire in ISO-8859-1
        let latin1_wire: &[u8] = &[0x63, 0x61, 0x66, 0xe9];
        // "café" decoded into UTF-8 local form
        let utf8_local: &[u8] = &[0x63, 0x61, 0x66, 0xc3, 0xa9];

        let data = write_symlink_with_raw_target(latin1_wire);

        let converter = FilenameConverter::new("UTF-8", "ISO-8859-1").unwrap();
        let protocol = test_protocol();
        let mut reader = FileListReader::new(protocol)
            .with_preserve_links(true)
            .with_iconv(converter);

        let mut cursor = io::Cursor::new(&data[..]);
        let entry = reader.read_entry(&mut cursor).unwrap().unwrap();

        let target = entry.link_target().expect("symlink target present");
        assert_eq!(target.as_os_str().as_bytes(), utf8_local);
    }

    /// Without a converter, the symlink target wire bytes pass through
    /// unchanged into the local-side `PathBuf`.
    #[cfg(unix)]
    #[test]
    fn read_symlink_target_without_iconv_preserves_wire_bytes() {
        use std::os::unix::ffi::OsStrExt;

        let wire: &[u8] = &[0x63, 0x61, 0x66, 0xe9];
        let data = write_symlink_with_raw_target(wire);

        let protocol = test_protocol();
        let mut reader = FileListReader::new(protocol).with_preserve_links(true);
        let mut cursor = io::Cursor::new(&data[..]);
        let entry = reader.read_entry(&mut cursor).unwrap().unwrap();

        let target = entry.link_target().expect("symlink target present");
        assert_eq!(target.as_os_str().as_bytes(), wire);
    }
}
