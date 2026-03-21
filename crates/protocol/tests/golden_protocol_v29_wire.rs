//! Golden byte tests for protocol version 29 wire format.
//!
//! Protocol 29 is the first version to include flist build/transfer time
//! fields in transfer statistics. It shares the same file list encoding as
//! protocol 28 (fixed-width, no varint) but adds two varlong30 timing
//! fields after the core stats.
//!
//! Key protocol 29 characteristics:
//! - Transfer stats include flist_buildtime and flist_xfertime (varlong30)
//! - File list encoding identical to protocol 28 (fixed 4-byte LE integers)
//! - No incremental recursion (introduced in v30)
//! - No varint flist flags (introduced in v30)
//! - Uses legacy ASCII negotiation (not binary compat flags)
//! - MD4 checksums (16 bytes), same as protocol 28
//! - End-of-list marker is single `0x00` byte
//!
//! # Upstream Reference
//!
//! Transfer stats: `main.c:handle_stats()` - protocol >= 29 sends flist times.
//! File list encoding: `flist.c:send_file_entry()` / `recv_file_entry()` -
//! protocol 29 takes the `protocol_version < 30` code paths.

use std::io::Cursor;

use protocol::flist::{FileEntry, FileListReader, FileListWriter};
use protocol::{ProtocolVersion, TransferStats, write_int, write_longint};

fn v29() -> ProtocolVersion {
    ProtocolVersion::from_supported(29).expect("v29 must be supported")
}

// ---------------------------------------------------------------------------
// Transfer stats with flist build/transfer times
// upstream: main.c:handle_stats() - protocol >= 29 adds flist times
// ---------------------------------------------------------------------------

/// Verifies that protocol 29 transfer stats include flist timing fields.
///
/// Wire layout (all varlong30 with min_bytes=3):
///   total_read      : varlong30
///   total_written   : varlong30
///   total_size      : varlong30
///   flist_buildtime : varlong30 (microseconds, protocol >= 29 only)
///   flist_xfertime  : varlong30 (microseconds, protocol >= 29 only)
#[test]
fn golden_v29_stats_with_flist_times() {
    let stats = TransferStats::with_bytes(4096, 8192, 100_000).with_flist_times(500_000, 100_000);

    let protocol = v29();
    let mut buf = Vec::new();
    stats.write_to(&mut buf, protocol).unwrap();

    // Protocol 29 encodes 5 fields (3 core + 2 flist times), each as varlong30(min_bytes=3).
    // Verify round-trip decodes all 5 fields correctly.
    let mut cursor = Cursor::new(&buf[..]);
    let decoded = TransferStats::read_from(&mut cursor, protocol).unwrap();

    assert_eq!(decoded.total_read, 4096);
    assert_eq!(decoded.total_written, 8192);
    assert_eq!(decoded.total_size, 100_000);
    assert_eq!(decoded.flist_buildtime, 500_000);
    assert_eq!(decoded.flist_xfertime, 100_000);
}

/// Verifies that protocol 29 stats with zero flist times round-trip correctly.
///
/// Even when flist times are zero, protocol 29 writes them to the wire.
#[test]
fn golden_v29_stats_zero_flist_times() {
    let stats = TransferStats::with_bytes(1024, 2048, 50_000);
    // flist times default to 0

    let protocol = v29();
    let mut buf = Vec::new();
    stats.write_to(&mut buf, protocol).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let decoded = TransferStats::read_from(&mut cursor, protocol).unwrap();

    assert_eq!(decoded.total_read, 1024);
    assert_eq!(decoded.total_written, 2048);
    assert_eq!(decoded.total_size, 50_000);
    assert_eq!(decoded.flist_buildtime, 0, "zero flist_buildtime preserved");
    assert_eq!(decoded.flist_xfertime, 0, "zero flist_xfertime preserved");
}

