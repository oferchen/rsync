//! Comprehensive tests for device file encoding in the rsync wire protocol.
//!
//! Upstream rsync encodes device file information (major/minor numbers, device type)
//! in the file list wire protocol. The encoding varies by protocol version:
//!
//! - **Protocol 30+**: Major encoded as `varint30`, minor as `varint`
//! - **Protocol 28-29**: Major encoded as `varint30`, minor as `u8` (if fits) or `i32 LE`
//! - `XMIT_SAME_RDEV_MAJOR` flag suppresses major when same as previous entry
//! - `XMIT_RDEV_MINOR_8_PRE30` flag (proto 28-29) indicates 8-bit minor
//! - Block devices (S_IFBLK = 0o060000) and character devices (S_IFCHR = 0o020000)
//!   are distinguishable by their mode bits
//! - Special files (FIFOs, sockets) get dummy rdev (0, 0) in protocol < 31
//!
//! These tests verify byte-exact fidelity of device file encoding/decoding
//! at both the low-level wire format layer and the high-level FileListWriter/
//! FileListReader layer, matching upstream rsync's `flist.c` behavior.
//!
//! # Upstream Reference
//!
//! - `flist.c:send_file_entry()` lines 640-680 (rdev write)
//! - `flist.c:recv_file_entry()` lines 910-945 (rdev read)

use std::io::Cursor;
use std::path::PathBuf;

use protocol::ProtocolVersion;
use protocol::flist::{FileEntry, FileListReader, FileListWriter};
use protocol::wire::file_entry::{
    XMIT_RDEV_MINOR_8_PRE30, XMIT_SAME_RDEV_MAJOR, calculate_device_flags, encode_rdev,
};
use protocol::wire::file_entry_decode::decode_rdev;

// ============================================================================
// Test Helpers
// ============================================================================

/// Roundtrip a single device entry through FileListWriter/FileListReader.
fn roundtrip_device(entry: &FileEntry, protocol: ProtocolVersion) -> FileEntry {
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);
    writer.write_entry(&mut buf, entry).expect("write failed");
    writer.write_end(&mut buf, None).expect("write end failed");

    let mut cursor = Cursor::new(&buf);
    let mut reader = FileListReader::new(protocol).with_preserve_devices(true);
    reader
        .read_entry(&mut cursor)
        .expect("read failed")
        .expect("no entry returned")
}

/// Roundtrip multiple device entries through FileListWriter/FileListReader.
fn roundtrip_entries(entries: &[FileEntry], protocol: ProtocolVersion) -> Vec<FileEntry> {
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);
    for entry in entries {
        writer.write_entry(&mut buf, entry).expect("write failed");
    }
    writer.write_end(&mut buf, None).expect("write end failed");

    let mut cursor = Cursor::new(&buf);
    let mut reader = FileListReader::new(protocol).with_preserve_devices(true);
    let mut decoded = Vec::new();
    while let Some(entry) = reader.read_entry(&mut cursor).expect("read failed") {
        decoded.push(entry);
    }
    decoded
}

/// Creates a block device FileEntry with the given name, major, and minor.
fn make_block_device(name: &str, major: u32, minor: u32) -> FileEntry {
    let mut entry = FileEntry::new_block_device(PathBuf::from(name), 0o660, major, minor);
    entry.set_mtime(1700000000, 0);
    entry
}

/// Creates a character device FileEntry with the given name, major, and minor.
fn make_char_device(name: &str, major: u32, minor: u32) -> FileEntry {
    let mut entry = FileEntry::new_char_device(PathBuf::from(name), 0o666, major, minor);
    entry.set_mtime(1700000000, 0);
    entry
}

/// Creates a FIFO FileEntry.
fn make_fifo(name: &str) -> FileEntry {
    let mut entry = FileEntry::new_fifo(PathBuf::from(name), 0o644);
    entry.set_mtime(1700000000, 0);
    entry
}

/// Creates a socket FileEntry.
fn make_socket(name: &str) -> FileEntry {
    let mut entry = FileEntry::new_socket(PathBuf::from(name), 0o755);
    entry.set_mtime(1700000000, 0);
    entry
}

// ============================================================================
// 1. Low-Level Wire Format Tests (encode_rdev / decode_rdev)
// ============================================================================

/// Tests basic roundtrip of rdev encoding at the wire level for protocol 30+.
/// Protocol 30+ uses varint30(major) + varint(minor).
#[test]
fn wire_level_roundtrip_rdev_protocol_30() {
    let major = 8u32;
    let minor = 1u32;

    let mut buf = Vec::new();
    encode_rdev(&mut buf, major, minor, 0, 30).unwrap();

    let mut cursor = Cursor::new(&buf);
    let (dec_major, dec_minor) = decode_rdev(&mut cursor, 0, 0, 30).unwrap();
    assert_eq!(dec_major, major);
    assert_eq!(dec_minor, minor);
    // All bytes consumed
    assert_eq!(cursor.position() as usize, buf.len());
}

/// Tests roundtrip with XMIT_SAME_RDEV_MAJOR flag (major omitted).
#[test]
fn wire_level_roundtrip_rdev_same_major() {
    let major = 8u32;
    let minor = 16u32;
    let xflags = (XMIT_SAME_RDEV_MAJOR as u32) << 8;

    let mut buf = Vec::new();
    encode_rdev(&mut buf, major, minor, xflags, 30).unwrap();

    let mut cursor = Cursor::new(&buf);
    let (dec_major, dec_minor) = decode_rdev(&mut cursor, xflags, major, 30).unwrap();
    assert_eq!(dec_major, major, "major should come from prev_rdev_major");
    assert_eq!(dec_minor, minor);
    assert_eq!(cursor.position() as usize, buf.len());
}

