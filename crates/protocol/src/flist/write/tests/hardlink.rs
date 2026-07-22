use std::io::Cursor;

use super::super::super::read::FileListReader;
use super::*;

/// Decodes every entry from `buf`, threading the accumulating segment so
/// abbreviated hardlink followers resolve their leader - exactly as the real
/// receiver does (it passes `file_list[seg_start..]` to
/// [`FileListReader::read_entry_with_flist`]). Panics on decode error.
fn read_all(reader: &mut FileListReader, buf: &[u8]) -> Vec<FileEntry> {
    let mut cursor = Cursor::new(buf);
    let mut out: Vec<FileEntry> = Vec::new();
    while let Some(entry) = reader
        .read_entry_with_flist(&mut cursor, &out.clone())
        .expect("decode entry")
    {
        out.push(entry);
    }
    out
}

#[test]
fn write_hardlink_first_round_trip_protocol_30() {
    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

    let mut entry = FileEntry::new_file("file1.txt".into(), 100, 0o644);
    entry.set_hardlink_idx(u32::MAX); // u32::MAX marks the leader of a hardlink group.

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
    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

    // A follower must reference an already-seen leader (upstream flist.c:794).
    // Write the leader at NDX 0 and the follower at NDX 1 pointing back to it.
    let mut leader = FileEntry::new_file("file1.txt".into(), 100, 0o644);
    leader.set_hardlink_idx(u32::MAX);
    let mut follower = FileEntry::new_file("file2.txt".into(), 100, 0o644);
    follower.set_hardlink_idx(0);

    writer.write_entry(&mut buf, &leader).unwrap();
    writer.write_entry(&mut buf, &follower).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);
    let entries = read_all(&mut reader, &buf);

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].name(), "file2.txt");
    assert_eq!(entries[1].hardlink_idx(), Some(0));
}

#[test]
fn write_hardlink_without_preserve_hard_links_omits_idx() {
    let protocol = test_protocol();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let mut entry = FileEntry::new_file("file1.txt".into(), 100, 0o644);
    entry.set_hardlink_idx(5);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "file1.txt");
    assert!(read_entry.hardlink_idx().is_none());
}

#[test]
fn write_hardlink_group_round_trip_protocol_32() {
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

    let mut entry1 = FileEntry::new_file("original.txt".into(), 500, 0o644);
    entry1.set_hardlink_idx(u32::MAX);

    let mut entry2 = FileEntry::new_file("link1.txt".into(), 500, 0o644);
    entry2.set_hardlink_idx(0);

    let mut entry3 = FileEntry::new_file("link2.txt".into(), 500, 0o644);
    entry3.set_hardlink_idx(0);

    writer.write_entry(&mut buf, &entry1).unwrap();
    writer.write_entry(&mut buf, &entry2).unwrap();
    writer.write_entry(&mut buf, &entry3).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);
    let entries = read_all(&mut reader, &buf);

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].name(), "original.txt");
    assert_eq!(entries[0].hardlink_idx(), Some(u32::MAX));

    assert_eq!(entries[1].name(), "link1.txt");
    assert_eq!(entries[1].hardlink_idx(), Some(0));

    assert_eq!(entries[2].name(), "link2.txt");
    assert_eq!(entries[2].hardlink_idx(), Some(0));
}

#[test]
fn write_hardlink_follower_skips_metadata() {
    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

    let mut entry1 = FileEntry::new_file("original.txt".into(), 500, 0o644);
    entry1.set_mtime(1700000000, 0);
    entry1.set_hardlink_idx(u32::MAX);

    let mut entry2 = FileEntry::new_file("link.txt".into(), 500, 0o644);
    entry2.set_mtime(1700000000, 0);
    entry2.set_hardlink_idx(0);

    writer.write_entry(&mut buf, &entry1).unwrap();
    let first_len = buf.len();
    writer.write_entry(&mut buf, &entry2).unwrap();
    let second_len = buf.len() - first_len;
    writer.write_end(&mut buf, None).unwrap();

    assert!(
        second_len < first_len / 2,
        "follower entry should be much smaller: {second_len} vs {first_len}"
    );

    let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);
    let entries = read_all(&mut reader, &buf);

    assert_eq!(entries[0].name(), "original.txt");
    assert_eq!(entries[0].size(), 500);
    assert_eq!(entries[0].mtime(), 1700000000);
    assert_eq!(entries[0].hardlink_idx(), Some(u32::MAX));

    // Follower metadata is skipped on the wire; the receiver copies it from the
    // leader in the same segment (upstream flist.c:recv_file_entry lines
    // 805-834), so size and mtime match the leader rather than being blank.
    assert_eq!(entries[1].name(), "link.txt");
    assert_eq!(entries[1].size(), 500);
    assert_eq!(entries[1].mtime(), 1700000000);
    assert_eq!(entries[1].hardlink_idx(), Some(0));
}

