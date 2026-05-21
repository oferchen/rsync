use super::*;

#[test]
fn write_block_device_round_trip_protocol_30() {
    use super::super::super::read::FileListReader;
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
    use super::super::super::read::FileListReader;
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
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = test_protocol();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let entry = FileEntry::new_block_device("sda".into(), 0o660, 8, 0);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "sda");
    assert!(read_entry.is_block_device());
    assert!(read_entry.rdev_major().is_none());
    assert!(read_entry.rdev_minor().is_none());
}

#[test]
fn write_multiple_devices_with_same_major_compression() {
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    // Sharing rdev_major triggers XMIT_SAME_RDEV_MAJOR on the second entry.
    let entry1 = FileEntry::new_block_device("sda".into(), 0o660, 8, 0);
    let entry2 = FileEntry::new_block_device("sdb".into(), 0o660, 8, 16);

    writer.write_entry(&mut buf, &entry1).unwrap();
    let first_len = buf.len();
    writer.write_entry(&mut buf, &entry2).unwrap();
    let second_len = buf.len() - first_len;
    writer.write_end(&mut buf, None).unwrap();

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
fn special_file_fifo_round_trip_protocol_30() {
    use super::super::super::read::FileListReader;
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
    assert!(read_entry.rdev_major().is_none());
}

#[test]
fn special_file_socket_round_trip_protocol_30() {
    use super::super::super::read::FileListReader;
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
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(31u8).unwrap();
    let mut buf_30 = Vec::new();
    let mut buf_31 = Vec::new();

    // Protocol 30 emits a dummy rdev for FIFOs; protocol 31 omits it entirely.
    let mut writer30 = FileListWriter::new(ProtocolVersion::try_from(30u8).unwrap())
        .with_preserve_devices(true)
        .with_preserve_specials(true);
    let entry = FileEntry::new_fifo("fifo".into(), 0o644);
    writer30.write_entry(&mut buf_30, &entry).unwrap();

    let mut writer31 = FileListWriter::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);
    writer31.write_entry(&mut buf_31, &entry).unwrap();

    assert!(
        buf_31.len() < buf_30.len(),
        "protocol 31 should not write rdev for FIFOs"
    );

    let mut cursor = Cursor::new(&buf_31[..]);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_devices(true)
        .with_preserve_specials(true);
    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "fifo");
    assert!(read_entry.is_special());
}

/// Protocol 30 writes a dummy rdev for FIFOs/sockets; protocol 31+ omits it.
#[test]
fn special_file_rdev_protocol_30_vs_31() {
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
    use super::super::super::read::FileListReader;
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
        assert!(
            read_entry.rdev_major().is_none(),
            "protocol {proto_ver} FIFO should not have rdev"
        );
    }
}

#[test]
fn device_round_trip_protocol_28_29() {
    // Protocol 28-29 uses XMIT_RDEV_MINOR_8_PRE30 flag for 8-bit minors
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    for proto_ver in [28u8, 29u8] {
        let protocol = ProtocolVersion::try_from(proto_ver).unwrap();
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol)
            .with_preserve_devices(true)
            .with_preserve_specials(true);

        let dev_small_minor = FileEntry::new_block_device("sda".into(), 0o660, 8, 0);
        // Minor 300 exceeds 8 bits, so XMIT_RDEV_MINOR_8_PRE30 must not be set.
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