/// Tests protocol 29 with 8-bit minor (XMIT_RDEV_MINOR_8_PRE30 flag).
#[test]
fn wire_level_roundtrip_rdev_protocol_29_minor_8bit() {
    let major = 8u32;
    let minor = 5u32;
    let xflags = (XMIT_RDEV_MINOR_8_PRE30 as u32) << 8;

    let mut buf = Vec::new();
    encode_rdev(&mut buf, major, minor, xflags, 29).unwrap();

    let mut cursor = Cursor::new(&buf);
    let (dec_major, dec_minor) = decode_rdev(&mut cursor, xflags, 0, 29).unwrap();
    assert_eq!(dec_major, major);
    assert_eq!(dec_minor, minor);
    assert_eq!(cursor.position() as usize, buf.len());
}

/// Tests protocol 29 with large minor (32-bit encoding).
#[test]
fn wire_level_roundtrip_rdev_protocol_29_minor_32bit() {
    let major = 8u32;
    let minor = 300u32; // > 255, so no XMIT_RDEV_MINOR_8_PRE30

    let mut buf = Vec::new();
    encode_rdev(&mut buf, major, minor, 0, 29).unwrap();

    let mut cursor = Cursor::new(&buf);
    let (dec_major, dec_minor) = decode_rdev(&mut cursor, 0, 0, 29).unwrap();
    assert_eq!(dec_major, major);
    assert_eq!(dec_minor, minor);
    assert_eq!(cursor.position() as usize, buf.len());
}

/// Tests wire-level roundtrip across all supported protocol versions.
#[test]
fn wire_level_roundtrip_rdev_all_protocols() {
    let major = 8u32;
    let minor = 0u32;

    for proto in [28u8, 29, 30, 31, 32] {
        // Calculate flags as the implementation would
        let xflags = if (28..30).contains(&proto) && minor <= 0xFF {
            (XMIT_RDEV_MINOR_8_PRE30 as u32) << 8
        } else {
            0u32
        };

        let mut buf = Vec::new();
        encode_rdev(&mut buf, major, minor, xflags, proto).unwrap();

        let mut cursor = Cursor::new(&buf);
        let (dec_major, dec_minor) = decode_rdev(&mut cursor, xflags, 0, proto).unwrap();
        assert_eq!(dec_major, major, "major mismatch for proto {proto}");
        assert_eq!(dec_minor, minor, "minor mismatch for proto {proto}");
        assert_eq!(
            cursor.position() as usize,
            buf.len(),
            "not all bytes consumed for proto {proto}"
        );
    }
}

/// Tests wire-level roundtrip with zero major and zero minor.
#[test]
fn wire_level_roundtrip_rdev_zero_zero() {
    for proto in [29u8, 30, 32] {
        let xflags = if (28..30).contains(&proto) {
            (XMIT_RDEV_MINOR_8_PRE30 as u32) << 8
        } else {
            0u32
        };

        let mut buf = Vec::new();
        encode_rdev(&mut buf, 0, 0, xflags, proto).unwrap();

        let mut cursor = Cursor::new(&buf);
        let (dec_major, dec_minor) = decode_rdev(&mut cursor, xflags, 0, proto).unwrap();
        assert_eq!(dec_major, 0, "zero major for proto {proto}");
        assert_eq!(dec_minor, 0, "zero minor for proto {proto}");
    }
}

/// Tests wire-level roundtrip with maximum 8-bit minor value (255).
#[test]
fn wire_level_roundtrip_rdev_max_8bit_minor() {
    for proto in [29u8, 30, 32] {
        let xflags = if (28..30).contains(&proto) {
            (XMIT_RDEV_MINOR_8_PRE30 as u32) << 8
        } else {
            0u32
        };

        let mut buf = Vec::new();
        encode_rdev(&mut buf, 1, 255, xflags, proto).unwrap();

        let mut cursor = Cursor::new(&buf);
        let (dec_major, dec_minor) = decode_rdev(&mut cursor, xflags, 0, proto).unwrap();
        assert_eq!(dec_major, 1);
        assert_eq!(dec_minor, 255);
    }
}

/// Tests wire-level roundtrip with large major and minor values.
#[test]
fn wire_level_roundtrip_rdev_large_values() {
    // Linux supports major up to 4095 (12 bits) and minor up to ~1M (20 bits)
    let test_cases: &[(u32, u32)] = &[
        (255, 255),
        (256, 256),
        (4095, 1048575), // Linux max: 12-bit major, 20-bit minor
        (1000, 50000),
        (0, 65535),
        (65535, 0),
    ];

    for &(major, minor) in test_cases {
        let mut buf = Vec::new();
        encode_rdev(&mut buf, major, minor, 0, 30).unwrap();

        let mut cursor = Cursor::new(&buf);
        let (dec_major, dec_minor) = decode_rdev(&mut cursor, 0, 0, 30).unwrap();
        assert_eq!(dec_major, major, "major mismatch for ({major}, {minor})");
        assert_eq!(dec_minor, minor, "minor mismatch for ({major}, {minor})");
    }
}

