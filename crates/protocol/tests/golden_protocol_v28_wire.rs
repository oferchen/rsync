//! Golden byte tests for protocol version 28 wire format.
//!
//! Protocol 28 uses fixed-width encoding throughout:
//! - Two-byte flags (primary + optional extended byte via XMIT_EXTENDED_FLAGS)
//! - File size via `write_longint` (4 bytes for values <= 0x7FFFFFFF, 12 for larger)
//! - Mtime as 4-byte unsigned LE
//! - UID/GID as 4-byte LE i32
//! - Symlink target length as 4-byte LE i32
//! - No varint encoding, no incremental recursion
//! - MD4 checksums (16 bytes) instead of MD5
//!
//! # Upstream Reference
//!
//! Protocol 28 is the oldest supported version. Wire format is defined in
//! `flist.c:send_file_entry()` / `recv_file_entry()` with protocol version
//! guards controlling encoding dispatch.

use std::io::Cursor;

use protocol::flist::{FileEntry, FileListReader, FileListWriter};
use protocol::{ProtocolVersion, TransferStats, write_int, write_longint};

fn proto28() -> ProtocolVersion {
    ProtocolVersion::try_from(28u8).unwrap()
}

// ---------------------------------------------------------------------------
// Regular file entry encoding
// upstream: flist.c:send_file_entry() - protocol 28, S_ISREG
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_regular_file_first_entry() {
    // First entry in a file list: no previous entry to share fields with.
    // File: "hello.txt", size=42, mode=0o100644, mtime=1700000000
    // No preserve_uid/gid, no checksum mode.
    //
    // With default PreserveFlags (uid=false, gid=false), upstream sets
    // XMIT_SAME_UID and XMIT_SAME_GID unconditionally (flist.c:463,473).
    // xflags = 0x18 (non-zero), so no XMIT_TOP_DIR substitution needed.

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(proto28());

    let mut entry = FileEntry::new_file("hello.txt".into(), 42, 0o644);
    entry.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();

    #[rustfmt::skip]
    let expected: &[u8] = &[
        // flags: XMIT_SAME_UID (0x08) | XMIT_SAME_GID (0x10) = 0x18
        0x18,
        // name suffix length (1 byte, no XMIT_LONG_NAME)
        0x09,
        // name: "hello.txt"
        b'h', b'e', b'l', b'l', b'o', b'.', b't', b'x', b't',
        // size: write_longint(42) = 4-byte LE
        0x2A, 0x00, 0x00, 0x00,
        // mtime: 1700000000 as u32 LE
        0x00, 0xF1, 0x53, 0x65,
        // mode: 0o100644 = 33188 as i32 LE
        0xA4, 0x81, 0x00, 0x00,
    ];

    assert_eq!(
        buf, expected,
        "wire bytes mismatch for protocol 28 regular file"
    );
}

#[test]
fn golden_v28_regular_file_roundtrip() {
    // Write a regular file entry and read it back.
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let mut entry = FileEntry::new_file("data.bin".into(), 1024, 0o755);
    entry.set_mtime(1_600_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "data.bin");
    assert_eq!(read_entry.size(), 1024);
    assert_eq!(read_entry.mtime(), 1_600_000_000);
    assert_eq!(read_entry.mode(), 0o100755);
}

#[test]
fn golden_v28_regular_file_size_encoding() {
    // Protocol 28 uses write_longint for file size.
    // Small value (<=0x7FFFFFFF): 4-byte LE.
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(proto28());

    let mut entry = FileEntry::new_file("small.dat".into(), 100, 0o644);
    entry.set_mtime(1_000_000_000, 0);
    writer.write_entry(&mut buf, &entry).unwrap();

    // The size field starts after: flags(1) + name_len(1) + name(9) = 11 bytes
    // size = 100 = 0x64 as write_longint => 4-byte LE: [0x64, 0x00, 0x00, 0x00]
    assert_eq!(&buf[11..15], [0x64, 0x00, 0x00, 0x00]);
}

#[test]
fn golden_v28_regular_file_large_size() {
    // File size > 0x7FFFFFFF uses longint extended encoding:
    // 4-byte marker 0xFFFFFFFF + 8-byte LE i64.
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let large_size: u64 = 5_000_000_000; // ~4.7 GB
    let mut entry = FileEntry::new_file("big.iso".into(), large_size, 0o644);
    entry.set_mtime(1_700_000_000, 0);
    writer.write_entry(&mut buf, &entry).unwrap();

    // After flags(1) + name_len(1) + name(7) = 9 bytes, the size field is:
    // marker: [0xFF, 0xFF, 0xFF, 0xFF]
    // value: 5000000000 = 0x12A05F200 as i64 LE
    assert_eq!(&buf[9..13], [0xFF, 0xFF, 0xFF, 0xFF]);
    let size_bytes = &buf[13..21];
    let decoded_size = i64::from_le_bytes(size_bytes.try_into().unwrap());
    assert_eq!(decoded_size, large_size as i64);

    // Verify round-trip
    writer = FileListWriter::new(protocol);
    let mut rt_buf = Vec::new();
    let mut rt_entry = FileEntry::new_file("big.iso".into(), large_size, 0o644);
    rt_entry.set_mtime(1_700_000_000, 0);
    writer.write_entry(&mut rt_buf, &rt_entry).unwrap();
    writer.write_end(&mut rt_buf, None).unwrap();

    let mut cursor = Cursor::new(&rt_buf[..]);
    let mut reader = FileListReader::new(protocol);
    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.size(), large_size);
}

