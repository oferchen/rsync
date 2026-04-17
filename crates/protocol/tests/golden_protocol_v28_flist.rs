//! Golden byte tests for protocol 28 file list (flist) encoding behavior.
//!
//! Supplements `golden_protocol_v28_wire.rs` with tests for flist-level
//! behaviors specific to protocol 28:
//!
//! - Sort order: plain lexicographic (no file-before-directory)
//! - XMIT_SAME_TIME / XMIT_SAME_MODE flag suppression across entries
//! - XMIT_LONG_NAME for filenames > 255 bytes
//! - XMIT_TOP_DIR substitution when xflags == 0 for directories
//! - Wire format divergence from protocol 30+ (varint vs fixed encoding)
//! - No INC_RECURSE NDX markers in flist
//!
//! # Upstream Reference
//!
//! Protocol 28 is the oldest supported version. Sort and encoding behavior
//! defined in `flist.c:f_name_cmp()` and `flist.c:send_file_entry()`.

use std::io::Cursor;

use protocol::flist::{
    compare_file_entries, sort_file_list, FileEntry, FileListReader, FileListWriter,
};
use protocol::ProtocolVersion;

fn proto28() -> ProtocolVersion {
    ProtocolVersion::try_from(28u8).unwrap()
}

fn proto30() -> ProtocolVersion {
    ProtocolVersion::from_supported(30).unwrap()
}

// ---------------------------------------------------------------------------
// Sort order: protocol 28 uses plain lexicographic, no file-before-directory
// upstream: flist.c:3223 - protocol_version >= 29 ? t_PATH : t_ITEM
// ---------------------------------------------------------------------------

/// Protocol 28 sorts directories and files by plain byte order, without
/// the file-before-directory rule used in protocol 29+.
///
/// At protocol 29+, files sort before directories at the same level.
/// At protocol 28, entries are sorted purely lexicographically, so a
/// directory "b" sorts before a file "c" (by name), not after.
#[test]
fn golden_v28_sort_plain_lexicographic() {
    // "a" (file) < "b" (dir) < "c" (file) in plain lexicographic order.
    // Protocol 29+ would sort files before dirs: "a","c" then "b".
    // Protocol 28 uses plain byte order: "a","b","c".
    let mut entries = vec![
        FileEntry::new_file("c".into(), 10, 0o644),
        FileEntry::new_directory("b".into(), 0o755),
        FileEntry::new_file("a".into(), 20, 0o644),
    ];

    sort_file_list(&mut entries, false, true); // protocol_pre29 = true

    assert_eq!(entries[0].name(), "a", "plain lexicographic: a first");
    assert_eq!(
        entries[1].name(),
        "b",
        "plain lexicographic: b (dir) second"
    );
    assert_eq!(entries[2].name(), "c", "plain lexicographic: c third");
}

/// Protocol 29+ sorts files before directories at the same level.
/// This test verifies the difference from protocol 28.
#[test]
fn golden_v28_sort_differs_from_v29() {
    let mut entries_pre29 = vec![
        FileEntry::new_directory("b".into(), 0o755),
        FileEntry::new_file("b".into(), 10, 0o644),
    ];
    let mut entries_v29 = entries_pre29.clone();

    sort_file_list(&mut entries_pre29, false, true); // protocol < 29
    sort_file_list(&mut entries_v29, false, false); // protocol >= 29

    // Protocol >= 29: file "b" sorts before directory "b" (files before dirs).
    assert!(entries_v29[0].is_file(), "v29+: file before dir");
    assert!(entries_v29[1].is_dir(), "v29+: dir after file");

    // Protocol < 29: file "b" and dir "b" are equal in plain lexicographic.
    // The stable sort preserves input order for equal elements.
    assert!(
        entries_pre29[0].is_dir(),
        "pre29: input order preserved for ties"
    );
    assert!(
        entries_pre29[1].is_file(),
        "pre29: input order preserved for ties"
    );
}