/// Verifies exact wire bytes for protocol 29 transfer stats.
///
/// Each field uses varlong30 encoding with min_bytes=3:
///   - Leading byte encodes the number of significant bytes
///   - Followed by min_bytes-1 (=2) bytes of the LE value
///
/// For small values that fit in 3 bytes (values < 2^23 with bit 7 of
/// byte 2 clear), the encoding is: [byte2, byte0, byte1] where
/// byte0..byte2 are the LE representation.
#[test]
fn golden_v29_stats_exact_wire_bytes() {
    // Use values where we can predict the exact varlong30 encoding.
    // varlong30(0, min_bytes=3): leading=0x00, then 2 zero bytes -> [0x00, 0x00, 0x00]
    let stats = TransferStats::with_bytes(0, 0, 0);

    let protocol = v29();
    let mut buf = Vec::new();
    stats.write_to(&mut buf, protocol).unwrap();

    // 5 fields, each 3 bytes (all zeros) = 15 bytes total
    assert_eq!(buf.len(), 15, "5 zero-valued varlong30 fields = 15 bytes");

    #[rustfmt::skip]
    let expected: &[u8] = &[
        // total_read = 0
        0x00, 0x00, 0x00,
        // total_written = 0
        0x00, 0x00, 0x00,
        // total_size = 0
        0x00, 0x00, 0x00,
        // flist_buildtime = 0
        0x00, 0x00, 0x00,
        // flist_xfertime = 0
        0x00, 0x00, 0x00,
    ];

    assert_eq!(buf, expected, "all-zero stats wire bytes");
}

/// Verifies that protocol 29 stats are longer than protocol 28 stats
/// due to the two additional flist timing fields.
#[test]
fn golden_v29_stats_longer_than_v28() {
    let v28 = ProtocolVersion::from_supported(28).expect("v28 must be supported");
    let protocol = v29();

    let stats = TransferStats::with_bytes(4096, 8192, 100_000).with_flist_times(500_000, 100_000);

    let mut buf_28 = Vec::new();
    stats.write_to(&mut buf_28, v28).unwrap();

    let mut buf_29 = Vec::new();
    stats.write_to(&mut buf_29, protocol).unwrap();

    // Protocol 28: 3 fields. Protocol 29: 5 fields.
    assert!(
        buf_29.len() > buf_28.len(),
        "v29 stats ({} bytes) must be longer than v28 ({} bytes) due to flist times",
        buf_29.len(),
        buf_28.len()
    );

    // Protocol 28 should NOT decode flist times
    let mut cursor_28 = Cursor::new(&buf_28[..]);
    let decoded_28 = TransferStats::read_from(&mut cursor_28, v28).unwrap();
    assert_eq!(decoded_28.flist_buildtime, 0, "v28 has no flist_buildtime");
    assert_eq!(decoded_28.flist_xfertime, 0, "v28 has no flist_xfertime");
}

/// Verifies that protocol 29 stats with large flist times round-trip correctly.
///
/// Flist times are in microseconds, so realistic values can be large
/// (e.g., 10 seconds = 10_000_000 microseconds).
#[test]
fn golden_v29_stats_large_flist_times() {
    let stats = TransferStats::with_bytes(1_000_000_000, 500_000_000, 10_000_000_000)
        .with_flist_times(10_000_000, 5_000_000); // 10s build, 5s transfer

    let protocol = v29();
    let mut buf = Vec::new();
    stats.write_to(&mut buf, protocol).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let decoded = TransferStats::read_from(&mut cursor, protocol).unwrap();

    assert_eq!(decoded.total_read, 1_000_000_000);
    assert_eq!(decoded.total_written, 500_000_000);
    assert_eq!(decoded.total_size, 10_000_000_000);
    assert_eq!(decoded.flist_buildtime, 10_000_000);
    assert_eq!(decoded.flist_xfertime, 5_000_000);
}

/// Verifies swap_perspective preserves flist times.
#[test]
fn golden_v29_stats_swap_preserves_flist_times() {
    let stats = TransferStats::with_bytes(1024, 2048, 10_000).with_flist_times(500_000, 100_000);

    let swapped = stats.swap_perspective();

    // Read/write swap
    assert_eq!(swapped.total_read, 2048);
    assert_eq!(swapped.total_written, 1024);
    assert_eq!(swapped.total_size, 10_000);
    // Flist times are preserved, not swapped
    assert_eq!(swapped.flist_buildtime, 500_000);
    assert_eq!(swapped.flist_xfertime, 100_000);

    // Verify swapped stats round-trip through wire
    let protocol = v29();
    let mut buf = Vec::new();
    swapped.write_to(&mut buf, protocol).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let decoded = TransferStats::read_from(&mut cursor, protocol).unwrap();
    assert_eq!(decoded, swapped);
}