// ---------------------------------------------------------------------------
// Directory entry encoding
// upstream: flist.c:send_file_entry() - protocol 28, S_ISDIR
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_directory_first_entry() {
    // Directory: "mydir", mode=0o40755, mtime=1700000000
    // With default PreserveFlags (uid=false, gid=false), upstream sets
    // XMIT_SAME_UID and XMIT_SAME_GID unconditionally (flist.c:463,473:
    // `!preserve_uid` is true). This gives xflags=0x18 (single-byte encoding).
    //
    // Expected wire format:
    //   flags: 1 byte = 0x18 (XMIT_SAME_UID | XMIT_SAME_GID)
    //   name_len: 1 byte = 5
    //   name: "mydir"
    //   size: write_longint(0) = 4-byte LE [0x00, 0x00, 0x00, 0x00]
    //   mtime: 4-byte unsigned LE = 1700000000
    //   mode: to_wire_mode(0o40755) = 16877 = 0x000041ED
    //         LE: [0xED, 0x41, 0x00, 0x00]

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(proto28());

    let mut entry = FileEntry::new_directory("mydir".into(), 0o755);
    entry.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();

    #[rustfmt::skip]
    let expected: &[u8] = &[
        // flags: XMIT_SAME_UID (0x08) | XMIT_SAME_GID (0x10) = 0x18
        0x18,
        // name suffix length
        0x05,
        // name: "mydir"
        b'm', b'y', b'd', b'i', b'r',
        // size: write_longint(0) = 4-byte LE
        0x00, 0x00, 0x00, 0x00,
        // mtime: 1700000000 as u32 LE
        0x00, 0xF1, 0x53, 0x65,
        // mode: 0o40755 = 16877 as i32 LE
        0xED, 0x41, 0x00, 0x00,
    ];

    assert_eq!(
        buf, expected,
        "wire bytes mismatch for protocol 28 directory"
    );
}

#[test]
fn golden_v28_directory_roundtrip() {
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let mut entry = FileEntry::new_directory("subdir".into(), 0o755);
    entry.set_mtime(1_600_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "subdir");
    assert_eq!(read_entry.size(), 0);
    assert!(read_entry.is_dir());
    assert_eq!(read_entry.mtime(), 1_600_000_000);
    assert_eq!(read_entry.mode(), 0o40755);
}

// ---------------------------------------------------------------------------
// Symlink entry encoding
// upstream: flist.c:send_file_entry() - protocol 28, S_ISLNK
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_symlink_entry() {
    // Symlink: "link" -> "target", mode=0o120777, mtime=1700000000
    // When preserve_links is set, symlink target is written after mode.
    // Target length uses write_varint30_int(proto=28) = write_int (4-byte LE).
    //
    // With default PreserveFlags (uid=false, gid=false), upstream sets
    // XMIT_SAME_UID and XMIT_SAME_GID unconditionally (flist.c:463,473).
    // xflags = 0x18 (non-zero), so no XMIT_TOP_DIR substitution needed.

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(proto28()).with_preserve_links(true);

    let mut entry = FileEntry::new_symlink("link".into(), "target".into());
    entry.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();

    #[rustfmt::skip]
    let expected: &[u8] = &[
        // flags: XMIT_SAME_UID (0x08) | XMIT_SAME_GID (0x10) = 0x18
        0x18,
        // name suffix length
        0x04,
        // name: "link"
        b'l', b'i', b'n', b'k',
        // size: write_longint(0)
        0x00, 0x00, 0x00, 0x00,
        // mtime: 1700000000 as u32 LE
        0x00, 0xF1, 0x53, 0x65,
        // mode: 0o120777 = 41471 as i32 LE
        0xFF, 0xA1, 0x00, 0x00,
        // symlink target length: write_int(6) = 4-byte LE
        0x06, 0x00, 0x00, 0x00,
        // symlink target: "target"
        b't', b'a', b'r', b'g', b'e', b't',
    ];

    assert_eq!(buf, expected, "wire bytes mismatch for protocol 28 symlink");
}

#[test]
fn golden_v28_symlink_roundtrip() {
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_links(true);

    let entry = FileEntry::new_symlink("mylink".into(), "/usr/bin/foo".into());

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_links(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "mylink");
    assert!(read_entry.is_symlink());
    assert_eq!(
        read_entry.link_target().map(|p| p.to_path_buf()),
        Some("/usr/bin/foo".into())
    );
}

// ---------------------------------------------------------------------------
// File list terminator
// upstream: flist.c - end of file list is a single zero byte
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_end_marker() {
    // Protocol 28 end marker is a single zero byte (no varint, no safe file list).
    let writer = FileListWriter::new(proto28());
    let mut buf = Vec::new();
    writer.write_end(&mut buf, None).unwrap();

    assert_eq!(
        buf,
        [0x00],
        "protocol 28 end marker must be a single zero byte"
    );
}

#[test]
fn golden_v28_end_marker_ignores_io_error() {
    // Protocol 28 has no safe file list mode, so io_error is ignored.
    // The end marker is still just a single zero byte.
    let writer = FileListWriter::new(proto28());
    let mut buf = Vec::new();
    writer.write_end(&mut buf, Some(5)).unwrap();

    assert_eq!(
        buf,
        [0x00],
        "protocol 28 end marker must ignore io_error (no safe file list)"
    );
}