/// Protocol 28 sort does not add implicit trailing '/' to directories.
/// This changes ordering when a directory name is a prefix of a file name.
#[test]
fn golden_v28_sort_no_implicit_trailing_slash() {
    // "dir" (directory) vs "dir.txt" (file):
    // Protocol 29+: "dir" gets implicit '/'. Comparison: "dir/" vs "dir.txt"
    //   '/' (0x2F) < '.' (0x2E) is FALSE, so "dir/" > "dir.txt" at v29+.
    //   Actually '/' = 0x2F > '.' = 0x2E, so dir sorts after dir.txt at v29+.
    // Protocol 28: plain comparison "dir" vs "dir.txt" - "dir" < "dir.txt"
    //   because "dir" is a prefix and shorter.
    let mut entries_pre29 = vec![
        FileEntry::new_file("dir.txt".into(), 10, 0o644),
        FileEntry::new_directory("dir".into(), 0o755),
    ];
    let mut entries_v29 = entries_pre29.clone();

    sort_file_list(&mut entries_pre29, false, true);
    sort_file_list(&mut entries_v29, false, false);

    // Protocol 28: "dir" < "dir.txt" (prefix is shorter)
    assert_eq!(
        entries_pre29[0].name(),
        "dir",
        "pre29: shorter prefix comes first"
    );
    assert_eq!(entries_pre29[1].name(), "dir.txt");

    // Protocol 29+: "dir/" (with implicit slash) vs "dir.txt"
    // '/' (0x2F) > '.' (0x2E), so "dir/" > "dir.txt"
    assert_eq!(
        entries_v29[0].name(),
        "dir.txt",
        "v29+: file before dir when dir name is prefix"
    );
    assert_eq!(entries_v29[1].name(), "dir");
}

/// Protocol 28 sort treats "." as always first, matching all protocol versions.
#[test]
fn golden_v28_sort_dot_always_first() {
    let mut entries = vec![
        FileEntry::new_file("a".into(), 10, 0o644),
        FileEntry::new_directory(".".into(), 0o755),
        FileEntry::new_file("b".into(), 20, 0o644),
    ];

    sort_file_list(&mut entries, false, true); // protocol_pre29

    assert_eq!(entries[0].name(), ".", "dot is always first at proto 28");
    assert_eq!(entries[1].name(), "a");
    assert_eq!(entries[2].name(), "b");
}

/// Protocol 28 sort with nested paths uses plain byte comparison,
/// not the segment-aware comparison of protocol 29+.
#[test]
fn golden_v28_sort_nested_paths_plain_byte_order() {
    // Plain byte comparison: 'a' < 'a/' because '/' (0x2F) compared against nothing,
    // and the shorter string is "less" when it's a prefix.
    // But "a/z" vs "ab" at proto 28: 'a'='a', then '/' (0x2F) vs 'b' (0x62).
    // '/' < 'b', so "a/z" < "ab" at proto 28.
    // At proto 29+, "a/z" inside dir "a" would sort differently due to
    // segment-aware comparison.
    let mut entries = vec![
        FileEntry::new_file("ab".into(), 10, 0o644),
        FileEntry::new_file("a/z".into(), 20, 0o644),
    ];

    sort_file_list(&mut entries, false, true); // protocol_pre29

    // '/' (0x2F) < 'b' (0x62), so "a/z" comes before "ab"
    assert_eq!(entries[0].name(), "a/z", "pre29: slash byte < 'b' byte");
    assert_eq!(entries[1].name(), "ab");
}

// ---------------------------------------------------------------------------
// compare_file_entries (protocol 29+ comparison used by default)
// ---------------------------------------------------------------------------

/// Verify that compare_file_entries uses file-before-directory semantics
/// (protocol 29+ default), contrasting with sort_file_list's pre29 mode.
#[test]
fn golden_v28_compare_fn_uses_v29_semantics() {
    let file = FileEntry::new_file("same".into(), 10, 0o644);
    let dir = FileEntry::new_directory("same".into(), 0o755);

    // compare_file_entries always uses v29+ semantics (files before dirs)
    let result = compare_file_entries(&file, &dir);
    assert_eq!(
        result,
        std::cmp::Ordering::Less,
        "compare_file_entries: file < dir (v29+ semantics)"
    );
}

// ---------------------------------------------------------------------------
// XMIT_SAME_TIME flag: mtime suppression across entries
// upstream: flist.c - XMIT_SAME_TIME (0x80) omits mtime when equal to previous
// ---------------------------------------------------------------------------