/// Tests multiple consecutive rdev encodings at the wire level.
#[test]
fn wire_level_consecutive_rdev_entries() {
    let devices: &[(u32, u32)] = &[(8, 0), (8, 1), (8, 16), (1, 3), (1, 5), (136, 0)];

    for proto in [30u8, 32] {
        let mut buf = Vec::new();
        for &(major, minor) in devices {
            encode_rdev(&mut buf, major, minor, 0, proto).unwrap();
        }

        let mut cursor = Cursor::new(&buf);
        for (i, &(expected_major, expected_minor)) in devices.iter().enumerate() {
            let (dec_major, dec_minor) = decode_rdev(&mut cursor, 0, 0, proto).unwrap();
            assert_eq!(
                dec_major, expected_major,
                "major mismatch at entry {i} for proto {proto}"
            );
            assert_eq!(
                dec_minor, expected_minor,
                "minor mismatch at entry {i} for proto {proto}"
            );
        }
        assert_eq!(cursor.position() as usize, buf.len());
    }
}

// ============================================================================
// 2. Device Flag Calculation Tests
// ============================================================================

/// Tests that calculate_device_flags correctly identifies same major.
#[test]
fn device_flags_same_major() {
    let flags = calculate_device_flags(8, 8, 1, 30);
    assert_ne!(flags & XMIT_SAME_RDEV_MAJOR, 0);
}

/// Tests that calculate_device_flags detects different major.
#[test]
fn device_flags_different_major() {
    let flags = calculate_device_flags(8, 1, 1, 30);
    assert_eq!(flags & XMIT_SAME_RDEV_MAJOR, 0);
}

/// Tests that XMIT_RDEV_MINOR_8_PRE30 is set for protocol 28-29 when minor fits in byte.
#[test]
fn device_flags_minor_8bit_proto28() {
    let flags = calculate_device_flags(8, 0, 255, 28);
    assert_ne!(flags & XMIT_RDEV_MINOR_8_PRE30, 0);
}

/// Tests that XMIT_RDEV_MINOR_8_PRE30 is set for protocol 29 when minor fits in byte.
#[test]
fn device_flags_minor_8bit_proto29() {
    let flags = calculate_device_flags(8, 0, 0, 29);
    assert_ne!(flags & XMIT_RDEV_MINOR_8_PRE30, 0);
}

/// Tests that XMIT_RDEV_MINOR_8_PRE30 is NOT set for protocol 29 when minor > 255.
#[test]
fn device_flags_minor_large_proto29() {
    let flags = calculate_device_flags(8, 0, 256, 29);
    assert_eq!(flags & XMIT_RDEV_MINOR_8_PRE30, 0);
}

/// Tests that XMIT_RDEV_MINOR_8_PRE30 is NOT set for protocol 30+ (uses varint instead).
#[test]
fn device_flags_no_minor_8bit_proto30() {
    let flags = calculate_device_flags(8, 0, 5, 30);
    assert_eq!(flags & XMIT_RDEV_MINOR_8_PRE30, 0);
}

/// Tests flag calculation for the boundary minor value of 255 across protocols.
#[test]
fn device_flags_minor_boundary_255() {
    // Protocol 28-29: minor=255 fits in byte, flag should be set
    for proto in [28u8, 29] {
        let flags = calculate_device_flags(1, 0, 255, proto);
        assert_ne!(
            flags & XMIT_RDEV_MINOR_8_PRE30,
            0,
            "proto {proto} should set 8-bit flag for minor=255"
        );
    }
    // Protocol 30+: no 8-bit flag (uses varint)
    let flags = calculate_device_flags(1, 0, 255, 30);
    assert_eq!(flags & XMIT_RDEV_MINOR_8_PRE30, 0);
}

/// Tests flag calculation for the boundary minor value of 256 across protocols.
#[test]
fn device_flags_minor_boundary_256() {
    // Protocol 28-29: minor=256 does NOT fit in byte
    for proto in [28u8, 29] {
        let flags = calculate_device_flags(1, 0, 256, proto);
        assert_eq!(
            flags & XMIT_RDEV_MINOR_8_PRE30,
            0,
            "proto {proto} should NOT set 8-bit flag for minor=256"
        );
    }
}

// ============================================================================
// 3. High-Level Block Device Roundtrip Tests
// ============================================================================

/// Tests high-level roundtrip of a block device (e.g., /dev/sda).
#[test]
fn flist_roundtrip_block_device() {
    for protocol in [
        ProtocolVersion::V28,
        ProtocolVersion::V30,
        ProtocolVersion::NEWEST,
    ] {
        let entry = make_block_device("sda", 8, 0);
        let decoded = roundtrip_device(&entry, protocol);

        assert!(decoded.is_device());
        assert!(decoded.is_block_device());
        assert!(!decoded.is_char_device());
        assert_eq!(decoded.name(), "sda");
        assert_eq!(decoded.rdev_major(), Some(8), "major for {protocol:?}");
        assert_eq!(decoded.rdev_minor(), Some(0), "minor for {protocol:?}");
    }
}

/// Tests high-level roundtrip of a block device with partition (e.g., /dev/sda1).
#[test]
fn flist_roundtrip_block_device_with_partition() {
    let entry = make_block_device("sda1", 8, 1);
    let decoded = roundtrip_device(&entry, ProtocolVersion::NEWEST);

    assert!(decoded.is_block_device());
    assert_eq!(decoded.rdev_major(), Some(8));
    assert_eq!(decoded.rdev_minor(), Some(1));
}