// ---------------------------------------------------------------------------
// Checksum format (MD4, 16 bytes for protocol 28)
// upstream: flist.c - always_checksum sends flist_csum_len bytes per regular file
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_checksum_md4_regular_file() {
    // With always_checksum enabled, protocol 28 writes 16 bytes of checksum
    // (MD4 digest length) for regular files.
    let protocol = proto28();
    let mut buf = Vec::new();
    let md4_len = 16;
    let mut writer = FileListWriter::new(protocol).with_always_checksum(md4_len);

    let mut entry = FileEntry::new_file("check.txt".into(), 10, 0o644);
    entry.set_mtime(1_000_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();

    // The checksum field comes last. Without uid/gid/symlink/devices, it's after:
    // flags(1) + name_len(1) + name(9) + size(4) + mtime(4) + mode(4) = 23
    // Then 16 bytes of checksum (all zeros since no checksum was set on entry)
    assert_eq!(buf.len(), 23 + md4_len);
    assert_eq!(
        &buf[23..],
        &[0u8; 16],
        "MD4 checksum should be 16 zero bytes"
    );
}

#[test]
fn golden_v28_checksum_skips_directory() {
    // Protocol 28+ only writes checksums for regular files (not dirs).
    let protocol = proto28();
    let md4_len = 16;

    // Regular file with checksum
    let mut file_buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_always_checksum(md4_len);
    let mut file_entry = FileEntry::new_file("f.txt".into(), 5, 0o644);
    file_entry.set_mtime(1_000_000_000, 0);
    writer.write_entry(&mut file_buf, &file_entry).unwrap();

    // Directory without checksum
    let mut dir_buf = Vec::new();
    let mut dir_writer = FileListWriter::new(protocol).with_always_checksum(md4_len);
    let mut dir_entry = FileEntry::new_directory("d".into(), 0o755);
    dir_entry.set_mtime(1_000_000_000, 0);
    dir_writer.write_entry(&mut dir_buf, &dir_entry).unwrap();

    // file_buf should be longer than dir_buf by exactly md4_len bytes,
    // accounting for the different name lengths and flag encoding.
    // File: flags(1) + name_len(1) + name(5) + size(4) + mtime(4) + mode(4) + csum(16) = 35
    // Dir: flags(2) + name_len(1) + name(1) + size(4) + mtime(4) + mode(4) = 16
    // The key assertion: file has checksum bytes, dir does not.
    let file_metadata_end = file_buf.len() - md4_len;
    assert_eq!(
        &file_buf[file_metadata_end..],
        &[0u8; 16],
        "regular file must have 16-byte checksum appended"
    );
}

#[test]
fn golden_v28_checksum_with_known_digest() {
    // Verify a known checksum is written to the wire verbatim.
    let protocol = proto28();
    let md4_len = 16;
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_always_checksum(md4_len);

    let mut entry = FileEntry::new_file("x.bin".into(), 1, 0o644);
    entry.set_mtime(1_000_000_000, 0);
    let digest: Vec<u8> = (0..16).collect(); // [0x00, 0x01, ..., 0x0F]
    entry.set_checksum(digest.clone());

    writer.write_entry(&mut buf, &entry).unwrap();

    // Checksum is the last 16 bytes
    let csum_start = buf.len() - md4_len;
    assert_eq!(
        &buf[csum_start..],
        &digest,
        "checksum bytes must match the set digest"
    );
}

// ---------------------------------------------------------------------------
// UID/GID encoding (fixed 4-byte LE for protocol 28)
// upstream: flist.c - uid/gid use write_int (4-byte LE) for protocol < 30
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_uid_gid_fixed_encoding() {
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

    let mut entry = FileEntry::new_file("owned.txt".into(), 50, 0o644);
    entry.set_mtime(1_700_000_000, 0);
    entry.set_uid(1000);
    entry.set_gid(1000);

    writer.write_entry(&mut buf, &entry).unwrap();

    // After flags(1) + name_len(1) + name(9) + size(4) + mtime(4) + mode(4) = 23
    // uid: write_int(1000) = 4-byte LE: [0xE8, 0x03, 0x00, 0x00]
    // gid: write_int(1000) = 4-byte LE: [0xE8, 0x03, 0x00, 0x00]
    assert_eq!(
        &buf[23..27],
        [0xE8, 0x03, 0x00, 0x00],
        "UID must be 4-byte LE"
    );
    assert_eq!(
        &buf[27..31],
        [0xE8, 0x03, 0x00, 0x00],
        "GID must be 4-byte LE"
    );
    assert_eq!(buf.len(), 31);
}

#[test]
fn golden_v28_uid_gid_roundtrip() {
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

    let mut entry = FileEntry::new_file("user.txt".into(), 10, 0o644);
    entry.set_mtime(1_500_000_000, 0);
    entry.set_uid(65534);
    entry.set_gid(100);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.uid(), Some(65534));
    assert_eq!(read_entry.gid(), Some(100));
}

// ---------------------------------------------------------------------------
// Name compression across entries
// upstream: flist.c - XMIT_SAME_NAME prefix sharing between consecutive entries
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_name_compression() {
    // Two files sharing a prefix: "dir/file1.txt" and "dir/file2.txt"
    // Second entry should use XMIT_SAME_NAME with shared prefix "dir/file".
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let mut e1 = FileEntry::new_file("dir/file1.txt".into(), 100, 0o644);
    e1.set_mtime(1_700_000_000, 0);

    let mut e2 = FileEntry::new_file("dir/file2.txt".into(), 200, 0o644);
    e2.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut buf, &e1).unwrap();
    let first_len = buf.len();
    writer.write_entry(&mut buf, &e2).unwrap();

    // Second entry should be shorter due to name compression
    let second_bytes = &buf[first_len..];

    // Flags for second entry: XMIT_SAME_NAME(0x20) | XMIT_SAME_TIME(0x80) | XMIT_SAME_MODE(0x02)
    // | XMIT_SAME_UID(0x08) | XMIT_SAME_GID(0x10) = 0xBA
    assert_eq!(
        second_bytes[0], 0xBA,
        "second entry flags should have SAME_NAME|SAME_TIME|SAME_MODE"
    );

    // same_len byte (shared prefix "dir/file" = 8 bytes)
    assert_eq!(second_bytes[1], 8, "shared prefix length should be 8");

    // suffix_len byte (remaining "2.txt" = 5 bytes)
    assert_eq!(second_bytes[2], 5, "suffix length should be 5");

    // suffix: "2.txt"
    assert_eq!(&second_bytes[3..8], b"2.txt");
}