#[test]
fn write_hardlink_follower_with_uid_gid_skips_all() {
    let protocol = ProtocolVersion::try_from(32u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_hard_links(true)
        .with_preserve_uid(true)
        .with_preserve_gid(true)
        .with_name_follows(true);

    let mut entry1 = FileEntry::new_file("leader.txt".into(), 1000, 0o755);
    entry1.set_mtime(1700000000, 0);
    entry1.set_uid(1000);
    entry1.set_gid(1000);
    entry1.set_user_name("testuser".to_string());
    entry1.set_group_name("testgroup".to_string());
    entry1.set_hardlink_idx(u32::MAX);

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

    assert!(
        second_len < first_len / 2,
        "follower should skip metadata: {second_len} vs {first_len}"
    );

    let mut reader = FileListReader::new(protocol)
        .with_preserve_hard_links(true)
        .with_preserve_uid(true)
        .with_preserve_gid(true);
    let entries = read_all(&mut reader, &buf);

    assert_eq!(entries[0].user_name(), Some("testuser"));
    assert_eq!(entries[0].group_name(), Some("testgroup"));

    // The follower inherits size/mtime/mode/uid/gid from the leader in the same
    // segment; the id->name strings are not part of the copied metadata.
    assert_eq!(entries[1].size(), 1000);
    assert_eq!(entries[1].mtime(), 1700000000);
    assert_eq!(entries[1].mode(), entries[0].mode());
    assert_eq!(entries[1].uid(), entries[0].uid());
    assert_eq!(entries[1].gid(), entries[0].gid());
    assert_eq!(entries[1].user_name(), None);
    assert_eq!(entries[1].group_name(), None);
    assert_eq!(entries[1].hardlink_idx(), Some(0));
}

#[test]
fn write_hardlink_leader_has_full_metadata() {
    let protocol = ProtocolVersion::try_from(30u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

    let mut entry = FileEntry::new_file("leader.txt".into(), 500, 0o644);
    entry.set_mtime(1700000000, 0);
    entry.set_hardlink_idx(u32::MAX);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();

    assert_eq!(read_entry.name(), "leader.txt");
    assert_eq!(read_entry.size(), 500);
    assert_eq!(read_entry.mtime(), 1700000000);
    assert_eq!(read_entry.hardlink_idx(), Some(u32::MAX));
}

#[test]
fn is_hardlink_follower_helper() {
    let writer = FileListWriter::new(test_protocol()).with_preserve_hard_links(true);

    let xflags_none: u32 = 0;
    assert!(!writer.is_hardlink_follower(xflags_none));

    // HLINKED + HLINK_FIRST: leader, not a follower.
    let xflags_leader = ((XMIT_HLINKED as u32) << 8) | ((XMIT_HLINK_FIRST as u32) << 8);
    assert!(!writer.is_hardlink_follower(xflags_leader));

    // HLINKED only: follower.
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
fn hardlink_dev_ino_round_trip_protocol_29() {
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
    let protocol = ProtocolVersion::try_from(29u8).unwrap();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

    // Sharing a dev across consecutive entries triggers XMIT_SAME_DEV_PRE30.
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

/// Verifies hardlink indices survive a write-read round-trip when directory
/// entries are interspersed among hardlinked files. This simulates the
/// `--relative` scenario where implied directories occupy wire NDX positions
/// between hardlinked files, shifting the follower's index value.
///
/// upstream: generator.c - send_implied_dirs() creates FLAG_IMPLIED_DIR entries
/// that occupy wire positions but are not hardlinked themselves.
#[test]
fn hardlink_round_trip_with_interspersed_directories() {
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

    let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);
    let entries = read_all(&mut reader, &buf);

    // Directory entries have no hardlink index
    assert_eq!(entries[0].hardlink_idx(), None);
    assert_eq!(entries[2].hardlink_idx(), None);

    // Leader round-trips with u32::MAX
    assert_eq!(entries[1].name(), "a/orig.txt");
    assert_eq!(entries[1].hardlink_idx(), Some(u32::MAX));

    // Follower round-trips with the correct wire NDX pointing to the leader
    assert_eq!(entries[3].name(), "b/link.txt");
    assert_eq!(entries[3].hardlink_idx(), Some(1));
}

/// Verifies that multiple hardlink groups with directories interspersed all
/// resolve correctly. Two separate hardlink groups with implied directories
/// between them must maintain independent leader/follower relationships.
#[test]
fn hardlink_multiple_groups_with_directories() {
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

    let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);
    let entries = read_all(&mut reader, &buf);

    // NDX 1/4 are the group leaders; NDX 2/5 the followers referencing them.
    assert_eq!(entries[1].hardlink_idx(), Some(u32::MAX));
    assert_eq!(entries[2].hardlink_idx(), Some(1));
    assert_eq!(entries[4].hardlink_idx(), Some(u32::MAX));
    assert_eq!(entries[5].hardlink_idx(), Some(4));
}