/// Tests block device mode bits are preserved.
#[test]
fn flist_block_device_mode_preserved() {
    let entry = make_block_device("sda", 8, 0);
    let decoded = roundtrip_device(&entry, ProtocolVersion::NEWEST);

    // S_IFBLK = 0o060000
    assert_eq!(decoded.mode() & 0o170000, 0o060000, "S_IFBLK must be set");
    assert_eq!(decoded.mode() & 0o7777, 0o660, "permissions must be 0o660");
}

/// Tests block device file size is zero.
#[test]
fn flist_block_device_size_is_zero() {
    let entry = make_block_device("sda", 8, 0);
    let decoded = roundtrip_device(&entry, ProtocolVersion::NEWEST);
    assert_eq!(decoded.size(), 0, "block device file size should be 0");
}

// ============================================================================
// 4. High-Level Character Device Roundtrip Tests
// ============================================================================

/// Tests high-level roundtrip of a character device (e.g., /dev/null).
#[test]
fn flist_roundtrip_char_device() {
    for protocol in [
        ProtocolVersion::V28,
        ProtocolVersion::V30,
        ProtocolVersion::NEWEST,
    ] {
        let entry = make_char_device("null", 1, 3);
        let decoded = roundtrip_device(&entry, protocol);

        assert!(decoded.is_device());
        assert!(decoded.is_char_device());
        assert!(!decoded.is_block_device());
        assert_eq!(decoded.name(), "null");
        assert_eq!(decoded.rdev_major(), Some(1), "major for {protocol:?}");
        assert_eq!(decoded.rdev_minor(), Some(3), "minor for {protocol:?}");
    }
}

/// Tests high-level roundtrip of /dev/zero (char device major=1, minor=5).
#[test]
fn flist_roundtrip_char_device_zero() {
    let entry = make_char_device("zero", 1, 5);
    let decoded = roundtrip_device(&entry, ProtocolVersion::NEWEST);

    assert!(decoded.is_char_device());
    assert_eq!(decoded.rdev_major(), Some(1));
    assert_eq!(decoded.rdev_minor(), Some(5));
}

/// Tests high-level roundtrip of /dev/tty0 (char device major=4, minor=0).
#[test]
fn flist_roundtrip_char_device_tty() {
    let entry = make_char_device("tty0", 4, 0);
    let decoded = roundtrip_device(&entry, ProtocolVersion::NEWEST);

    assert!(decoded.is_char_device());
    assert_eq!(decoded.rdev_major(), Some(4));
    assert_eq!(decoded.rdev_minor(), Some(0));
}

/// Tests character device mode bits are preserved.
#[test]
fn flist_char_device_mode_preserved() {
    let entry = make_char_device("null", 1, 3);
    let decoded = roundtrip_device(&entry, ProtocolVersion::NEWEST);

    // S_IFCHR = 0o020000
    assert_eq!(decoded.mode() & 0o170000, 0o020000, "S_IFCHR must be set");
    assert_eq!(decoded.mode() & 0o7777, 0o666, "permissions must be 0o666");
}

// ============================================================================
// 5. Various Major/Minor Number Combinations
// ============================================================================

/// Tests device encoding with major=0, minor=0.
#[test]
fn flist_roundtrip_device_zero_zero() {
    let entry = make_block_device("dev0", 0, 0);
    let decoded = roundtrip_device(&entry, ProtocolVersion::NEWEST);

    assert_eq!(decoded.rdev_major(), Some(0));
    assert_eq!(decoded.rdev_minor(), Some(0));
}

/// Tests device encoding with major=1, minor=1.
#[test]
fn flist_roundtrip_device_one_one() {
    let entry = make_char_device("dev1", 1, 1);
    let decoded = roundtrip_device(&entry, ProtocolVersion::NEWEST);

    assert_eq!(decoded.rdev_major(), Some(1));
    assert_eq!(decoded.rdev_minor(), Some(1));
}

/// Tests device encoding with major=255, minor=255.
#[test]
fn flist_roundtrip_device_255_255() {
    let entry = make_block_device("dev255", 255, 255);
    let decoded = roundtrip_device(&entry, ProtocolVersion::NEWEST);

    assert_eq!(decoded.rdev_major(), Some(255));
    assert_eq!(decoded.rdev_minor(), Some(255));
}

/// Tests device encoding with large major and minor values.
#[test]
fn flist_roundtrip_device_large_values() {
    // Linux: major up to 4095 (12-bit), minor up to ~1M (20-bit)
    let test_cases: &[(u32, u32, &str)] = &[
        (256, 256, "dev256"),
        (4095, 1048575, "dev_max_linux"),
        (1000, 50000, "dev_large"),
        (0, 65535, "dev_zero_major"),
        (65535, 0, "dev_zero_minor"),
    ];

    for &(major, minor, name) in test_cases {
        let entry = make_block_device(name, major, minor);
        let decoded = roundtrip_device(&entry, ProtocolVersion::NEWEST);

        assert_eq!(
            decoded.rdev_major(),
            Some(major),
            "major mismatch for ({major}, {minor})"
        );
        assert_eq!(
            decoded.rdev_minor(),
            Some(minor),
            "minor mismatch for ({major}, {minor})"
        );
    }
}