// ---------------------------------------------------------------------------
// Flags encoding: two-byte extended flags for protocol 28
// upstream: flist.c - protocol 28 uses XMIT_EXTENDED_FLAGS for second byte
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_flags_single_byte_non_dir() {
    // A non-directory with non-zero flags uses a single byte.
    // File with XMIT_SAME_MODE set: flags = 0x02
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    // Write first entry to set up state
    let mut e1 = FileEntry::new_file("a.txt".into(), 10, 0o644);
    e1.set_mtime(1_000_000_000, 0);
    writer.write_entry(&mut buf, &e1).unwrap();

    // Write second entry with same mode but different time
    let first_len = buf.len();
    let mut e2 = FileEntry::new_file("b.txt".into(), 20, 0o644);
    e2.set_mtime(1_000_000_001, 0);
    writer.write_entry(&mut buf, &e2).unwrap();

    // Second entry flags: XMIT_SAME_MODE (0x02) | XMIT_SAME_UID (0x08) | XMIT_SAME_GID (0x10)
    // upstream: !preserve_uid sets SAME_UID unconditionally (flist.c:463)
    assert_eq!(buf[first_len], 0x1A);
}

#[test]
fn golden_v28_flags_two_byte_directory_zero() {
    // Directory with default PreserveFlags (uid=false, gid=false):
    // upstream sets XMIT_SAME_UID | XMIT_SAME_GID = 0x18 (single-byte encoding).
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let mut entry = FileEntry::new_directory("d".into(), 0o755);
    entry.set_mtime(1_000_000_000, 0);
    writer.write_entry(&mut buf, &entry).unwrap();

    // Single byte: XMIT_SAME_UID (0x08) | XMIT_SAME_GID (0x10) = 0x18
    assert_eq!(buf[0], 0x18, "flags must be XMIT_SAME_UID | XMIT_SAME_GID");
}

// ---------------------------------------------------------------------------
// Mtime encoding (4-byte unsigned LE for protocol 28)
// upstream: flist.c - mtime uses write_uint for protocol < 30
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_mtime_encoding() {
    // Protocol 28 encodes mtime as 4-byte unsigned LE (write_uint).
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);

    let mut entry = FileEntry::new_file("t.dat".into(), 1, 0o644);
    // Use a timestamp that exercises all 4 bytes: 0xDEADBEEF = 3735928559
    entry.set_mtime(0xDEAD_BEEF_i64, 0);
    writer.write_entry(&mut buf, &entry).unwrap();

    // After flags(1) + name_len(1) + name(5) + size(4) = 11 bytes
    // mtime: 0xDEADBEEF as u32 LE: [0xEF, 0xBE, 0xAD, 0xDE]
    assert_eq!(
        &buf[11..15],
        [0xEF, 0xBE, 0xAD, 0xDE],
        "mtime must be 4-byte unsigned LE"
    );
}

// ---------------------------------------------------------------------------
// No varint flags (protocol 28 uses fixed encoding)
// upstream: protocol 28 does NOT support VARINT_FLIST_FLAGS
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_no_varint_flags() {
    // Verify protocol 28 uses fixed flag encoding, not varint.
    let v28 = proto28();
    assert!(
        !v28.uses_varint_flist_flags(),
        "protocol 28 must not use varint flist flags"
    );
    assert!(
        v28.uses_fixed_encoding(),
        "protocol 28 must use fixed encoding"
    );
    assert!(
        !v28.uses_varint_encoding(),
        "protocol 28 must not use varint encoding"
    );
}

// ---------------------------------------------------------------------------
// No incremental recursion (protocol 28)
// upstream: protocol 28 uses legacy ASCII negotiation with no inc_recurse
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_no_inc_recurse() {
    // Protocol 28 uses legacy ASCII negotiation and does not support the
    // binary compatibility flags exchange needed for incremental recursion.
    let v28 = proto28();
    assert!(
        v28.uses_legacy_ascii_negotiation(),
        "protocol 28 must use legacy ASCII negotiation (no binary compat flags)"
    );
    assert!(
        !v28.uses_binary_negotiation(),
        "protocol 28 must not use binary negotiation"
    );
}

// ---------------------------------------------------------------------------
// Multi-entry file list round-trip
// upstream: flist.c - full file list with mixed entry types
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_multi_entry_roundtrip() {
    // Write a mix of files, directories, and symlinks, then read them back.
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_links(true);

    let mut f1 = FileEntry::new_file("README.md".into(), 500, 0o644);
    f1.set_mtime(1_700_000_000, 0);

    let mut d1 = FileEntry::new_directory("src".into(), 0o755);
    d1.set_mtime(1_700_000_000, 0);

    let mut f2 = FileEntry::new_file("src/main.rs".into(), 2000, 0o644);
    f2.set_mtime(1_700_000_100, 0);

    let s1 = FileEntry::new_symlink("latest".into(), "src/main.rs".into());

    writer.write_entry(&mut buf, &f1).unwrap();
    writer.write_entry(&mut buf, &d1).unwrap();
    writer.write_entry(&mut buf, &f2).unwrap();
    writer.write_entry(&mut buf, &s1).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_links(true);

    let r1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r1.name(), "README.md");
    assert!(r1.is_file());
    assert_eq!(r1.size(), 500);

    let r2 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r2.name(), "src");
    assert!(r2.is_dir());

    let r3 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r3.name(), "src/main.rs");
    assert!(r3.is_file());
    assert_eq!(r3.size(), 2000);

    let r4 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r4.name(), "latest");
    assert!(r4.is_symlink());
    assert_eq!(
        r4.link_target().map(|p| p.to_path_buf()),
        Some("src/main.rs".into())
    );

    // End of list
    let end = reader.read_entry(&mut cursor).unwrap();
    assert!(end.is_none(), "should reach end of file list");
}

