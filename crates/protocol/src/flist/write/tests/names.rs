use super::*;

#[test]
fn write_user_name_round_trip_protocol_30() {
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true)
        .with_name_follows(true);

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
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true)
        .with_name_follows(true);

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
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true)
        .with_name_follows(true);

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
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true)
        .with_name_follows(true);

    let mut entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);
    entry1.set_uid(1000);
    entry1.set_user_name("testuser".to_string());

    let mut entry2 = FileEntry::new_file("file2.txt".into(), 200, 0o644);
    entry2.set_uid(1000);
    entry2.set_user_name("testuser".to_string());

    writer.write_entry(&mut buf, &entry1).unwrap();
    let first_len = buf.len();
    writer.write_entry(&mut buf, &entry2).unwrap();
    let second_len = buf.len() - first_len;
    writer.write_end(&mut buf, None).unwrap();

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
    // Second entry inherits the user name from XMIT_SAME_UID compression and is
    // not retransmitted, so the reader exposes it as None.
    assert_eq!(read2.user_name(), None);
}

#[test]
fn write_names_omitted_for_protocol_29() {
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    // Protocol 29 doesn't support user/group name strings
    let protocol = ProtocolVersion::try_from(29u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true)
        .with_name_follows(true);

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
    assert_eq!(read_entry.user_name(), None);
    assert_eq!(read_entry.group_name(), None);
}

/// upstream: flist.c:783-791 - `add_uid()` names a uid only on its first
/// sighting (uidlist.c:340-354), so an owner that comes back after another
/// one is sent without its name, in a later sub-list as in the same one.
#[test]
fn returning_owner_is_not_named_again() {
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_uid(true)
        .with_name_follows(true);
    let mut named = Vec::new();
    for (name, uid, user) in [
        ("a", 1000, "alice"),
        ("b", 1001, "bob"),
        ("c", 1000, "alice"),
    ] {
        let mut entry = FileEntry::new_file(name.into(), 1, 0o644);
        entry.set_uid(uid);
        entry.set_user_name(user.to_string());
        let mut buf = Vec::new();
        writer.write_entry(&mut buf, &entry).unwrap();
        if name == "b" {
            // A new segment: the sightings belong to the transfer, not the list.
            writer.write_end(&mut Vec::new(), None).unwrap();
        }
        let user_bytes = user.as_bytes();
        named.push(buf.windows(user_bytes.len()).any(|w| w == user_bytes));
    }
    assert_eq!(named, [true, true, false]);
}