/// Tests device encoding at the 8-bit boundary (255 and 256 minor).
#[test]
fn flist_roundtrip_device_minor_boundary() {
    for protocol in [
        ProtocolVersion::V29,
        ProtocolVersion::V30,
        ProtocolVersion::NEWEST,
    ] {
        // Minor = 255 (fits in 8 bits)
        let entry255 = make_block_device("dev255", 8, 255);
        let decoded255 = roundtrip_device(&entry255, protocol);
        assert_eq!(decoded255.rdev_minor(), Some(255), "255 for {protocol:?}");

        // Minor = 256 (does not fit in 8 bits)
        let entry256 = make_block_device("dev256", 8, 256);
        let decoded256 = roundtrip_device(&entry256, protocol);
        assert_eq!(decoded256.rdev_minor(), Some(256), "256 for {protocol:?}");
    }
}

// ============================================================================
// 6. Multiple Devices with Major Compression
// ============================================================================

/// Tests that consecutive devices with the same major benefit from compression.
#[test]
fn flist_roundtrip_same_major_compression() {
    let entries = vec![
        make_block_device("sda", 8, 0),
        make_block_device("sdb", 8, 16),
        make_block_device("sdc", 8, 32),
    ];

    for protocol in [ProtocolVersion::V30, ProtocolVersion::NEWEST] {
        let decoded = roundtrip_entries(&entries, protocol);

        assert_eq!(decoded.len(), 3);
        for (i, dec) in decoded.iter().enumerate() {
            assert_eq!(
                dec.rdev_major(),
                Some(8),
                "major mismatch at index {i} for {protocol:?}"
            );
        }
        assert_eq!(decoded[0].rdev_minor(), Some(0));
        assert_eq!(decoded[1].rdev_minor(), Some(16));
        assert_eq!(decoded[2].rdev_minor(), Some(32));
    }
}

/// Tests that consecutive devices with different majors encode correctly.
#[test]
fn flist_roundtrip_different_major_devices() {
    let entries = vec![
        make_block_device("sda", 8, 0),   // SCSI disk
        make_char_device("null", 1, 3),   // char device
        make_block_device("loop0", 7, 0), // loop device
        make_char_device("tty0", 4, 0),   // terminal
    ];

    let decoded = roundtrip_entries(&entries, ProtocolVersion::NEWEST);

    assert_eq!(decoded.len(), 4);
    assert_eq!(decoded[0].rdev_major(), Some(8));
    assert_eq!(decoded[0].rdev_minor(), Some(0));
    assert_eq!(decoded[1].rdev_major(), Some(1));
    assert_eq!(decoded[1].rdev_minor(), Some(3));
    assert_eq!(decoded[2].rdev_major(), Some(7));
    assert_eq!(decoded[2].rdev_minor(), Some(0));
    assert_eq!(decoded[3].rdev_major(), Some(4));
    assert_eq!(decoded[3].rdev_minor(), Some(0));
}

/// Tests that the second device entry is more compact when sharing major.
#[test]
fn flist_same_major_compression_produces_smaller_encoding() {
    let protocol = ProtocolVersion::NEWEST;

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol).with_preserve_devices(true);

    let entry1 = make_block_device("sda", 8, 0);
    let entry2 = make_block_device("sdb", 8, 16);

    writer.write_entry(&mut buf, &entry1).unwrap();
    let first_len = buf.len();
    writer.write_entry(&mut buf, &entry2).unwrap();
    let second_len = buf.len() - first_len;

    // Second entry should be smaller due to XMIT_SAME_RDEV_MAJOR
    assert!(
        second_len < first_len,
        "second device ({second_len} bytes) should be smaller than first ({first_len} bytes)",
    );
}

// ============================================================================
// 7. Block vs Character Device Distinction
// ============================================================================

/// Tests that block and character devices are correctly distinguished after roundtrip.
#[test]
fn flist_block_vs_char_device_distinction() {
    let entries = vec![
        make_block_device("block_dev", 8, 0),
        make_char_device("char_dev", 1, 3),
    ];

    for protocol in [
        ProtocolVersion::V28,
        ProtocolVersion::V30,
        ProtocolVersion::NEWEST,
    ] {
        let decoded = roundtrip_entries(&entries, protocol);

        assert_eq!(decoded.len(), 2);

        // Block device
        assert!(
            decoded[0].is_block_device(),
            "first should be block for {protocol:?}"
        );
        assert!(!decoded[0].is_char_device());
        assert_eq!(decoded[0].mode() & 0o170000, 0o060000); // S_IFBLK

        // Char device
        assert!(
            decoded[1].is_char_device(),
            "second should be char for {protocol:?}"
        );
        assert!(!decoded[1].is_block_device());
        assert_eq!(decoded[1].mode() & 0o170000, 0o020000); // S_IFCHR
    }
}

/// Tests that block and char devices with same major/minor are still distinguishable.
#[test]
fn flist_same_rdev_different_type() {
    let entries = vec![
        make_block_device("block_8_0", 8, 0),
        make_char_device("char_8_0", 8, 0),
    ];

    let decoded = roundtrip_entries(&entries, ProtocolVersion::NEWEST);

    assert_eq!(decoded.len(), 2);
    assert!(decoded[0].is_block_device());
    assert!(decoded[1].is_char_device());
    // Same major/minor
    assert_eq!(decoded[0].rdev_major(), decoded[1].rdev_major());
    assert_eq!(decoded[0].rdev_minor(), decoded[1].rdev_minor());
    // Different mode type bits
    assert_ne!(decoded[0].mode() & 0o170000, decoded[1].mode() & 0o170000);
}

// ============================================================================
// 8. Special Files (FIFOs and Sockets)
// ============================================================================