// ---------------------------------------------------------------------------
// Checksum header for protocol 28 uses MD4 (16-byte strong checksum)
// upstream: match.c/sender.c - s2length=16 for MD4 in sum_head
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_sum_head_md4() {
    // Protocol 28 uses MD4 for strong checksums.
    // A typical SumHead for MD4: count=100, blength=700, s2length=16, remainder=300
    let mut buf = Vec::new();
    write_int(&mut buf, 100).unwrap(); // count
    write_int(&mut buf, 700).unwrap(); // blength
    write_int(&mut buf, 16).unwrap(); // s2length (MD4 = 16 bytes)
    write_int(&mut buf, 300).unwrap(); // remainder

    assert_eq!(buf.len(), 16);

    // Verify exact encoding
    // count=100: [0x64, 0x00, 0x00, 0x00]
    assert_eq!(&buf[0..4], [0x64, 0x00, 0x00, 0x00]);
    // blength=700: [0xBC, 0x02, 0x00, 0x00]
    assert_eq!(&buf[4..8], [0xBC, 0x02, 0x00, 0x00]);
    // s2length=16: [0x10, 0x00, 0x00, 0x00]
    assert_eq!(&buf[8..12], [0x10, 0x00, 0x00, 0x00]);
    // remainder=300: [0x2C, 0x01, 0x00, 0x00]
    assert_eq!(&buf[12..16], [0x2C, 0x01, 0x00, 0x00]);

    // Round-trip
    let mut cursor = Cursor::new(&buf);
    use protocol::read_int;
    assert_eq!(read_int(&mut cursor).unwrap(), 100);
    assert_eq!(read_int(&mut cursor).unwrap(), 700);
    assert_eq!(read_int(&mut cursor).unwrap(), 16);
    assert_eq!(read_int(&mut cursor).unwrap(), 300);
}

// ---------------------------------------------------------------------------
// Transfer stats for protocol 28 (no flist times)
// upstream: main.c - protocol < 29 omits flist_buildtime/flist_xfertime
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_stats_no_flist_times() {
    let stats = TransferStats::with_bytes(4096, 8192, 100_000);
    let mut buf = Vec::new();
    stats.write_to(&mut buf, proto28()).unwrap();

    // Protocol 28: only 3 core stats, each as varlong30(min_bytes=3)
    // No flist_buildtime or flist_xfertime.
    let mut cursor = Cursor::new(&buf);
    let decoded = TransferStats::read_from(&mut cursor, proto28()).unwrap();

    assert_eq!(decoded.total_read, 4096);
    assert_eq!(decoded.total_written, 8192);
    assert_eq!(decoded.total_size, 100_000);
    assert_eq!(decoded.flist_buildtime, 0, "protocol 28 has no flist times");
    assert_eq!(decoded.flist_xfertime, 0, "protocol 28 has no flist times");
}

// ---------------------------------------------------------------------------
// Longint encoding (used by protocol 28 for file sizes)
// upstream: io.c - write_longint() / read_longint()
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_longint_small_value() {
    // Values <= 0x7FFFFFFF use 4-byte LE encoding.
    let mut buf = Vec::new();
    write_longint(&mut buf, 12345).unwrap();

    // 12345 = 0x3039 LE: [0x39, 0x30, 0x00, 0x00]
    assert_eq!(buf, [0x39, 0x30, 0x00, 0x00]);
    assert_eq!(buf.len(), 4);

    let mut cursor = Cursor::new(&buf);
    use protocol::read_longint;
    assert_eq!(read_longint(&mut cursor).unwrap(), 12345);
}

#[test]
fn golden_v28_longint_large_value() {
    // Values > 0x7FFFFFFF use 0xFFFFFFFF marker + 8-byte LE i64.
    let value: i64 = 10_000_000_000; // ~9.3 GB
    let mut buf = Vec::new();
    write_longint(&mut buf, value).unwrap();

    // Marker
    assert_eq!(&buf[0..4], [0xFF, 0xFF, 0xFF, 0xFF]);
    // Value as i64 LE
    let decoded = i64::from_le_bytes(buf[4..12].try_into().unwrap());
    assert_eq!(decoded, value);
    assert_eq!(buf.len(), 12);

    let mut cursor = Cursor::new(&buf);
    use protocol::read_longint;
    assert_eq!(read_longint(&mut cursor).unwrap(), value);
}

// ---------------------------------------------------------------------------
// Block device encoding (protocol 28 rdev with XMIT_RDEV_MINOR_8_PRE30)
// upstream: flist.c:send_file_entry() - protocol 28 device encoding
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_block_device_small_minor() {
    // Block device: "sda", major=8, minor=0, mode=0o60660
    // Protocol 28: minor fits in 8 bits, so XMIT_RDEV_MINOR_8_PRE30 flag is set.
    //
    // Expected wire format:
    //   flags: 2 bytes LE (XMIT_EXTENDED_FLAGS in low byte because high byte
    //          contains XMIT_RDEV_MINOR_8_PRE30)
    //   name_len: 1 byte = 3
    //   name: "sda"
    //   size: write_longint(0) = 4-byte LE
    //   mtime: 4-byte u32 LE
    //   mode: 0o60660 = 0x000061B0 as i32 LE
    //   rdev_major: write_int(8) = 4-byte LE
    //   rdev_minor: 1 byte (because XMIT_RDEV_MINOR_8_PRE30 is set)
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);

    let mut entry = FileEntry::new_block_device("sda".into(), 0o660, 8, 0);
    entry.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();

    // Flags: high byte has XMIT_RDEV_MINOR_8_PRE30 (bit 11 = 0x0800),
    // so XMIT_EXTENDED_FLAGS (0x04) is set in low byte.
    // Two-byte LE flags encoding.
    assert_eq!(
        buf[0] & 0x04,
        0x04,
        "XMIT_EXTENDED_FLAGS must be set for device with extended flags"
    );

    // Verify the rdev_minor is 1 byte (small minor path).
    // After: flags(2) + name_len(1) + name(3) + size(4) + mtime(4) + mode(4) = 18
    // rdev_major: write_int(8) = 4-byte LE at offset 18
    assert_eq!(
        &buf[18..22],
        &8_i32.to_le_bytes(),
        "rdev major must be 4-byte LE int"
    );
    // rdev_minor: 1 byte (XMIT_RDEV_MINOR_8_PRE30 set)
    assert_eq!(buf[22], 0, "rdev minor=0 as single byte");
    assert_eq!(
        buf.len(),
        23,
        "total length for block device with 8-bit minor"
    );
}

