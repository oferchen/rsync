use super::*;

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
    entry.set_mtime(1700000000, 0);
    entry.set_uid(1000);
    entry.set_gid(1000);

    let mut buf = Vec::new();
    writer.write_entry(&mut buf, &entry).unwrap();

    let (flags_value, _) = decode_varint(&buf).unwrap();

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
    use super::super::super::read::FileListReader;
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
    use super::super::super::read::FileListReader;
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
    use super::super::super::read::FileListReader;
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
    use super::super::super::read::FileListReader;
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
    use super::super::super::read::FileListReader;
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