/// When consecutive entries share the same mtime, the second entry sets
/// XMIT_SAME_TIME and omits the 4-byte mtime field.
#[test]
fn golden_v28_same_time_flag_suppresses_mtime() {
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let mut e1 = FileEntry::new_file("a.txt".into(), 100, 0o644);
    e1.set_mtime(1_700_000_000, 0);
    writer.write_entry(&mut buf, &e1).unwrap();
    let first_len = buf.len();

    let mut e2 = FileEntry::new_file("b.txt".into(), 200, 0o644);
    e2.set_mtime(1_700_000_000, 0); // Same mtime as e1
    writer.write_entry(&mut buf, &e2).unwrap();

    let second_bytes = &buf[first_len..];

    // XMIT_SAME_TIME = 0x80 must be set
    assert_ne!(
        second_bytes[0] & 0x80,
        0,
        "XMIT_SAME_TIME must be set when mtime matches previous"
    );

    // Second entry omits 4 bytes of mtime. First entry:
    //   flags(1) + name_len(1) + name(5) + size(4) + mtime(4) + mode(4) = 19
    // Second entry (same time + same mode):
    //   flags(1) + name_len(1) + name(5) + size(4) = 11
    let second_len = buf.len() - first_len;
    assert!(
        second_len < first_len,
        "second entry ({second_len} bytes) must be shorter than first ({first_len} bytes)"
    );
}

/// When mtime differs, XMIT_SAME_TIME is not set and 4 bytes are emitted.
#[test]
fn golden_v28_different_time_emits_mtime() {
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let mut e1 = FileEntry::new_file("a.txt".into(), 100, 0o644);
    e1.set_mtime(1_700_000_000, 0);
    writer.write_entry(&mut buf, &e1).unwrap();
    let first_len = buf.len();

    let mut e2 = FileEntry::new_file("b.txt".into(), 200, 0o644);
    e2.set_mtime(1_700_000_001, 0); // Different mtime
    writer.write_entry(&mut buf, &e2).unwrap();

    let second_bytes = &buf[first_len..];

    // XMIT_SAME_TIME must NOT be set
    assert_eq!(
        second_bytes[0] & 0x80,
        0,
        "XMIT_SAME_TIME must not be set when mtime differs"
    );
}

// ---------------------------------------------------------------------------
// XMIT_SAME_MODE flag: mode suppression across entries
// upstream: flist.c - XMIT_SAME_MODE (0x02) omits mode when equal to previous
// ---------------------------------------------------------------------------

/// When consecutive entries share the same mode, the second entry sets
/// XMIT_SAME_MODE and omits the 4-byte mode field.
#[test]
fn golden_v28_same_mode_flag_suppresses_mode() {
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let mut e1 = FileEntry::new_file("a.txt".into(), 100, 0o644);
    e1.set_mtime(1_700_000_000, 0);
    writer.write_entry(&mut buf, &e1).unwrap();
    let first_len = buf.len();

    let mut e2 = FileEntry::new_file("b.txt".into(), 200, 0o644); // Same mode 0o644
    e2.set_mtime(1_700_000_001, 0); // Different mtime to avoid same_time
    writer.write_entry(&mut buf, &e2).unwrap();

    let second_bytes = &buf[first_len..];

    // XMIT_SAME_MODE = 0x02 must be set
    assert_ne!(
        second_bytes[0] & 0x02,
        0,
        "XMIT_SAME_MODE must be set when mode matches previous"
    );

    // Second entry omits 4 bytes of mode.
    // Second: flags(1) + name_len(1) + name(5) + size(4) + mtime(4) = 15
    // (no mode bytes)
    let second_len = buf.len() - first_len;
    assert_eq!(
        second_len, 15,
        "second entry without mode: flags + name_len + name(5) + size(4) + mtime(4)"
    );
}

/// When mode differs between entries, XMIT_SAME_MODE is not set.
#[test]
fn golden_v28_different_mode_emits_mode() {
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let mut e1 = FileEntry::new_file("a.txt".into(), 100, 0o644);
    e1.set_mtime(1_700_000_000, 0);
    writer.write_entry(&mut buf, &e1).unwrap();
    let first_len = buf.len();

    let mut e2 = FileEntry::new_file("b.txt".into(), 200, 0o755); // Different mode
    e2.set_mtime(1_700_000_001, 0);
    writer.write_entry(&mut buf, &e2).unwrap();

    let second_bytes = &buf[first_len..];

    // XMIT_SAME_MODE must NOT be set
    assert_eq!(
        second_bytes[0] & 0x02,
        0,
        "XMIT_SAME_MODE must not be set when mode differs"
    );
}

