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

/// upstream: flist.c:787-800 - the sender names an owner from its id list
/// (`add_uid()`/`add_gid()`), not from the file entry. A nameless entry
/// still sends the names the caller hands the writer, and only on the ids'
/// first sighting.
#[test]
fn pending_owner_names_name_a_nameless_entry_once() {
    use super::super::super::read::FileListReader;
    use std::io::Cursor;

    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true)
        .with_name_follows(true);
    let mut buf = Vec::new();
    for name in ["a", "b"] {
        let mut entry = FileEntry::new_file(name.into(), 1, 0o644);
        entry.set_uid(1000);
        entry.set_gid(2000);
        let user = writer.needs_user_name(1000).then(|| "alice".to_string());
        let group = writer.needs_group_name(2000).then(|| "staff".to_string());
        writer.set_pending_owner_names(user, group);
        writer.write_entry(&mut buf, &entry).unwrap();
    }
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true);
    let first = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(first.user_name(), Some("alice"));
    assert_eq!(first.group_name(), Some("staff"));
    let second = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(second.user_name(), None);
    assert_eq!(second.group_name(), None);
}

/// A pending name belongs to one entry: the next entry, a new owner the
/// caller did not name, must not inherit it.
#[test]
fn pending_owner_names_do_not_leak_into_the_next_entry() {
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_uid(true)
        .with_name_follows(true);
    let mut first = FileEntry::new_file("a".into(), 1, 0o644);
    first.set_uid(1000);
    writer.set_pending_owner_names(Some("alice".to_string()), None);
    writer.write_entry(&mut Vec::new(), &first).unwrap();

    let mut second = FileEntry::new_file("b".into(), 1, 0o644);
    second.set_uid(1001);
    let mut buf = Vec::new();
    writer.write_entry(&mut buf, &second).unwrap();
    assert!(!buf.windows(5).any(|w| w == b"alice"));
}

/// upstream: flist.c:789-790 - inline names need `inc_recurse`; without it
/// the names travel only in the trailing id list, so none is asked for.
#[test]
fn no_inline_owner_name_is_needed_without_name_follows() {
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let writer = FileListWriter::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true);
    assert!(!writer.needs_user_name(1000));
    assert!(!writer.needs_group_name(2000));
}