// ---------------------------------------------------------------------------
// Protocol version 29 feature flags
// upstream: various protocol version guards throughout the codebase
// ---------------------------------------------------------------------------

/// Verifies protocol 29 supports flist times but not varint encoding.
#[test]
fn golden_v29_feature_flags() {
    let protocol = v29();

    // v29 supports flist times (the key addition over v28)
    assert!(
        protocol.supports_flist_times(),
        "v29 must support flist times"
    );

    // v29 uses fixed encoding (not varint, introduced in v30)
    assert!(
        protocol.uses_fixed_encoding(),
        "v29 must use fixed encoding"
    );
    assert!(
        !protocol.uses_varint_encoding(),
        "v29 must not use varint encoding"
    );

    // v29 uses legacy ASCII negotiation (not binary)
    assert!(
        protocol.uses_legacy_ascii_negotiation(),
        "v29 must use legacy ASCII negotiation"
    );
    assert!(
        !protocol.uses_binary_negotiation(),
        "v29 must not use binary negotiation"
    );

    // v29 does not use varint flist flags
    assert!(
        !protocol.uses_varint_flist_flags(),
        "v29 must not use varint flist flags"
    );
}

/// Verifies the boundary: v28 does NOT support flist times, v29 does.
#[test]
fn golden_v29_flist_times_boundary() {
    let v28 = ProtocolVersion::from_supported(28).expect("v28 must be supported");
    let protocol = v29();
    let v30 = ProtocolVersion::from_supported(30).expect("v30 must be supported");

    assert!(
        !v28.supports_flist_times(),
        "v28 must NOT support flist times"
    );
    assert!(
        protocol.supports_flist_times(),
        "v29 must support flist times"
    );
    assert!(v30.supports_flist_times(), "v30 must support flist times");
}

// ---------------------------------------------------------------------------
// Checksum header (SumHead) for protocol 29 - same as v28, uses MD4
// upstream: match.c/sender.c - s2length=16 for MD4 in sum_head
// ---------------------------------------------------------------------------

/// Verifies the SumHead wire format for protocol 29 uses MD4 (16-byte strong checksum).
///
/// Wire layout: count(4) + blength(4) + s2length(4) + remainder(4) = 16 bytes.
/// Same as protocol 28 - both use write_int (fixed 4-byte LE).
#[test]
fn golden_v29_sum_head_md4() {
    // SumHead: count=50, blength=700, s2length=16 (MD4), remainder=150
    let mut buf = Vec::new();
    write_int(&mut buf, 50).unwrap(); // count
    write_int(&mut buf, 700).unwrap(); // blength
    write_int(&mut buf, 16).unwrap(); // s2length (MD4 = 16 bytes)
    write_int(&mut buf, 150).unwrap(); // remainder

    assert_eq!(buf.len(), 16, "SumHead is 4 fixed-width fields = 16 bytes");

    #[rustfmt::skip]
    let expected: &[u8] = &[
        // count = 50 = 0x32
        0x32, 0x00, 0x00, 0x00,
        // blength = 700 = 0x02BC
        0xBC, 0x02, 0x00, 0x00,
        // s2length = 16 = 0x10 (MD4 digest length)
        0x10, 0x00, 0x00, 0x00,
        // remainder = 150 = 0x96
        0x96, 0x00, 0x00, 0x00,
    ];

    assert_eq!(buf, expected, "SumHead wire bytes for MD4 checksum");

    // Round-trip
    let mut cursor = Cursor::new(&buf);
    use protocol::read_int;
    assert_eq!(read_int(&mut cursor).unwrap(), 50, "count");
    assert_eq!(read_int(&mut cursor).unwrap(), 700, "blength");
    assert_eq!(read_int(&mut cursor).unwrap(), 16, "s2length");
    assert_eq!(read_int(&mut cursor).unwrap(), 150, "remainder");
}

// ---------------------------------------------------------------------------
// File entry checksum encoding for protocol 29 (MD4, 16 bytes)
// upstream: flist.c - always_checksum sends flist_csum_len bytes per regular file
// ---------------------------------------------------------------------------