// ---------------------------------------------------------------------------
// XMIT_LONG_NAME: filenames exceeding 255 bytes
// upstream: flist.c - XMIT_LONG_NAME (1<<6) triggers varint/int name length
// ---------------------------------------------------------------------------

/// Filenames with suffix > 255 bytes use XMIT_LONG_NAME flag, which
/// encodes the name length as a 4-byte LE int instead of a single byte.
#[test]
fn golden_v28_long_name_encoding() {
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let long_name = "x".repeat(300);
    let mut entry = FileEntry::new_file(long_name.clone().into(), 10, 0o644);
    entry.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();

    // XMIT_LONG_NAME = 0x40 must be set in flags
    assert_ne!(
        buf[0] & 0x40,
        0,
        "XMIT_LONG_NAME must be set for names > 255 bytes"
    );

    // Name length is encoded as 4-byte LE int after flags byte.
    // For protocol 28: write_int(300) = [0x2C, 0x01, 0x00, 0x00]
    assert_eq!(
        &buf[1..5],
        &300_i32.to_le_bytes(),
        "long name length as 4-byte LE int"
    );

    // Name bytes follow
    assert_eq!(&buf[5..305], long_name.as_bytes(), "name bytes");
}

/// Long filename round-trips correctly through write and read at proto 28.
#[test]
fn golden_v28_long_name_roundtrip() {
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let long_name = "a/".repeat(150); // 300 bytes
    let mut entry = FileEntry::new_file(long_name.clone().into(), 42, 0o644);
    entry.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), long_name, "long name must round-trip");
    assert_eq!(read_entry.size(), 42);
}

// ---------------------------------------------------------------------------
// Wire format divergence: protocol 28 (fixed) vs protocol 30 (varint)
// upstream: protocol 30 introduced varint encoding for flist fields
// ---------------------------------------------------------------------------

/// Protocol 28 and 30 produce different wire bytes for the same entry.
/// Protocol 28 uses fixed 4-byte LE for size, mtime, mode.
/// Protocol 30 uses varint encoding which is typically more compact.
#[test]
fn golden_v28_wire_format_differs_from_v30() {
    let v28 = proto28();
    let v30 = proto30();

    let mut buf_28 = Vec::new();
    let mut buf_30 = Vec::new();
    let mut writer_28 = FileListWriter::new(v28);
    let mut writer_30 = FileListWriter::new(v30);

    let mut entry = FileEntry::new_file("test.dat".into(), 4096, 0o644);
    entry.set_mtime(1_700_000_000, 0);

    writer_28.write_entry(&mut buf_28, &entry).unwrap();
    writer_30.write_entry(&mut buf_30, &entry).unwrap();

    assert_ne!(
        buf_28, buf_30,
        "v28 (fixed encoding) and v30 (varint) must differ"
    );

    // v28 fixed encoding is typically longer than v30 varint for small values
    assert!(
        buf_28.len() >= buf_30.len(),
        "v28 ({}) must be at least as long as v30 ({})",
        buf_28.len(),
        buf_30.len()
    );
}

/// Protocol 28 UID/GID uses 4-byte fixed LE; protocol 30 uses varint.
/// This test verifies the divergence with preserve_uid/gid enabled.
#[test]
fn golden_v28_uid_gid_encoding_differs_from_v30() {
    let v28 = proto28();
    let v30 = proto30();

    let mut buf_28 = Vec::new();
    let mut buf_30 = Vec::new();
    let mut writer_28 = FileListWriter::new(v28)
        .with_preserve_uid(true)
        .with_preserve_gid(true);
    let mut writer_30 = FileListWriter::new(v30)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

    let mut entry = FileEntry::new_file("f.txt".into(), 100, 0o644);
    entry.set_mtime(1_700_000_000, 0);
    entry.set_uid(1000);
    entry.set_gid(1000);

    writer_28.write_entry(&mut buf_28, &entry).unwrap();
    writer_30.write_entry(&mut buf_30, &entry).unwrap();

    // v28: UID and GID each use 4-byte LE = 8 bytes total for id fields
    // v30: UID and GID use varint = typically fewer bytes
    assert_ne!(buf_28, buf_30, "v28 and v30 uid/gid encoding must differ");
}