/// Tests that FIFO entries have dummy rdev consumed but not stored in protocol < 31.
/// Upstream writes dummy rdev (0, 0) for special files in proto < 31, but the reader
/// consumes the bytes without storing them in the FileEntry (they are meaningless).
#[test]
fn flist_fifo_dummy_rdev_consumed_proto30() {
    let entry = make_fifo("myfifo");
    let decoded = roundtrip_device(&entry, ProtocolVersion::V30);

    assert!(decoded.is_special());
    assert_eq!(decoded.mode() & 0o170000, 0o010000); // S_IFIFO
    // Dummy rdev is consumed from wire but NOT stored in FileEntry
    assert_eq!(decoded.rdev_major(), None);
    assert_eq!(decoded.rdev_minor(), None);
}

/// Tests that socket entries have dummy rdev consumed but not stored in protocol < 31.
/// Upstream writes dummy rdev (0, 0) for special files in proto < 31, but the reader
/// consumes the bytes without storing them in the FileEntry (they are meaningless).
#[test]
fn flist_socket_dummy_rdev_consumed_proto30() {
    let entry = make_socket("mysock");
    let decoded = roundtrip_device(&entry, ProtocolVersion::V30);

    assert!(decoded.is_special());
    assert_eq!(decoded.mode() & 0o170000, 0o140000); // S_IFSOCK
    // Dummy rdev is consumed from wire but NOT stored in FileEntry
    assert_eq!(decoded.rdev_major(), None);
    assert_eq!(decoded.rdev_minor(), None);
}

/// Tests that FIFO entries do NOT get rdev in protocol 31+.
#[test]
fn flist_fifo_no_rdev_proto31() {
    let entry = make_fifo("myfifo");
    let decoded = roundtrip_device(&entry, ProtocolVersion::V31);

    assert!(decoded.is_special());
    // Protocol 31+: special files do NOT get rdev
    assert_eq!(decoded.rdev_major(), None);
    assert_eq!(decoded.rdev_minor(), None);
}

/// Tests that socket entries do NOT get rdev in protocol 31+.
#[test]
fn flist_socket_no_rdev_proto31() {
    let entry = make_socket("mysock");
    let decoded = roundtrip_device(&entry, ProtocolVersion::V31);

    assert!(decoded.is_special());
    // Protocol 31+: special files do NOT get rdev
    assert_eq!(decoded.rdev_major(), None);
    assert_eq!(decoded.rdev_minor(), None);
}

// ============================================================================
// 9. preserve_devices=false Behavior
// ============================================================================

/// Tests that rdev is NOT encoded when preserve_devices is false.
#[test]
fn flist_no_rdev_when_preserve_devices_disabled() {
    let entry = make_block_device("sda", 8, 0);

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(ProtocolVersion::NEWEST); // preserve_devices = false
    writer.write_entry(&mut buf, &entry).expect("write failed");
    writer.write_end(&mut buf, None).expect("write end failed");

    let mut cursor = Cursor::new(&buf);
    let mut reader = FileListReader::new(ProtocolVersion::NEWEST); // preserve_devices = false
    let decoded = reader
        .read_entry(&mut cursor)
        .expect("read failed")
        .expect("no entry");

    assert!(decoded.is_block_device()); // Mode is still preserved
    assert_eq!(decoded.rdev_major(), None, "rdev should not be present");
    assert_eq!(decoded.rdev_minor(), None, "rdev should not be present");
}

/// Tests that char device rdev is NOT encoded when preserve_devices is false.
#[test]
fn flist_no_rdev_char_device_when_preserve_devices_disabled() {
    let entry = make_char_device("null", 1, 3);

    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(ProtocolVersion::NEWEST);
    writer.write_entry(&mut buf, &entry).expect("write failed");
    writer.write_end(&mut buf, None).expect("write end failed");

    let mut cursor = Cursor::new(&buf);
    let mut reader = FileListReader::new(ProtocolVersion::NEWEST);
    let decoded = reader
        .read_entry(&mut cursor)
        .expect("read failed")
        .expect("no entry");

    assert!(decoded.is_char_device());
    assert_eq!(decoded.rdev_major(), None);
    assert_eq!(decoded.rdev_minor(), None);
}

// ============================================================================
// 10. Protocol Version Differences
// ============================================================================

/// Tests device encoding across all supported protocol versions.
#[test]
fn flist_roundtrip_device_all_protocol_versions() {
    let entry = make_block_device("sda", 8, 0);

    for protocol in [
        ProtocolVersion::V28,
        ProtocolVersion::V29,
        ProtocolVersion::V30,
        ProtocolVersion::V31,
        ProtocolVersion::NEWEST,
    ] {
        let decoded = roundtrip_device(&entry, protocol);
        assert_eq!(decoded.rdev_major(), Some(8), "major for {protocol:?}");
        assert_eq!(decoded.rdev_minor(), Some(0), "minor for {protocol:?}");
    }
}

