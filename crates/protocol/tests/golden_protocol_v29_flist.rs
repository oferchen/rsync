//! Golden byte tests for protocol version 29 file list wire format.
//!
//! Protocol 29 is a transitional version that uses fixed-width encoding
//! (not varint) for all integer fields. It shares the same encoding as
//! protocol 28 but introduces sender/receiver modifiers and flist timing.
//!
//! Key wire format characteristics:
//! - Fixed 4-byte LE integers for file sizes (via `write_longint`)
//! - Fixed 4-byte LE unsigned integers for mtimes (via `write_uint`)
//! - Fixed 4-byte LE integers for UIDs and GIDs
//! - 1-2 byte flags with `XMIT_EXTENDED_FLAGS` for the second byte
//! - No varint encoding (introduced in v30)
//! - No incremental recursion (introduced in v30)
//! - Symlink target length uses 4-byte fixed int
//! - End-of-list marker is single `0x00` byte (no safe file list)
//!
//! # Upstream Reference
//!
//! Wire encoding: `flist.c:send_file_entry()` and `flist.c:recv_file_entry()`
//! in rsync 3.4.1 source. Protocol 29 takes the `protocol_version < 30`
//! code paths throughout.

use std::io::Cursor;
use std::path::PathBuf;

use protocol::ProtocolVersion;
use protocol::flist::{FileEntry, FileListReader, FileListWriter};

fn v29() -> ProtocolVersion {
    ProtocolVersion::from_supported(29).expect("v29 must be supported")
}

// ---------------------------------------------------------------------------
// Regular file entry encoding
// upstream: flist.c:send_file_entry() with S_ISREG mode
// ---------------------------------------------------------------------------

/// Verifies byte-level encoding of a regular file entry at protocol 29.
///
/// Wire layout for first entry (no previous state):
///   [flags: 1 byte] [name_len: 1 byte] [name bytes] [size: 4 byte LE]
///   [mtime: 4 byte LE u32] [mode: 4 byte LE]
#[test]
fn golden_v29_regular_file_entry() {
    let protocol = v29();
    let mut writer = FileListWriter::new(protocol);
    let mut buf = Vec::new();

    let mut entry = FileEntry::new_file("hello.txt".into(), 1024, 0o644);
    entry.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();

    // Flags byte: first entry has no compression state, so mode/time differ.
    // XMIT_TOP_DIR(0x01) is NOT set (regular file, not dir).
    // XMIT_SAME_MODE(0x02) is NOT set (first entry, mode differs from default 0).
    // XMIT_SAME_TIME(0x80) is NOT set (first entry, mtime differs from default 0).
    // XMIT_SAME_NAME(0x20) is NOT set (no shared prefix).
    // XMIT_LONG_NAME(0x40) is NOT set (name < 256 bytes).
    // For protocol 28-29, when xflags==0 and !is_dir, XMIT_TOP_DIR is set
    // to avoid zero flags (upstream: flist.c line 550).
    let flags = buf[0];
    assert_eq!(
        flags & 0x01,
        0x01,
        "XMIT_TOP_DIR set to avoid zero flags for non-dir"
    );

    // Name: suffix_len=9 ("hello.txt"), no same_len byte since XMIT_SAME_NAME is off.
    // Name length is single byte (not XMIT_LONG_NAME).
    assert_eq!(buf[1], 9, "name suffix length");
    assert_eq!(&buf[2..11], b"hello.txt", "name bytes");

    // File size: 1024 as 4-byte LE via write_longint (fits in 31 bits).
    assert_eq!(
        &buf[11..15],
        &1024_i32.to_le_bytes(),
        "file size 1024 as 4-byte LE"
    );

    // Mtime: 1_700_000_000 as 4-byte LE unsigned (write_uint for proto < 30).
    assert_eq!(
        &buf[15..19],
        &1_700_000_000_u32.to_le_bytes(),
        "mtime as 4-byte LE u32"
    );

    // Mode: 0o100644 (S_IFREG | 0644) as 4-byte LE.
    assert_eq!(
        &buf[19..23],
        &(0o100644_i32).to_le_bytes(),
        "mode 0o100644 as 4-byte LE"
    );

    // No UID/GID (preserve_uid/gid not set).
    // No symlink target (not a symlink).
    // No checksum (always_checksum not set).
    assert_eq!(buf.len(), 23, "total entry length for basic regular file");
}