/// Verifies that protocol 29 writes 16-byte MD4 checksums for regular files
/// when always_checksum is enabled.
#[test]
fn golden_v29_checksum_regular_file() {
    let protocol = v29();
    let md4_len = 16;
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_always_checksum(md4_len);

    let mut entry = FileEntry::new_file("check.txt".into(), 10, 0o644);
    entry.set_mtime(1_000_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();

    // Layout: flags(1) + name_len(1) + name(9) + size(4) + mtime(4) + mode(4) = 23 + checksum(16) = 39
    assert_eq!(buf.len(), 23 + md4_len, "entry with MD4 checksum");
    assert_eq!(
        &buf[23..],
        &[0u8; 16],
        "MD4 checksum should be 16 zero bytes when not set"
    );
}

/// Verifies that a known checksum digest is written verbatim on the wire for v29.
#[test]
fn golden_v29_checksum_known_digest() {
    let protocol = v29();
    let md4_len = 16;
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_always_checksum(md4_len);

    let mut entry = FileEntry::new_file("x.bin".into(), 1, 0o644);
    entry.set_mtime(1_000_000_000, 0);
    let digest: Vec<u8> = (0xA0..0xB0).collect(); // 16 bytes: [0xA0, 0xA1, ..., 0xAF]
    entry.set_checksum(digest.clone());

    writer.write_entry(&mut buf, &entry).unwrap();

    let csum_start = buf.len() - md4_len;
    assert_eq!(
        &buf[csum_start..],
        &digest,
        "checksum bytes must match the set digest"
    );
}

/// Verifies checksum round-trip for protocol 29.
#[test]
fn golden_v29_checksum_roundtrip() {
    let protocol = v29();
    let md4_len = 16;
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_always_checksum(md4_len);

    let mut entry = FileEntry::new_file("data.bin".into(), 512, 0o644);
    entry.set_mtime(1_700_000_000, 0);
    let digest: Vec<u8> = (0..16).collect();
    entry.set_checksum(digest.clone());

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_always_checksum(md4_len);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "data.bin");
    assert_eq!(read_entry.size(), 512);
    assert_eq!(read_entry.checksum(), Some(digest.as_slice()));
}

// ---------------------------------------------------------------------------
// Longint encoding (used by protocol 29 for file sizes)
// upstream: io.c - write_longint() / read_longint()
// ---------------------------------------------------------------------------

/// Verifies small longint values use 4-byte encoding at protocol 29.
#[test]
fn golden_v29_longint_small_value() {
    // Values <= 0x7FFFFFFF use 4-byte LE encoding.
    let mut buf = Vec::new();
    write_longint(&mut buf, 99999).unwrap();

    // 99999 = 0x1869F LE: [0x9F, 0x86, 0x01, 0x00]
    assert_eq!(buf, [0x9F, 0x86, 0x01, 0x00]);
    assert_eq!(buf.len(), 4);

    let mut cursor = Cursor::new(&buf);
    use protocol::read_longint;
    assert_eq!(read_longint(&mut cursor).unwrap(), 99999);
}

/// Verifies large longint values use 12-byte marker+value encoding at protocol 29.
#[test]
fn golden_v29_longint_large_value() {
    // Values > 0x7FFFFFFF use 0xFFFFFFFF marker + 8-byte LE i64.
    let value: i64 = 8_000_000_000; // ~7.5 GB
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
// Device entry encoding for protocol 29
// upstream: flist.c:send_file_entry() - device numbers for S_ISBLK/S_ISCHR
// ---------------------------------------------------------------------------

/// Verifies block device entry encoding and round-trip at protocol 29.
///
/// Device entries include rdev (major/minor) after mode when preserve_devices
/// is enabled.
#[test]
fn golden_v29_block_device_roundtrip() {
    let protocol = v29();
    let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);
    let mut buf = Vec::new();

    let mut entry = FileEntry::new_block_device("sda".into(), 0o660, 8, 0);
    entry.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_devices(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "sda");
    assert!(
        read_entry.is_block_device(),
        "entry should be a block device"
    );
    assert_eq!(read_entry.mtime(), 1_700_000_000);
}

/// Verifies char device entry round-trip at protocol 29.
#[test]
fn golden_v29_char_device_roundtrip() {
    let protocol = v29();
    let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);
    let mut buf = Vec::new();

    let mut entry = FileEntry::new_char_device("null".into(), 0o666, 1, 3);
    entry.set_mtime(1_700_000_000, 0);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_devices(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "null");
    assert!(read_entry.is_char_device(), "entry should be a char device");
}