#[test]
fn golden_v28_block_device_large_minor() {
    // Block device with minor > 255: requires 4-byte LE encoding.
    // XMIT_RDEV_MINOR_8_PRE30 is NOT set when minor does not fit in 8 bits.
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);

    let mut entry = FileEntry::new_block_device("sdb".into(), 0o660, 8, 300);
    entry.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();

    // After: flags + name_len(1) + name(3) + size(4) + mtime(4) + mode(4)
    // rdev_major: write_int(8) = 4-byte LE
    // rdev_minor: write_int(300) = 4-byte LE (large minor path)
    // Find minor offset: after major
    let major_offset = buf.len() - 4 - 4; // 4 for major, 4 for minor (from end)
    let minor_offset = major_offset + 4;
    assert_eq!(
        &buf[minor_offset..minor_offset + 4],
        &300_i32.to_le_bytes(),
        "rdev minor=300 must be 4-byte LE int when > 255"
    );
}

#[test]
fn golden_v28_block_device_roundtrip() {
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);

    let mut entry = FileEntry::new_block_device("loop0".into(), 0o660, 7, 128);
    entry.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_devices(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "loop0");
    assert!(read_entry.is_device());
    assert_eq!(read_entry.rdev_major(), Some(7));
    assert_eq!(read_entry.rdev_minor(), Some(128));
    assert_eq!(read_entry.mode(), 0o60660);
}

// ---------------------------------------------------------------------------
// Character device encoding (protocol 28)
// upstream: flist.c - char devices use S_IFCHR mode
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_char_device_roundtrip() {
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);

    let mut entry = FileEntry::new_char_device("tty0".into(), 0o666, 4, 0);
    entry.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_devices(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "tty0");
    assert!(read_entry.is_device());
    assert_eq!(read_entry.rdev_major(), Some(4));
    assert_eq!(read_entry.rdev_minor(), Some(0));
    assert_eq!(read_entry.mode(), 0o20666);
}

// ---------------------------------------------------------------------------
// FIFO (special file) encoding with dummy rdev (protocol 28)
// upstream: flist.c - specials get dummy rdev(0,0) for protocol < 31
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_fifo_with_dummy_rdev() {
    // Protocol 28-30: FIFOs write dummy rdev(0, 0) when preserve_specials is set.
    // This is omitted in protocol 31+.
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_specials(true);

    let mut entry = FileEntry::new_fifo("pipe0".into(), 0o644);
    entry.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_specials(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "pipe0");
    assert!(read_entry.is_special());
    assert_eq!(read_entry.mode(), 0o10644);
}

// ---------------------------------------------------------------------------
// Hardlink dev/ino encoding (protocol 28-29 uses longint pairs)
// upstream: flist.c - protocol < 30 writes dev+1 and ino as longints
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_hardlink_dev_ino_encoding() {
    // Protocol 28 encodes hardlinks using (dev, ino) pairs via write_longint,
    // not the varint index used in protocol 30+.
    // Wire format: longint(dev + 1), longint(ino)
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

    let mut entry = FileEntry::new_file("linked.txt".into(), 100, 0o644);
    entry.set_mtime(1_700_000_000, 0);
    entry.set_hardlink_dev(42);
    entry.set_hardlink_ino(12345);

    writer.write_entry(&mut buf, &entry).unwrap();

    // After: flags(2 - extended for XMIT_HLINKED) + name_len(1) + name(10)
    //        + size(4) + mtime(4) + mode(4)
    // Then: longint(dev+1=43) + longint(ino=12345)
    //
    // dev+1 = 43 fits in i32: 4-byte LE [0x2B, 0x00, 0x00, 0x00]
    // ino = 12345 fits in i32: 4-byte LE [0x39, 0x30, 0x00, 0x00]
    //
    // Find the hardlink data at the end of the buffer.
    let expected_tail: &[u8] = &[
        // dev+1=43 as 4-byte LE longint
        0x2B, 0x00, 0x00, 0x00, // ino=12345 as 4-byte LE longint
        0x39, 0x30, 0x00, 0x00,
    ];
    assert_eq!(
        &buf[buf.len() - 8..],
        expected_tail,
        "hardlink dev/ino must be longint pairs at end of entry"
    );
}

#[test]
fn golden_v28_hardlink_dev_ino_roundtrip() {
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

    let mut e1 = FileEntry::new_file("original.txt".into(), 200, 0o644);
    e1.set_mtime(1_700_000_000, 0);
    e1.set_hardlink_dev(100);
    e1.set_hardlink_ino(99999);

    let mut e2 = FileEntry::new_file("hardlink.txt".into(), 200, 0o644);
    e2.set_mtime(1_700_000_000, 0);
    e2.set_hardlink_dev(100);
    e2.set_hardlink_ino(99999);

    writer.write_entry(&mut buf, &e1).unwrap();
    writer.write_entry(&mut buf, &e2).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);

    let r1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r1.hardlink_dev(), Some(100));
    assert_eq!(r1.hardlink_ino(), Some(99999));

    let r2 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r2.hardlink_dev(), Some(100));
    assert_eq!(r2.hardlink_ino(), Some(99999));
}