// ---------------------------------------------------------------------------
// Directory entry encoding
// upstream: flist.c:send_file_entry() with S_ISDIR mode
// ---------------------------------------------------------------------------

/// Verifies byte-level encoding of a directory entry at protocol 29.
///
/// Directories differ from regular files:
/// - size is always 0
/// - XMIT_TOP_DIR may be set via entry flags
/// - No symlink target
#[test]
fn golden_v29_directory_entry() {
    let protocol = v29();
    let mut writer = FileListWriter::new(protocol);
    let mut buf = Vec::new();

    let mut entry = FileEntry::new_directory("mydir".into(), 0o755);
    entry.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();

    // Flags: for first directory without top_dir flag, xflags == 0.
    // Protocol 28-29: xflags==0 for directories is valid (the zero-avoidance
    // only applies to non-directories). But actually, with no matching previous
    // state, the flags should be 0. For directories, 0 flags are allowed,
    // so we get XMIT_EXTENDED_FLAGS set to encode the zero.
    // Actually for proto 28-29: if (xflags & 0xFF00) != 0 OR xflags == 0,
    // then XMIT_EXTENDED_FLAGS is set and 2-byte encoding is used.
    let flags_lo = buf[0];
    let flags_hi = buf[1];
    assert_eq!(
        flags_lo & 0x04,
        0x04,
        "XMIT_EXTENDED_FLAGS set for zero-flags dir"
    );
    assert_eq!(flags_hi, 0x00, "extended flags byte is zero");

    // Name: "mydir" = 5 bytes
    assert_eq!(buf[2], 5, "name suffix length");
    assert_eq!(&buf[3..8], b"mydir", "name bytes");

    // Size: 0 as 4-byte LE
    assert_eq!(&buf[8..12], &0_i32.to_le_bytes(), "dir size is 0");

    // Mtime: 1_700_000_000
    assert_eq!(&buf[12..16], &1_700_000_000_u32.to_le_bytes(), "dir mtime");

    // Mode: 0o040755 (S_IFDIR | 0755) as 4-byte LE
    assert_eq!(
        &buf[16..20],
        &(0o040755_i32).to_le_bytes(),
        "dir mode 0o040755"
    );

    assert_eq!(buf.len(), 20, "total directory entry length");
}

// ---------------------------------------------------------------------------
// Symlink entry encoding
// upstream: flist.c:send_file_entry() with S_ISLNK mode
// ---------------------------------------------------------------------------

/// Verifies byte-level encoding of a symlink entry at protocol 29.
///
/// Symlinks add a target path after the metadata. The target length
/// uses `write_varint30_int` which for protocol < 30 is `write_int`
/// (4-byte fixed LE).
#[test]
fn golden_v29_symlink_entry() {
    let protocol = v29();
    let mut writer = FileListWriter::new(protocol).with_preserve_links(true);
    let mut buf = Vec::new();

    let mut entry = FileEntry::new_symlink("link".into(), "/target/path".into());
    entry.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();

    // Flags: XMIT_TOP_DIR set to avoid zero flags (non-dir)
    let flags = buf[0];
    assert_eq!(flags & 0x01, 0x01, "XMIT_TOP_DIR set for non-dir");

    // Name: "link" = 4 bytes
    assert_eq!(buf[1], 4, "name suffix length");
    assert_eq!(&buf[2..6], b"link", "name bytes");

    // Size: 0 for symlink
    assert_eq!(&buf[6..10], &0_i32.to_le_bytes(), "symlink size is 0");

    // Mtime
    assert_eq!(
        &buf[10..14],
        &1_700_000_000_u32.to_le_bytes(),
        "symlink mtime"
    );

    // Mode: 0o120777 (S_IFLNK | 0777) as 4-byte LE
    assert_eq!(
        &buf[14..18],
        &(0o120777_i32).to_le_bytes(),
        "symlink mode 0o120777"
    );

    // Symlink target: length as 4-byte LE, then target bytes.
    // "/target/path" = 12 bytes
    assert_eq!(
        &buf[18..22],
        &12_i32.to_le_bytes(),
        "symlink target length as 4-byte LE"
    );
    assert_eq!(&buf[22..34], b"/target/path", "symlink target bytes");

    assert_eq!(buf.len(), 34, "total symlink entry length");
}