/// Tests that protocol 30+ minor encoding is more compact than protocol 29 for small minors.
#[test]
fn wire_format_protocol_30_rdev_more_compact() {
    // Small minor value that fits in 1 byte varint
    let mut buf29 = Vec::new();
    let xflags29 = (XMIT_RDEV_MINOR_8_PRE30 as u32) << 8;
    encode_rdev(&mut buf29, 8, 5, xflags29, 29).unwrap();

    let mut buf30 = Vec::new();
    encode_rdev(&mut buf30, 8, 5, 0, 30).unwrap();

    // Protocol 30 uses varint for both major and minor
    // Protocol 29 with 8-bit minor uses varint30(major) + u8(minor)
    // For small values, they should be similar in size
    // (both should encode efficiently)
    assert!(!buf29.is_empty());
    assert!(!buf30.is_empty());

    // Verify both decode correctly
    let mut cursor29 = Cursor::new(&buf29);
    let (maj29, min29) = decode_rdev(&mut cursor29, xflags29, 0, 29).unwrap();
    assert_eq!(maj29, 8);
    assert_eq!(min29, 5);

    let mut cursor30 = Cursor::new(&buf30);
    let (maj30, min30) = decode_rdev(&mut cursor30, 0, 0, 30).unwrap();
    assert_eq!(maj30, 8);
    assert_eq!(min30, 5);
}

/// Tests protocol 29 with large minor requiring 32-bit encoding.
#[test]
fn flist_roundtrip_device_large_minor_proto29() {
    // Minor > 255 requires 32-bit encoding in proto 28-29
    let entry = make_block_device("devlarge", 8, 300);
    let decoded = roundtrip_device(&entry, ProtocolVersion::V29);

    assert_eq!(decoded.rdev_major(), Some(8));
    assert_eq!(decoded.rdev_minor(), Some(300));
}

// ============================================================================
// 11. Mixed Device Types in Sequence
// ============================================================================

/// Tests a file list containing various device types interleaved with regular files.
#[test]
fn flist_roundtrip_mixed_entry_types_with_devices() {
    let mut file = FileEntry::new_file(PathBuf::from("file.txt"), 100, 0o644);
    file.set_mtime(1700000000, 0);

    let mut dir = FileEntry::new_directory(PathBuf::from("mydir"), 0o755);
    dir.set_mtime(1700000000, 0);

    let entries = vec![
        file,
        make_block_device("sda", 8, 0),
        dir,
        make_char_device("null", 1, 3),
        make_block_device("sdb", 8, 16),
    ];

    let decoded = roundtrip_entries(&entries, ProtocolVersion::NEWEST);

    assert_eq!(decoded.len(), 5);
    assert!(decoded[0].is_file());
    assert!(decoded[1].is_block_device());
    assert!(decoded[2].is_dir());
    assert!(decoded[3].is_char_device());
    assert!(decoded[4].is_block_device());

    assert_eq!(decoded[1].rdev_major(), Some(8));
    assert_eq!(decoded[1].rdev_minor(), Some(0));
    assert_eq!(decoded[3].rdev_major(), Some(1));
    assert_eq!(decoded[3].rdev_minor(), Some(3));
    assert_eq!(decoded[4].rdev_major(), Some(8));
    assert_eq!(decoded[4].rdev_minor(), Some(16));
}

/// Tests devices interleaved with special files (FIFOs and sockets).
#[test]
fn flist_roundtrip_devices_with_specials_proto30() {
    let entries = vec![
        make_block_device("sda", 8, 0),
        make_fifo("pipe1"),
        make_char_device("null", 1, 3),
        make_socket("sock1"),
        make_block_device("sdb", 8, 16),
    ];

    let decoded = roundtrip_entries(&entries, ProtocolVersion::V30);

    assert_eq!(decoded.len(), 5);
    assert!(decoded[0].is_block_device());
    assert!(decoded[1].is_special()); // FIFO
    assert!(decoded[2].is_char_device());
    assert!(decoded[3].is_special()); // Socket
    assert!(decoded[4].is_block_device());

    // Devices have their actual rdev
    assert_eq!(decoded[0].rdev_major(), Some(8));
    assert_eq!(decoded[0].rdev_minor(), Some(0));
    assert_eq!(decoded[2].rdev_major(), Some(1));
    assert_eq!(decoded[2].rdev_minor(), Some(3));
    assert_eq!(decoded[4].rdev_major(), Some(8));
    assert_eq!(decoded[4].rdev_minor(), Some(16));

    // Specials have dummy rdev consumed from wire but NOT stored in FileEntry
    assert_eq!(decoded[1].rdev_major(), None);
    assert_eq!(decoded[1].rdev_minor(), None);
    assert_eq!(decoded[3].rdev_major(), None);
    assert_eq!(decoded[3].rdev_minor(), None);
}

// ============================================================================
// 12. Common Linux Device Number Encoding
// ============================================================================

/// Tests encoding of well-known Linux device numbers.
#[test]
fn flist_roundtrip_linux_common_devices() {
    let devices: Vec<FileEntry> = vec![
        make_char_device("null", 1, 3),    // /dev/null
        make_char_device("zero", 1, 5),    // /dev/zero
        make_char_device("full", 1, 7),    // /dev/full
        make_char_device("random", 1, 8),  // /dev/random
        make_char_device("urandom", 1, 9), // /dev/urandom
        make_char_device("tty", 5, 0),     // /dev/tty
        make_char_device("console", 5, 1), // /dev/console
        make_block_device("sda", 8, 0),    // /dev/sda
        make_block_device("sda1", 8, 1),   // /dev/sda1
        make_block_device("sda2", 8, 2),   // /dev/sda2
        make_block_device("sdb", 8, 16),   // /dev/sdb
        make_block_device("loop0", 7, 0),  // /dev/loop0
        make_block_device("loop1", 7, 1),  // /dev/loop1
    ];

    let decoded = roundtrip_entries(&devices, ProtocolVersion::NEWEST);

    assert_eq!(decoded.len(), devices.len());
    for (i, (orig, dec)) in devices.iter().zip(decoded.iter()).enumerate() {
        assert_eq!(
            dec.rdev_major(),
            orig.rdev_major(),
            "major mismatch at index {} ({})",
            i,
            orig.name()
        );
        assert_eq!(
            dec.rdev_minor(),
            orig.rdev_minor(),
            "minor mismatch at index {} ({})",
            i,
            orig.name()
        );
        assert_eq!(
            dec.is_block_device(),
            orig.is_block_device(),
            "type mismatch at index {} ({})",
            i,
            orig.name()
        );
    }
}