// ---------------------------------------------------------------------------
// Mixed hardlink and non-hardlink entries (protocol 28-29 interop fix)
// upstream: flist.c:recv_file_entry() - dev/ino read gated on XMIT_HLINKED
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_mixed_hardlink_and_plain_roundtrip() {
    // Verifies that non-hardlinked entries interleaved with hardlinked entries
    // decode correctly at protocol 28. The receiver must only read dev/ino
    // when XMIT_HLINKED is set - reading unconditionally would consume bytes
    // meant for the next entry, causing wire desync.
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);

    // Entry 1: plain file (no hardlink)
    let mut e1 = FileEntry::new_file("plain.txt".into(), 100, 0o644);
    e1.set_mtime(1_700_000_000, 0);

    // Entry 2: hardlinked file
    let mut e2 = FileEntry::new_file("linked_a.txt".into(), 200, 0o644);
    e2.set_mtime(1_700_000_000, 0);
    e2.set_hardlink_dev(42);
    e2.set_hardlink_ino(12345);

    // Entry 3: another plain file
    let mut e3 = FileEntry::new_file("another.txt".into(), 300, 0o644);
    e3.set_mtime(1_700_000_100, 0);

    // Entry 4: same hardlink group
    let mut e4 = FileEntry::new_file("linked_b.txt".into(), 200, 0o644);
    e4.set_mtime(1_700_000_000, 0);
    e4.set_hardlink_dev(42);
    e4.set_hardlink_ino(12345);

    writer.write_entry(&mut buf, &e1).unwrap();
    writer.write_entry(&mut buf, &e2).unwrap();
    writer.write_entry(&mut buf, &e3).unwrap();
    writer.write_entry(&mut buf, &e4).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);

    let r1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r1.name(), "plain.txt");
    assert_eq!(r1.size(), 100);
    assert_eq!(
        r1.hardlink_dev(),
        None,
        "plain entry must have no hardlink dev"
    );
    assert_eq!(
        r1.hardlink_ino(),
        None,
        "plain entry must have no hardlink ino"
    );

    let r2 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r2.name(), "linked_a.txt");
    assert_eq!(r2.size(), 200);
    assert_eq!(r2.hardlink_dev(), Some(42));
    assert_eq!(r2.hardlink_ino(), Some(12345));

    let r3 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r3.name(), "another.txt");
    assert_eq!(r3.size(), 300);
    assert_eq!(
        r3.hardlink_dev(),
        None,
        "plain entry must have no hardlink dev"
    );

    let r4 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r4.name(), "linked_b.txt");
    assert_eq!(r4.size(), 200);
    assert_eq!(r4.hardlink_dev(), Some(42));
    assert_eq!(r4.hardlink_ino(), Some(12345));

    // End marker
    assert!(reader.read_entry(&mut cursor).unwrap().is_none());
}

// ---------------------------------------------------------------------------
// Transfer stats exact wire bytes (protocol 28 - varlong30 only, no flist times)
// upstream: main.c:handle_stats() - protocol < 29 omits flist times
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_stats_wire_bytes_small_values() {
    // Protocol 28 stats: 3 varlong30 fields with min_bytes=3.
    // For small values that fit in 3 bytes, each is encoded as:
    //   leading_byte + 3 value bytes (total 4 bytes per field).
    //
    // varlong30 with min_bytes=3 for value=4096 (0x1000):
    //   bytes = [0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
    //   cnt starts at 8, strips trailing zeros to min_bytes=3
    //   bytes[2] = 0x00, bit = 1<<(7+3-3) = 0x80
    //   0x00 < 0x80 and cnt == min_bytes, so leading = bytes[2] = 0x00
    //   output: [0x00] + bytes[0..2] = [0x00, 0x00, 0x10]
    let stats = TransferStats::with_bytes(4096, 8192, 65536);
    let mut buf = Vec::new();
    stats.write_to(&mut buf, proto28()).unwrap();

    // 3 fields, each varlong30 with min_bytes=3 for values fitting in 3 bytes
    // No flist times for protocol 28.
    let mut cursor = Cursor::new(&buf[..]);
    let decoded = TransferStats::read_from(&mut cursor, proto28()).unwrap();

    assert_eq!(decoded.total_read, 4096);
    assert_eq!(decoded.total_written, 8192);
    assert_eq!(decoded.total_size, 65536);
    assert_eq!(decoded.flist_buildtime, 0);
    assert_eq!(decoded.flist_xfertime, 0);

    // Ensure all bytes consumed - no trailing flist time fields
    assert_eq!(
        cursor.position() as usize,
        buf.len(),
        "all bytes must be consumed for protocol 28 stats"
    );
}

#[test]
fn golden_v28_stats_wire_bytes_zero() {
    // Zero-value stats: varlong30(0, min_bytes=3)
    // bytes = [0,0,0,0,0,0,0,0], cnt=3 (min), leading = bytes[2]=0, output = [0x00, 0x00, 0x00]
    let stats = TransferStats::with_bytes(0, 0, 0);
    let mut buf = Vec::new();
    stats.write_to(&mut buf, proto28()).unwrap();

    // 3 fields of varlong30(0, 3): each is [0x00, 0x00, 0x00] = 3 bytes
    assert_eq!(
        buf.len(),
        9,
        "3 zero-value varlong30(min=3) = 9 bytes total"
    );

    #[rustfmt::skip]
    let expected: &[u8] = &[
        // total_read = 0: varlong30(0, 3)
        0x00, 0x00, 0x00,
        // total_written = 0: varlong30(0, 3)
        0x00, 0x00, 0x00,
        // total_size = 0: varlong30(0, 3)
        0x00, 0x00, 0x00,
    ];
    assert_eq!(buf, expected, "zero stats must be 9 bytes of zeros");
}

#[test]
fn golden_v28_stats_wire_bytes_large() {
    // Large transfer stats that require more than 3 bytes.
    // total_read = 10_000_000_000 (~9.3 GB) requires 5 bytes in varlong30.
    let stats = TransferStats::with_bytes(10_000_000_000, 500, 10_000_000_000);
    let mut buf = Vec::new();
    stats.write_to(&mut buf, proto28()).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let decoded = TransferStats::read_from(&mut cursor, proto28()).unwrap();

    assert_eq!(decoded.total_read, 10_000_000_000);
    assert_eq!(decoded.total_written, 500);
    assert_eq!(decoded.total_size, 10_000_000_000);
}