// ---------------------------------------------------------------------------
// File list end-of-list terminator
// upstream: flist.c:recv_file_list() - zero byte terminates
// ---------------------------------------------------------------------------

/// Verifies that the end-of-list marker is a single zero byte for v29.
///
/// Protocol 29 does not support safe file list mode (introduced in v30),
/// so the terminator is always a single `0x00` byte regardless of I/O errors.
#[test]
fn golden_v29_end_of_list_marker() {
    let protocol = v29();
    let writer = FileListWriter::new(protocol);
    let mut buf = Vec::new();

    writer.write_end(&mut buf, None).unwrap();
    assert_eq!(buf, vec![0x00], "end marker is single zero byte");
}

/// Protocol 29 ignores I/O error codes in the end marker (no safe file list).
#[test]
fn golden_v29_end_of_list_ignores_io_error() {
    let protocol = v29();
    let writer = FileListWriter::new(protocol);
    let mut buf = Vec::new();

    writer.write_end(&mut buf, Some(5)).unwrap();
    assert_eq!(
        buf,
        vec![0x00],
        "end marker is zero byte even with io_error"
    );
}

// ---------------------------------------------------------------------------
// UID/GID encoding
// upstream: flist.c:send_file_entry() - write_int for proto < 30
// ---------------------------------------------------------------------------

/// Verifies that UID is encoded as a 4-byte fixed LE integer for v29.
///
/// For protocol < 30, UIDs use `write_int()` (4-byte signed LE) rather
/// than the varint encoding used in protocol 30+.
#[test]
fn golden_v29_uid_encoding() {
    let protocol = v29();
    let mut writer = FileListWriter::new(protocol).with_preserve_uid(true);
    let mut buf = Vec::new();

    let mut entry = FileEntry::new_file("f.txt".into(), 100, 0o644);
    entry.set_mtime(1_700_000_000, 0);
    entry.set_uid(1000);

    writer.write_entry(&mut buf, &entry).unwrap();

    // Layout: flags(1) + name_len(1) + name(5) + size(4) + mtime(4) + mode(4) + uid(4)
    // flags: XMIT_TOP_DIR(0x01) to avoid zero
    assert_eq!(buf[0] & 0x01, 0x01, "XMIT_TOP_DIR set");

    // UID at offset 19 (after mode at offset 15..19)
    let uid_offset = 1 + 1 + 5 + 4 + 4 + 4; // = 19
    assert_eq!(
        &buf[uid_offset..uid_offset + 4],
        &1000_i32.to_le_bytes(),
        "UID 1000 as 4-byte LE"
    );
}

/// Verifies that GID is encoded as a 4-byte fixed LE integer for v29.
#[test]
fn golden_v29_gid_encoding() {
    let protocol = v29();
    let mut writer = FileListWriter::new(protocol).with_preserve_gid(true);
    let mut buf = Vec::new();

    let mut entry = FileEntry::new_file("f.txt".into(), 100, 0o644);
    entry.set_mtime(1_700_000_000, 0);
    entry.set_gid(500);

    writer.write_entry(&mut buf, &entry).unwrap();

    // Layout: flags(1) + name_len(1) + name(5) + size(4) + mtime(4) + mode(4) + gid(4)
    let gid_offset = 1 + 1 + 5 + 4 + 4 + 4; // = 19
    assert_eq!(
        &buf[gid_offset..gid_offset + 4],
        &500_i32.to_le_bytes(),
        "GID 500 as 4-byte LE"
    );
}