// ---------------------------------------------------------------------------
// No INC_RECURSE NDX markers in protocol 28 flist
// upstream: protocol 28 uses legacy ASCII negotiation, no binary compat flags
// ---------------------------------------------------------------------------

/// Protocol 28 file lists use a simple zero-byte terminator without
/// any INC_RECURSE segment markers or NDX framing.
#[test]
fn golden_v28_no_inc_recurse_in_flist() {
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let mut e1 = FileEntry::new_directory("dir1".into(), 0o755);
    e1.set_mtime(1_700_000_000, 0);

    let mut e2 = FileEntry::new_file("dir1/file.txt".into(), 100, 0o644);
    e2.set_mtime(1_700_000_000, 0);

    let mut e3 = FileEntry::new_directory("dir2".into(), 0o755);
    e3.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut buf, &e1).unwrap();
    writer.write_entry(&mut buf, &e2).unwrap();
    writer.write_entry(&mut buf, &e3).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    // The entire flist is one contiguous block terminated by a single 0x00.
    // No NDX_FLIST_EOF (-1) or other framing bytes between entries.
    assert_eq!(*buf.last().unwrap(), 0x00, "flist ends with zero byte");

    // Verify round-trip reads all 3 entries without NDX interruption
    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);

    let r1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r1.name(), "dir1");
    let r2 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r2.name(), "dir1/file.txt");
    let r3 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r3.name(), "dir2");
    assert!(reader.read_entry(&mut cursor).unwrap().is_none());
}

// ---------------------------------------------------------------------------
// Cumulative flag compression across many entries
// upstream: flist.c - state carries prev_uid, prev_gid, prev_mode, prev_mtime
// ---------------------------------------------------------------------------

/// Demonstrates maximum flag compression when all metadata fields match
/// between consecutive entries. Only name suffix and size vary.
#[test]
fn golden_v28_maximum_flag_compression() {
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    // First entry sets the baseline
    let mut e1 = FileEntry::new_file("dir/file001.dat".into(), 100, 0o644);
    e1.set_mtime(1_700_000_000, 0);
    writer.write_entry(&mut buf, &e1).unwrap();
    let first_len = buf.len();

    // Second entry: same mode, same mtime, shared name prefix "dir/file00"
    let mut e2 = FileEntry::new_file("dir/file002.dat".into(), 200, 0o644);
    e2.set_mtime(1_700_000_000, 0);
    writer.write_entry(&mut buf, &e2).unwrap();
    let second_len = buf.len() - first_len;

    // Third entry: same mode, same mtime, shared name prefix "dir/file00"
    let mut e3 = FileEntry::new_file("dir/file003.dat".into(), 300, 0o644);
    e3.set_mtime(1_700_000_000, 0);
    writer.write_entry(&mut buf, &e3).unwrap();
    let third_len = buf.len() - first_len - second_len;

    // First entry is the longest (no compression)
    assert!(
        first_len > second_len,
        "first entry ({first_len}) should be longer than second ({second_len})"
    );

    // Second and third should be similar size (same compression level)
    assert_eq!(
        second_len, third_len,
        "consecutive compressed entries with same pattern should have equal length"
    );

    // Verify the flags on second entry contain all SAME_* flags
    let second_flags = buf[first_len];
    assert_ne!(second_flags & 0x20, 0, "XMIT_SAME_NAME set");
    assert_ne!(second_flags & 0x02, 0, "XMIT_SAME_MODE set");
    assert_ne!(second_flags & 0x80, 0, "XMIT_SAME_TIME set");
    assert_ne!(second_flags & 0x08, 0, "XMIT_SAME_UID set");
    assert_ne!(second_flags & 0x10, 0, "XMIT_SAME_GID set");

    // Round-trip all three
    writer.write_end(&mut buf, None).unwrap();
    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);

    let r1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r1.name(), "dir/file001.dat");
    assert_eq!(r1.size(), 100);

    let r2 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r2.name(), "dir/file002.dat");
    assert_eq!(r2.size(), 200);

    let r3 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r3.name(), "dir/file003.dat");
    assert_eq!(r3.size(), 300);

    assert!(reader.read_entry(&mut cursor).unwrap().is_none());
}

