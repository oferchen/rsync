use super::*;

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
    use super::super::super::read::FileListReader;
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
fn checksum_round_trip() {
    use super::super::super::read::FileListReader;
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
fn directory_content_dir_flag_round_trip() {
    use super::super::super::read::FileListReader;
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