// ---------------------------------------------------------------------------
// Hardlink encoding for protocol 29
// upstream: flist.c:send_file_entry() - XMIT_HLINKED flag, hardlink index
// ---------------------------------------------------------------------------

/// Verifies that hardlink entries round-trip at protocol 29.
///
/// Protocol 29 uses the same hardlink encoding as protocol 28:
/// XMIT_HLINKED flag in the entry flags, followed by the hardlink
/// device and inode written with fixed-width encoding.
#[test]
fn golden_v29_hardlink_roundtrip() {
    let protocol = v29();
    let mut writer = FileListWriter::new(protocol).with_preserve_hard_links(true);
    let mut buf = Vec::new();

    let mut entry = FileEntry::new_file("linked.txt".into(), 1024, 0o644);
    entry.set_mtime(1_700_000_000, 0);
    entry.set_hardlink_dev(0xFD00);
    entry.set_hardlink_ino(123_456);

    writer.write_entry(&mut buf, &entry).unwrap();
    writer.write_end(&mut buf, None).unwrap();

    let mut cursor = Cursor::new(&buf[..]);
    let mut reader = FileListReader::new(protocol).with_preserve_hard_links(true);

    let read_entry = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert_eq!(read_entry.name(), "linked.txt");
    assert_eq!(read_entry.size(), 1024);
}

// ---------------------------------------------------------------------------
// Wire format identity: v29 file list encoding matches v28
// upstream: flist.c - protocol 29 takes same code path as 28 for flist
// ---------------------------------------------------------------------------

/// Verifies that protocol 29 and 28 produce identical wire bytes for
/// all file entry types (regular file, directory, symlink).
#[test]
fn golden_v29_flist_wire_identical_to_v28() {
    let v28 = ProtocolVersion::from_supported(28).expect("v28 must be supported");
    let protocol = v29();

    // Regular file
    {
        let mut writer_28 = FileListWriter::new(v28);
        let mut writer_29 = FileListWriter::new(protocol);
        let mut buf_28 = Vec::new();
        let mut buf_29 = Vec::new();

        let mut entry = FileEntry::new_file("test.dat".into(), 4096, 0o755);
        entry.set_mtime(1_700_000_000, 0);

        writer_28.write_entry(&mut buf_28, &entry).unwrap();
        writer_29.write_entry(&mut buf_29, &entry).unwrap();

        assert_eq!(
            buf_28, buf_29,
            "v28 and v29 must produce identical bytes for regular file"
        );
    }

    // Directory
    {
        let mut writer_28 = FileListWriter::new(v28);
        let mut writer_29 = FileListWriter::new(protocol);
        let mut buf_28 = Vec::new();
        let mut buf_29 = Vec::new();

        let mut entry = FileEntry::new_directory("subdir".into(), 0o755);
        entry.set_mtime(1_700_000_000, 0);

        writer_28.write_entry(&mut buf_28, &entry).unwrap();
        writer_29.write_entry(&mut buf_29, &entry).unwrap();

        assert_eq!(
            buf_28, buf_29,
            "v28 and v29 must produce identical bytes for directory"
        );
    }

    // Symlink
    {
        let mut writer_28 = FileListWriter::new(v28).with_preserve_links(true);
        let mut writer_29 = FileListWriter::new(protocol).with_preserve_links(true);
        let mut buf_28 = Vec::new();
        let mut buf_29 = Vec::new();

        let mut entry = FileEntry::new_symlink("link".into(), "/target".into());
        entry.set_mtime(1_700_000_000, 0);

        writer_28.write_entry(&mut buf_28, &entry).unwrap();
        writer_29.write_entry(&mut buf_29, &entry).unwrap();

        assert_eq!(
            buf_28, buf_29,
            "v28 and v29 must produce identical bytes for symlink"
        );
    }
}

/// Verifies that protocol 29 flist encoding differs from protocol 30
/// (varint boundary).
#[test]
fn golden_v29_flist_wire_differs_from_v30() {
    let protocol = v29();
    let v30 = ProtocolVersion::from_supported(30).expect("v30 must be supported");

    let mut writer_29 = FileListWriter::new(protocol);
    let mut writer_30 = FileListWriter::new(v30);
    let mut buf_29 = Vec::new();
    let mut buf_30 = Vec::new();

    let mut entry = FileEntry::new_file("test.dat".into(), 4096, 0o644);
    entry.set_mtime(1_700_000_000, 0);

    writer_29.write_entry(&mut buf_29, &entry).unwrap();
    writer_30.write_entry(&mut buf_30, &entry).unwrap();

    assert_ne!(
        buf_29, buf_30,
        "v29 fixed encoding and v30 varint encoding must differ"
    );
}