/// When no metadata matches between consecutive entries, no SAME_* flags
/// are set except XMIT_SAME_UID/XMIT_SAME_GID (always set when
/// preserve_uid/gid is disabled).
#[test]
fn golden_v28_no_flag_compression() {
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let mut e1 = FileEntry::new_file("alpha.txt".into(), 100, 0o644);
    e1.set_mtime(1_700_000_000, 0);
    writer.write_entry(&mut buf, &e1).unwrap();
    let first_len = buf.len();

    // Completely different entry: different name, mode, mtime
    let mut e2 = FileEntry::new_file("beta.bin".into(), 999, 0o755);
    e2.set_mtime(1_600_000_000, 0);
    writer.write_entry(&mut buf, &e2).unwrap();

    let second_flags = buf[first_len];

    // No name compression (no shared prefix)
    assert_eq!(second_flags & 0x20, 0, "XMIT_SAME_NAME not set");

    // Different mode and mtime
    assert_eq!(second_flags & 0x02, 0, "XMIT_SAME_MODE not set");
    assert_eq!(second_flags & 0x80, 0, "XMIT_SAME_TIME not set");

    // XMIT_SAME_UID and XMIT_SAME_GID always set when preserve is disabled
    assert_ne!(second_flags & 0x08, 0, "XMIT_SAME_UID always set");
    assert_ne!(second_flags & 0x10, 0, "XMIT_SAME_GID always set");
}

// ---------------------------------------------------------------------------
// Symlink target length uses 4-byte fixed int at protocol 28
// upstream: flist.c - write_varint30_int(proto=28) = write_int (4-byte LE)
// ---------------------------------------------------------------------------

/// Symlink target length encoding uses 4-byte LE at protocol 28,
/// not the varint encoding introduced in protocol 30.
#[test]
fn golden_v28_symlink_target_length_fixed_int() {
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_links(true);

    let target = "/very/long/symlink/target/path";
    let mut entry = FileEntry::new_symlink("link".into(), target.into());
    entry.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();

    // After: flags(1) + name_len(1) + name(4) + size(4) + mtime(4) + mode(4) = 18
    // Symlink target length as 4-byte LE int
    let target_len_offset = 18;
    let target_len = target.len() as i32;
    assert_eq!(
        &buf[target_len_offset..target_len_offset + 4],
        &target_len.to_le_bytes(),
        "symlink target length as 4-byte LE int at proto 28"
    );

    // Target bytes follow
    assert_eq!(
        &buf[target_len_offset + 4..target_len_offset + 4 + target.len()],
        target.as_bytes(),
        "symlink target bytes"
    );
}

// ---------------------------------------------------------------------------
// Full flist with sorted entries round-trip
// upstream: flist.c - sender sorts then sends, receiver receives then sorts
// ---------------------------------------------------------------------------

/// Builds a realistic file list, sorts it with protocol 28 rules,
/// encodes it, decodes it, and verifies the sort order is preserved.
#[test]
fn golden_v28_sorted_flist_roundtrip() {
    let protocol = proto28();

    let mut entries = vec![
        FileEntry::new_directory("src".into(), 0o755),
        FileEntry::new_file("README.md".into(), 500, 0o644),
        FileEntry::new_file("src/main.rs".into(), 2000, 0o644),
        FileEntry::new_directory("docs".into(), 0o755),
        FileEntry::new_file("docs/guide.md".into(), 1000, 0o644),
        FileEntry::new_file("Cargo.toml".into(), 300, 0o644),
    ];

    // Set mtimes
    for (i, e) in entries.iter_mut().enumerate() {
        e.set_mtime(1_700_000_000 + i as i64, 0);
    }

    // Sort with protocol 28 rules (plain lexicographic)
    sort_file_list(&mut entries, false, true);

    // Verify sort order is plain lexicographic
    let names: Vec<&str> = entries.iter().map(|e| e.name()).collect();
    assert_eq!(
        names,
        vec![
            "Cargo.toml",
            "README.md",
            "docs",
            "docs/guide.md",
            "src",
            "src/main.rs"
        ],
        "proto 28 sort: plain lexicographic byte order"
    );

    // Encode the sorted list
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);
    for entry in &entries {
        writer.write_entry(&mut buf, entry).unwrap();
    }
    writer.write_end(&mut buf, None).unwrap();

    // Decode and verify order is preserved
    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);

    for expected_name in &names {
        let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
        assert_eq!(
            read_entry.name(),
            *expected_name,
            "decoded entry name must match sorted order"
        );
    }
    assert!(reader.read_entry(&mut cursor).unwrap().is_none());
}