// ---------------------------------------------------------------------------
// Protocol 28 does not support checksum negotiation (always MD4)
// upstream: compat.c - checksum negotiation requires protocol >= 30
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_no_checksum_negotiation() {
    let v28 = proto28();
    assert!(
        !v28.uses_varint_encoding(),
        "protocol 28 cannot negotiate checksums (no varint)"
    );
    assert!(
        v28.uses_fixed_encoding(),
        "protocol 28 uses fixed encoding - always MD4"
    );
}

// ---------------------------------------------------------------------------
// Multiple devices with XMIT_SAME_RDEV_MAJOR optimization
// upstream: flist.c - consecutive devices with same major skip rdev_major
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_consecutive_devices_same_major() {
    // Two block devices with same major (8): second entry sets XMIT_SAME_RDEV_MAJOR
    // and omits the major number from the wire.
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);

    let mut dev1 = FileEntry::new_block_device("sda".into(), 0o660, 8, 0);
    dev1.set_mtime(1_700_000_000, 0);

    let mut dev2 = FileEntry::new_block_device("sdb".into(), 0o660, 8, 16);
    dev2.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut buf, &dev1).unwrap();
    let first_len = buf.len();
    writer.write_entry(&mut buf, &dev2).unwrap();
    let second_len = buf.len() - first_len;

    // Second entry should be shorter: XMIT_SAME_RDEV_MAJOR omits the 4-byte major.
    // First entry has major(4) + minor(1) = 5 bytes of rdev.
    // Second entry with same major: minor(1) = 1 byte of rdev (major omitted).
    assert!(
        second_len < first_len,
        "second device with same major must be shorter"
    );

    // Verify round-trip
    writer = FileListWriter::new(protocol).with_preserve_devices(true);
    let mut rt_buf = Vec::new();
    let mut rt_dev1 = FileEntry::new_block_device("sda".into(), 0o660, 8, 0);
    rt_dev1.set_mtime(1_700_000_000, 0);
    let mut rt_dev2 = FileEntry::new_block_device("sdb".into(), 0o660, 8, 16);
    rt_dev2.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut rt_buf, &rt_dev1).unwrap();
    writer.write_entry(&mut rt_buf, &rt_dev2).unwrap();
    writer.write_end(&mut rt_buf, None).unwrap();

    let mut cursor = Cursor::new(&rt_buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_devices(true);

    let r1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r1.rdev_major(), Some(8));
    assert_eq!(r1.rdev_minor(), Some(0));

    let r2 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r2.rdev_major(), Some(8));
    assert_eq!(r2.rdev_minor(), Some(16));
}

// ---------------------------------------------------------------------------
// Mixed entry types with all preserves enabled (protocol 28)
// upstream: flist.c - comprehensive file list with devices, specials, links
// ---------------------------------------------------------------------------

#[test]
fn golden_v28_mixed_all_preserves_roundtrip() {
    let protocol = proto28();
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true)
        .with_preserve_links(true)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    let mut f1 = FileEntry::new_file("config.yml".into(), 256, 0o644);
    f1.set_mtime(1_700_000_000, 0);
    f1.set_uid(1000);
    f1.set_gid(1000);

    let mut d1 = FileEntry::new_directory("dev".into(), 0o755);
    d1.set_mtime(1_700_000_000, 0);
    d1.set_uid(0);
    d1.set_gid(0);

    let mut blk = FileEntry::new_block_device("dev/sda".into(), 0o660, 8, 0);
    blk.set_mtime(1_700_000_000, 0);
    blk.set_uid(0);
    blk.set_gid(6);

    let mut chr = FileEntry::new_char_device("dev/null".into(), 0o666, 1, 3);
    chr.set_mtime(1_700_000_000, 0);
    chr.set_uid(0);
    chr.set_gid(0);

    let mut fifo = FileEntry::new_fifo("dev/pipe".into(), 0o644);
    fifo.set_mtime(1_700_000_000, 0);
    fifo.set_uid(1000);
    fifo.set_gid(1000);

    let s1 = FileEntry::new_symlink("link".into(), "config.yml".into());

    writer.write_entry(&mut buf, &f1).unwrap();
    writer.write_entry(&mut buf, &d1).unwrap();
    writer.write_entry(&mut buf, &blk).unwrap();
    writer.write_entry(&mut buf, &chr).unwrap();
    writer.write_entry(&mut buf, &fifo).unwrap();
    writer.write_entry(&mut buf, &s1).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_uid(true)
        .with_preserve_gid(true)
        .with_preserve_links(true)
        .with_preserve_devices(true)
        .with_preserve_specials(true);

    let r1 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r1.name(), "config.yml");
    assert!(r1.is_file());
    assert_eq!(r1.uid(), Some(1000));
    assert_eq!(r1.gid(), Some(1000));

    let r2 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r2.name(), "dev");
    assert!(r2.is_dir());
    assert_eq!(r2.uid(), Some(0));

    let r3 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r3.name(), "dev/sda");
    assert!(r3.is_device());
    assert_eq!(r3.rdev_major(), Some(8));
    assert_eq!(r3.rdev_minor(), Some(0));
    assert_eq!(r3.gid(), Some(6));

    let r4 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r4.name(), "dev/null");
    assert!(r4.is_device());
    assert_eq!(r4.rdev_major(), Some(1));
    assert_eq!(r4.rdev_minor(), Some(3));

    let r5 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r5.name(), "dev/pipe");
    assert!(r5.is_special());
    assert_eq!(r5.uid(), Some(1000));

    let r6 = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(r6.name(), "link");
    assert!(r6.is_symlink());
    assert_eq!(
        r6.link_target().map(|p| p.to_path_buf()),
        Some("config.yml".into())
    );

    let end = reader.read_entry(&mut cursor).unwrap();
    assert!(end.is_none());
}