/// Verifies that UID and GID together are both 4-byte fixed LE.
#[test]
fn golden_v29_uid_and_gid_encoding() {
    let protocol = v29();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true);
    let mut buf = Vec::new();

    let mut entry = FileEntry::new_file("f.txt".into(), 100, 0o644);
    entry.set_mtime(1_700_000_000, 0);
    entry.set_uid(1000);
    entry.set_gid(500);

    writer.write_entry(&mut buf, &entry).unwrap();

    // Layout: flags(1) + name_len(1) + name(5) + size(4) + mtime(4) + mode(4) + uid(4) + gid(4)
    let uid_offset = 1 + 1 + 5 + 4 + 4 + 4; // = 19
    let gid_offset = uid_offset + 4; // = 23

    assert_eq!(
        &buf[uid_offset..uid_offset + 4],
        &1000_i32.to_le_bytes(),
        "UID 1000"
    );
    assert_eq!(
        &buf[gid_offset..gid_offset + 4],
        &500_i32.to_le_bytes(),
        "GID 500"
    );
    assert_eq!(buf.len(), 27, "total length with uid+gid");
}

// ---------------------------------------------------------------------------
// Mtime encoding
// upstream: flist.c:send_file_entry() uses write_uint for proto < 30
// ---------------------------------------------------------------------------

/// Verifies mtime is encoded as unsigned 4-byte LE for v29.
///
/// The mtime is written with `write_uint()` for protocol < 30, which
/// treats the value as an unsigned 32-bit integer.
#[test]
fn golden_v29_mtime_encoding() {
    let protocol = v29();
    let mut writer = FileListWriter::new(protocol);
    let mut buf = Vec::new();

    let mut entry = FileEntry::new_file("x".into(), 0, 0o644);
    entry.set_mtime(0x6565_4321, 0);

    writer.write_entry(&mut buf, &entry).unwrap();

    // Layout: flags(1) + name_len(1) + name(1) + size(4) + mtime(4)
    let mtime_offset = 1 + 1 + 1 + 4; // = 7
    assert_eq!(
        &buf[mtime_offset..mtime_offset + 4],
        &0x6565_4321_u32.to_le_bytes(),
        "mtime 0x65654321 as 4-byte LE u32"
    );
}

/// Verifies XMIT_SAME_TIME flag suppresses mtime when it matches previous.
#[test]
fn golden_v29_mtime_same_time_flag() {
    let protocol = v29();
    let mut writer = FileListWriter::new(protocol);
    let mut buf = Vec::new();

    let mut entry1 = FileEntry::new_file("a.txt".into(), 100, 0o644);
    entry1.set_mtime(1_700_000_000, 0);
    writer.write_entry(&mut buf, &entry1).unwrap();
    let len_first = buf.len();

    let mut entry2 = FileEntry::new_file("b.txt".into(), 200, 0o644);
    entry2.set_mtime(1_700_000_000, 0); // Same mtime
    writer.write_entry(&mut buf, &entry2).unwrap();

    // Second entry should have XMIT_SAME_TIME (0x80) set and be shorter
    // (no mtime bytes).
    let second_flags = buf[len_first];
    assert_ne!(second_flags & 0x80, 0, "XMIT_SAME_TIME set for same mtime");

    let second_len = buf.len() - len_first;
    // Second entry: flags + name_len + name(5) + size(4) + mode(4) = shorter
    // because mtime is omitted and mode may be same too.
    assert!(
        second_len < len_first,
        "second entry shorter due to same_time"
    );
}

// ---------------------------------------------------------------------------
// Multi-entry round-trip with name compression
// upstream: flist.c name prefix compression via l1/l2 fields
// ---------------------------------------------------------------------------

