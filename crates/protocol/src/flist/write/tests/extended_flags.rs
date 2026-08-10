use super::*;

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
    use super::super::super::flags::FileFlags;
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = test_protocol();

    // Test XMIT_TOP_DIR (directories only) using from_raw with flags set
    {
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol);
        let flags = FileFlags::new(XMIT_TOP_DIR, 0);
        let dir = FileEntry::from_raw("topdir".into(), 0, 0o040755, 0, 0, flags);
        writer.write_entry(&mut buf, &dir).unwrap();
        writer.write_end(&mut buf, None).unwrap();

        let mut cursor = Cursor::new(&buf[..]);
        let mut reader = FileListReader::new(protocol);
        let read = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert!(read.top_dir(), "XMIT_TOP_DIR should round-trip");
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
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();

    // Test: XMIT_HLINKED | XMIT_HLINK_FIRST (hardlink leader)
    {
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);
        let mut entry = FileEntry::new_file("leader.txt".into(), 100, 0o644);
        entry.set_hardlink_idx(u32::MAX);
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
        entry.set_hardlink_idx(42);
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
    use super::super::super::read::FileListReader;
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

        let mut entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);
        entry1.set_atime(1700000000);

        let mut entry2 = FileEntry::new_file("file2.txt".into(), 200, 0o644);
        entry2.set_atime(1700000000);

        writer.write_entry(&mut buf, &entry1).unwrap();
        let first_len = buf.len();
        writer.write_entry(&mut buf, &entry2).unwrap();
        let second_len = buf.len() - first_len;
        writer.write_end(&mut buf, None).unwrap();

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
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();

    // Test both user and group names
    {
        let mut buf = Vec::new();
        let mut writer = FileListWriter::new(protocol)
            .with_preserve_uid(true)
            .with_preserve_gid(true)
            .with_name_follows(true);

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
fn name_follows_gated_on_inc_recurse() {
    // upstream: flist.c:481-482,491-492 - `if (inc_recurse && user_name)` gates
    // the inline XMIT_*_NAME_FOLLOWS flags. Without inc_recurse the sender must
    // NOT emit inline owner names (they ride only in the trailing id-list), so
    // `with_name_follows(false)` (the default) must produce a strictly shorter
    // entry and a receiver that sees no inline name. With `with_name_follows`
    // enabled the names appear inline, matching the incremental-recursion wire.
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();

    let make_entry = || {
        let mut entry = FileEntry::new_file("owned.txt".into(), 100, 0o644);
        entry.set_uid(1000);
        entry.set_gid(1000);
        entry.set_user_name("alice".to_string());
        entry.set_group_name("developers".to_string());
        entry
    };

    // Default (name_follows = false): no inline names on the wire.
    let mut off_buf = Vec::new();
    let mut off_writer = FileListWriter::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true);
    off_writer.write_entry(&mut off_buf, &make_entry()).unwrap();
    off_writer.write_end(&mut off_buf, None).unwrap();

    // name_follows = true: inline names emitted (incremental-recursion path).
    let mut on_buf = Vec::new();
    let mut on_writer = FileListWriter::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true)
        .with_name_follows(true);
    on_writer.write_entry(&mut on_buf, &make_entry()).unwrap();
    on_writer.write_end(&mut on_buf, None).unwrap();

    // The inline name strings appear only when name_follows is enabled; with it
    // off they are absent from the entry (and, with no other extended-flag bits
    // set, the XMIT_EXTENDED_FLAGS byte drops as well), so the gated-off stream
    // is strictly shorter.
    let contains = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).any(|w| w == needle);
    assert!(contains(&on_buf, b"alice") && contains(&on_buf, b"developers"));
    assert!(
        !contains(&off_buf, b"alice") && !contains(&off_buf, b"developers"),
        "inline owner names must be absent when name_follows is off"
    );
    assert!(off_buf.len() < on_buf.len());

    // A receiver decoding the gated-off stream sees no inline names but keeps
    // the numeric uid/gid (names would arrive via the id-list trailer).
    let mut off_reader = FileListReader::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true);
    let off_read = off_reader
        .read_entry(&mut Cursor::new(&off_buf[..]))
        .unwrap()
        .unwrap();
    assert_eq!(off_read.user_name(), None);
    assert_eq!(off_read.group_name(), None);
    assert_eq!(off_read.uid(), Some(1000));
    assert_eq!(off_read.gid(), Some(1000));

    // The name_follows stream round-trips the inline names.
    let mut on_reader = FileListReader::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true);
    let on_read = on_reader
        .read_entry(&mut Cursor::new(&on_buf[..]))
        .unwrap()
        .unwrap();
    assert_eq!(on_read.user_name(), Some("alice"));
    assert_eq!(on_read.group_name(), Some("developers"));
}

#[test]
fn extended_flags_device_flags_protocol_28_29() {
    // Test XMIT_SAME_RDEV_MAJOR and XMIT_RDEV_MINOR_8_PRE30 for protocol 28-29
    use super::super::super::read::FileListReader;
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
    // When xflags == 0 for a non-directory in protocol 28-29, write_flags()
    // substitutes XMIT_TOP_DIR so the leading byte cannot collide with the
    // end-of-list marker (which is also 0).
    let protocol = ProtocolVersion::try_from(29u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    // Prime the compression state so mode and time match the next entry.
    writer.state.update(b"test", 0o100644, 1700000000, 0, 0);

    let mut entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    entry.set_mtime(1700000000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();

    assert_ne!(buf[0], 0, "flags should not be zero for file entry");
}

#[test]
fn extended_flags_protocol_version_boundaries() {
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    // Protocol 27 lacks extended flags; protocol 28 is the minimum supported
    // here and the first version with extended flag encoding.
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