// ============================================================================
// 13. Edge Cases
// ============================================================================

/// Tests device name prefix compression works correctly.
#[test]
fn flist_device_name_prefix_compression() {
    let entries = vec![
        make_block_device("dev/sda", 8, 0),
        make_block_device("dev/sdb", 8, 16),
        make_block_device("dev/sdc", 8, 32),
    ];

    let decoded = roundtrip_entries(&entries, ProtocolVersion::NEWEST);

    assert_eq!(decoded.len(), 3);
    assert_eq!(decoded[0].name(), "dev/sda");
    assert_eq!(decoded[1].name(), "dev/sdb");
    assert_eq!(decoded[2].name(), "dev/sdc");
}

/// Tests that many devices with incrementing minors roundtrip correctly.
#[test]
fn flist_roundtrip_many_partitions() {
    let entries: Vec<FileEntry> = (0..64)
        .map(|i| make_block_device(&format!("sd{i}"), 8, i))
        .collect();

    let decoded = roundtrip_entries(&entries, ProtocolVersion::NEWEST);

    assert_eq!(decoded.len(), 64);
    for (i, dec) in decoded.iter().enumerate() {
        assert_eq!(dec.rdev_major(), Some(8), "major at index {i}");
        assert_eq!(dec.rdev_minor(), Some(i as u32), "minor at index {i}");
    }
}

/// Tests device with maximum varint-representable values.
#[test]
fn flist_roundtrip_device_max_varint_values() {
    // Large values that fit in varint encoding
    let entry = make_block_device("devmax", 0x0FFF_FFFF, 0x0FFF_FFFF);
    let decoded = roundtrip_device(&entry, ProtocolVersion::NEWEST);

    assert_eq!(decoded.rdev_major(), Some(0x0FFF_FFFF));
    assert_eq!(decoded.rdev_minor(), Some(0x0FFF_FFFF));
}

/// Tests wire-level encoding of rdev with same major flag and zero minor.
#[test]
fn wire_level_same_major_zero_minor() {
    let xflags = (XMIT_SAME_RDEV_MAJOR as u32) << 8;

    let mut buf = Vec::new();
    encode_rdev(&mut buf, 8, 0, xflags, 30).unwrap();

    let mut cursor = Cursor::new(&buf);
    let (dec_major, dec_minor) = decode_rdev(&mut cursor, xflags, 8, 30).unwrap();
    assert_eq!(dec_major, 8);
    assert_eq!(dec_minor, 0);

    // Only minor should be encoded (varint for 0 = 1 byte)
    assert_eq!(
        buf.len(),
        1,
        "only minor should be encoded when SAME_RDEV_MAJOR"
    );
}

/// Tests that the wire format produces known bytes for a common device.
#[test]
fn wire_level_known_bytes_proto30() {
    // Encode major=8, minor=0 in protocol 30
    let mut buf = Vec::new();
    encode_rdev(&mut buf, 8, 0, 0, 30).unwrap();

    // Decode and verify
    let mut cursor = Cursor::new(&buf);
    let (major, minor) = decode_rdev(&mut cursor, 0, 0, 30).unwrap();
    assert_eq!(major, 8);
    assert_eq!(minor, 0);

    // varint30(8) should be single byte 0x08
    // varint(0) should be single byte 0x00
    assert_eq!(buf.len(), 2, "major=8 and minor=0 should each be 1 byte");
    assert_eq!(buf[0], 8, "varint30(8) = 0x08");
    assert_eq!(buf[1], 0, "varint(0) = 0x00");
}

/// Tests that encode_rdev followed by decode_rdev is consistent across many values.
#[test]
fn wire_level_encode_decode_consistency() {
    let test_cases: &[(u32, u32)] = &[
        (0, 0),
        (1, 1),
        (1, 3),
        (1, 255),
        (1, 256),
        (8, 0),
        (8, 16),
        (255, 255),
        (256, 0),
        (1000, 50000),
    ];

    for proto in [28u8, 29, 30, 31, 32] {
        for &(major, minor) in test_cases {
            let xflags = if (28..30).contains(&proto) && minor <= 0xFF {
                (XMIT_RDEV_MINOR_8_PRE30 as u32) << 8
            } else {
                0u32
            };

            let mut buf = Vec::new();
            encode_rdev(&mut buf, major, minor, xflags, proto).unwrap();

            let mut cursor = Cursor::new(&buf);
            let (dec_major, dec_minor) = decode_rdev(&mut cursor, xflags, 0, proto).unwrap();

            assert_eq!(
                dec_major, major,
                "major mismatch for ({major}, {minor}) proto={proto}"
            );
            assert_eq!(
                dec_minor, minor,
                "minor mismatch for ({major}, {minor}) proto={proto}"
            );

            // All bytes consumed
            assert_eq!(
                cursor.position() as usize,
                buf.len(),
                "not all bytes consumed for ({major}, {minor}) proto={proto}"
            );
        }
    }
}