/// Verifies that multiple entries with shared name prefixes compress correctly
/// and round-trip through write then read.
///
/// Name compression: when consecutive entries share a common prefix,
/// XMIT_SAME_NAME is set and a `same_len` byte precedes the suffix length.
#[test]
fn golden_v29_multi_entry_name_compression() {
    let protocol = v29();
    let mut writer = FileListWriter::new(protocol);
    let mut buf = Vec::new();

    let mut e1 = FileEntry::new_file("dir/file1.txt".into(), 100, 0o644);
    e1.set_mtime(1_700_000_000, 0);

    let mut e2 = FileEntry::new_file("dir/file2.txt".into(), 200, 0o644);
    e2.set_mtime(1_700_000_000, 0);

    let mut e3 = FileEntry::new_file("dir/file3.txt".into(), 300, 0o644);
    e3.set_mtime(1_700_000_001, 0); // Different mtime

    writer.write_entry(&mut buf, &e1).unwrap();
    let len1 = buf.len();

    writer.write_entry(&mut buf, &e2).unwrap();
    let len2 = buf.len() - len1;

    writer.write_entry(&mut buf, &e3).unwrap();

    writer.write_end(&mut buf, None).unwrap();

    // First entry: full name, no compression
    assert_eq!(buf[0] & 0x20, 0, "first entry: XMIT_SAME_NAME not set");

    // Second entry: shares "dir/file" (8 bytes) with first
    let e2_flags = buf[len1];
    assert_ne!(
        e2_flags & 0x20,
        0,
        "second entry: XMIT_SAME_NAME set for shared prefix"
    );

    // Second entry is smaller than first due to compression
    assert!(
        len2 < len1,
        "compressed entry ({len2}) shorter than first ({len1})"
    );

    // Round-trip: read all entries back
    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);

    let r1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r1.name(), "dir/file1.txt");
    assert_eq!(r1.size(), 100);
    assert_eq!(r1.mtime(), 1_700_000_000);

    let r2 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r2.name(), "dir/file2.txt");
    assert_eq!(r2.size(), 200);
    assert_eq!(r2.mtime(), 1_700_000_000);

    let r3 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r3.name(), "dir/file3.txt");
    assert_eq!(r3.size(), 300);
    assert_eq!(r3.mtime(), 1_700_000_001);

    // End of list
    let end = reader.read_entry(&mut cursor).unwrap();
    assert!(end.is_none(), "end-of-list returns None");
}

/// Verifies the exact byte layout of the second compressed entry.
///
/// When "dir/file1.txt" is followed by "dir/file2.txt", the shared prefix
/// is "dir/file" (8 bytes), and the suffix is "2.txt" (5 bytes).
#[test]
fn golden_v29_name_compression_byte_layout() {
    let protocol = v29();
    let mut writer = FileListWriter::new(protocol);
    let mut buf = Vec::new();

    let mut e1 = FileEntry::new_file("dir/file1.txt".into(), 100, 0o644);
    e1.set_mtime(1_700_000_000, 0);
    writer.write_entry(&mut buf, &e1).unwrap();
    let offset = buf.len();

    let mut e2 = FileEntry::new_file("dir/file2.txt".into(), 100, 0o644);
    e2.set_mtime(1_700_000_000, 0);
    writer.write_entry(&mut buf, &e2).unwrap();

    let e2_bytes = &buf[offset..];

    // Flags: XMIT_SAME_NAME(0x20) | XMIT_SAME_MODE(0x02) | XMIT_SAME_TIME(0x80)
    // = 0xA2
    let flags = e2_bytes[0];
    assert_ne!(flags & 0x20, 0, "XMIT_SAME_NAME set");
    assert_ne!(flags & 0x02, 0, "XMIT_SAME_MODE set");
    assert_ne!(flags & 0x80, 0, "XMIT_SAME_TIME set");

    // same_len byte: 8 (length of "dir/file")
    assert_eq!(e2_bytes[1], 8, "shared prefix length");

    // suffix_len byte: 5 (length of "2.txt")
    assert_eq!(e2_bytes[2], 5, "suffix length");

    // suffix bytes: "2.txt"
    assert_eq!(&e2_bytes[3..8], b"2.txt", "suffix bytes");

    // File size: 100 as 4-byte LE
    assert_eq!(
        &e2_bytes[8..12],
        &100_i32.to_le_bytes(),
        "file size 100 as 4-byte LE"
    );

    // No mtime (XMIT_SAME_TIME), no mode (XMIT_SAME_MODE)
    assert_eq!(
        e2_bytes.len(),
        12,
        "compressed entry: flags(1) + same_len(1) + suffix_len(1) + suffix(5) + size(4)"
    );
}