// ---------------------------------------------------------------------------
// End-of-list marker
// upstream: flist.c - zero byte terminates, no safe file list for proto < 30
// ---------------------------------------------------------------------------

/// Verifies end-of-list marker is a single zero byte for protocol 29.
#[test]
fn golden_v29_end_marker() {
    let writer = FileListWriter::new(v29());
    let mut buf = Vec::new();
    writer.write_end(&mut buf, None).unwrap();

    assert_eq!(buf, [0x00], "protocol 29 end marker is single zero byte");
}

/// Verifies end-of-list marker ignores io_error for protocol 29
/// (no safe file list support).
#[test]
fn golden_v29_end_marker_ignores_io_error() {
    let writer = FileListWriter::new(v29());
    let mut buf = Vec::new();
    writer.write_end(&mut buf, Some(42)).unwrap();

    assert_eq!(
        buf,
        [0x00],
        "protocol 29 end marker must ignore io_error (no safe file list)"
    );
}

// ---------------------------------------------------------------------------
// Full mixed file list with stats - end-to-end protocol 29 scenario
// upstream: complete transfer session with flist + stats
// ---------------------------------------------------------------------------

/// Verifies a complete protocol 29 scenario: write a mixed file list,
/// terminate it, write transfer stats with flist times, then decode everything.
#[test]
fn golden_v29_full_session_flist_and_stats() {
    let protocol = v29();

    // Phase 1: File list
    let mut flist_buf = Vec::new();
    let mut writer = FileListWriter::new(protocol)
        .with_preserve_links(true)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

    let mut dir = FileEntry::new_directory("src".into(), 0o755);
    dir.set_mtime(1_700_000_000, 0);

    let mut file = FileEntry::new_file("src/lib.rs".into(), 8192, 0o644);
    file.set_mtime(1_700_000_100, 0);
    file.set_uid(1000);
    file.set_gid(1000);

    let mut link = FileEntry::new_symlink("src/latest".into(), "lib.rs".into());
    link.set_mtime(1_700_000_200, 0);

    writer.write_entry(&mut flist_buf, &dir).unwrap();
    writer.write_entry(&mut flist_buf, &file).unwrap();
    writer.write_entry(&mut flist_buf, &link).unwrap();
    writer.write_end(&mut flist_buf, None).unwrap();

    // Phase 2: Transfer stats
    let stats = TransferStats::with_bytes(flist_buf.len() as u64, 8192, 8192)
        .with_flist_times(250_000, 50_000);

    let mut stats_buf = Vec::new();
    stats.write_to(&mut stats_buf, protocol).unwrap();

    // Decode file list
    let mut cursor = Cursor::new(&flist_buf[..]);
    let mut reader = FileListReader::new(protocol)
        .with_preserve_links(true)
        .with_preserve_uid(true)
        .with_preserve_gid(true);

    let r_dir = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert!(r_dir.is_dir());
    assert_eq!(r_dir.name(), "src");

    let r_file = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert!(r_file.is_file());
    assert_eq!(r_file.name(), "src/lib.rs");
    assert_eq!(r_file.size(), 8192);
    assert_eq!(r_file.uid(), Some(1000));
    assert_eq!(r_file.gid(), Some(1000));

    let r_link = reader.read_entry(&mut cursor).unwrap().unwrap();
    assert!(r_link.is_symlink());
    assert_eq!(r_link.name(), "src/latest");

    assert!(reader.read_entry(&mut cursor).unwrap().is_none());

    // Decode stats
    let mut stats_cursor = Cursor::new(&stats_buf[..]);
    let decoded_stats = TransferStats::read_from(&mut stats_cursor, protocol).unwrap();

    assert_eq!(decoded_stats.total_read, flist_buf.len() as u64);
    assert_eq!(decoded_stats.total_written, 8192);
    assert_eq!(decoded_stats.total_size, 8192);
    assert_eq!(decoded_stats.flist_buildtime, 250_000);
    assert_eq!(decoded_stats.flist_xfertime, 50_000);
}