// ---------------------------------------------------------------------------
// Round-trip: directory entries
// ---------------------------------------------------------------------------

/// Verifies directory entry round-trip through write and read for v29.
#[test]
fn golden_v29_directory_round_trip() {
    let protocol = v29();
    let mut writer = FileListWriter::new(protocol);
    let mut buf = Vec::new();

    let mut entry = FileEntry::new_directory("subdir".into(), 0o755);
    entry.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "subdir");
    assert!(read_entry.is_dir(), "entry should be a directory");
    assert_eq!(read_entry.mtime(), 1_700_000_000);
    assert_eq!(read_entry.mode() & 0o7777, 0o755, "dir permissions");
}

// ---------------------------------------------------------------------------
// Round-trip: symlink entries
// ---------------------------------------------------------------------------

/// Verifies symlink entry round-trip for v29 with preserve_links enabled.
#[test]
fn golden_v29_symlink_round_trip() {
    let protocol = v29();
    let mut writer = FileListWriter::new(protocol).with_preserve_links(true);
    let mut buf = Vec::new();

    let mut entry = FileEntry::new_symlink("mylink".into(), "/usr/bin/target".into());
    entry.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_links(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "mylink");
    assert!(read_entry.is_symlink(), "entry should be a symlink");
    assert_eq!(
        read_entry.link_target().map(|p| p.to_path_buf()),
        Some(PathBuf::from("/usr/bin/target")),
        "symlink target must round-trip"
    );
}

// ---------------------------------------------------------------------------
// Round-trip: UID and GID
// ---------------------------------------------------------------------------

/// Verifies UID/GID round-trip through write and read for v29.
#[test]
fn golden_v29_uid_gid_round_trip() {
    let protocol = v29();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true);
    let mut buf = Vec::new();

    let mut entry = FileEntry::new_file("owned.txt".into(), 512, 0o644);
    entry.set_mtime(1_700_000_000, 0);
    entry.set_uid(1000);
    entry.set_gid(500);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "owned.txt");
    assert_eq!(read_entry.uid(), Some(1000), "UID must round-trip");
    assert_eq!(read_entry.gid(), Some(500), "GID must round-trip");
}

// ---------------------------------------------------------------------------
// v29 vs v28: identical wire format
// ---------------------------------------------------------------------------

/// Verifies that protocol 28 and 29 produce identical wire bytes for
/// the same file entry, confirming they share the same encoding.
#[test]
fn golden_v29_wire_format_matches_v28() {
    let v28 = ProtocolVersion::from_supported(28).expect("v28 must be supported");
    let v29 = v29();

    let mut writer_28 = FileListWriter::new(v28);
    let mut writer_29 = FileListWriter::new(v29);
    let mut buf_28 = Vec::new();
    let mut buf_29 = Vec::new();

    let mut entry = FileEntry::new_file("test.txt".into(), 4096, 0o644);
    entry.set_mtime(1_700_000_000, 0);

    writer_28.write_entry(&mut buf_28, &entry).unwrap();
    writer_29.write_entry(&mut buf_29, &entry).unwrap();

    assert_eq!(
        buf_28, buf_29,
        "v28 and v29 produce identical wire bytes for same entry"
    );
}

// ---------------------------------------------------------------------------
// v29 vs v30: different wire format
// ---------------------------------------------------------------------------

/// Verifies that protocol 29 and 30 produce different wire bytes,
/// confirming the varint encoding change at the v30 boundary.
#[test]
fn golden_v29_wire_format_differs_from_v30() {
    let v29 = v29();
    let v30 = ProtocolVersion::from_supported(30).expect("v30 must be supported");

    let mut writer_29 = FileListWriter::new(v29);
    let mut writer_30 = FileListWriter::new(v30);
    let mut buf_29 = Vec::new();
    let mut buf_30 = Vec::new();

    let mut entry = FileEntry::new_file("test.txt".into(), 4096, 0o644);
    entry.set_mtime(1_700_000_000, 0);

    writer_29.write_entry(&mut buf_29, &entry).unwrap();
    writer_30.write_entry(&mut buf_30, &entry).unwrap();

    assert_ne!(
        buf_29, buf_30,
        "v29 (fixed) and v30 (varint) must produce different wire bytes"
    );

    // v29 uses fixed 4-byte encoding, v30 uses more compact varint
    assert!(
        buf_29.len() >= buf_30.len(),
        "v29 fixed encoding ({}) should be at least as long as v30 varint ({})",
        buf_29.len(),
        buf_30.len()
    );
}

// ---------------------------------------------------------------------------
// Large file size encoding
// upstream: io.c:write_longint() - marker 0xFFFFFFFF + 8-byte value
// ---------------------------------------------------------------------------

/// Verifies that file sizes exceeding 31 bits use the longint marker
/// encoding in protocol 29.
#[test]
fn golden_v29_large_file_size() {
    let protocol = v29();
    let mut writer = FileListWriter::new(protocol);
    let mut buf = Vec::new();

    let large_size: u64 = 5_000_000_000; // > 2^32
    let mut entry = FileEntry::new_file("big.iso".into(), large_size, 0o644);
    entry.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();

    // After flags(1) + name_len(1) + name(7) = offset 9
    let size_offset = 1 + 1 + 7; // = 9

    // Longint marker: 0xFFFFFFFF
    assert_eq!(
        &buf[size_offset..size_offset + 4],
        &0xFFFF_FFFFu32.to_le_bytes(),
        "longint marker for large file size"
    );

    // Followed by 8-byte LE value
    assert_eq!(
        &buf[size_offset + 4..size_offset + 12],
        &(large_size as i64).to_le_bytes(),
        "8-byte LE file size after marker"
    );
}

// ---------------------------------------------------------------------------
// Mixed file types in a single file list
// ---------------------------------------------------------------------------

/// Verifies round-trip of a mixed file list containing files, directories,
/// and symlinks at protocol 29.
#[test]
fn golden_v29_mixed_file_list_round_trip() {
    let protocol = v29();
    let mut writer = FileListWriter::new(protocol).with_preserve_links(true);
    let mut buf = Vec::new();

    let mut dir = FileEntry::new_directory("project".into(), 0o755);
    dir.set_mtime(1_700_000_000, 0);

    let mut file = FileEntry::new_file("project/main.rs".into(), 2048, 0o644);
    file.set_mtime(1_700_000_001, 0);

    let mut link = FileEntry::new_symlink("project/latest".into(), "main.rs".into());
    link.set_mtime(1_700_000_002, 0);

    writer.write_entry(&mut buf, &dir).unwrap();
    writer.write_entry(&mut buf, &file).unwrap();
    writer.write_entry(&mut buf, &link).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_links(true);

    let r_dir = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert!(r_dir.is_dir());
    assert_eq!(r_dir.name(), "project");

    let r_file = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert!(r_file.is_file());
    assert_eq!(r_file.name(), "project/main.rs");
    assert_eq!(r_file.size(), 2048);

    let r_link = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert!(r_link.is_symlink());
    assert_eq!(r_link.name(), "project/latest");
    assert_eq!(
        r_link.link_target().map(|p| p.to_path_buf()),
        Some(PathBuf::from("main.rs"))
    );

    assert!(reader.read_entry(&mut cursor).unwrap().is_none());
}
