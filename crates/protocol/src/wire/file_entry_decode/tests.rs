#![deny(unsafe_code)]

use super::*;
use crate::varint::read_varint;
use crate::wire::file_entry::{
    XMIT_CRTIME_EQ_MTIME, XMIT_GROUP_NAME_FOLLOWS, XMIT_HLINK_FIRST, XMIT_HLINKED, XMIT_LONG_NAME,
    XMIT_MOD_NSEC, XMIT_RDEV_MINOR_8_PRE30, XMIT_SAME_ATIME, XMIT_SAME_DEV_PRE30, XMIT_SAME_MODE,
    XMIT_SAME_NAME, XMIT_SAME_RDEV_MAJOR, XMIT_SAME_TIME, XMIT_SAME_UID, XMIT_USER_NAME_FOLLOWS,
    encode_atime, encode_checksum, encode_crtime, encode_end_marker, encode_flags, encode_gid,
    encode_hardlink_dev_ino, encode_hardlink_idx, encode_mode, encode_mtime, encode_mtime_nsec,
    encode_name, encode_owner_name, encode_rdev, encode_size, encode_symlink_target, encode_uid,
};
use std::io::{self, Cursor};

#[test]
fn decode_flags_single_byte() {
    let mut buf = Vec::new();
    encode_flags(&mut buf, XMIT_SAME_MODE as u32, 32, false, false).unwrap();

    let mut cursor = Cursor::new(&buf);
    let (flags, is_end) = decode_flags(&mut cursor, 32, false).unwrap();
    assert_eq!(flags, XMIT_SAME_MODE as u32);
    assert!(!is_end);
}

#[test]
fn decode_flags_two_bytes_protocol_28() {
    let mut buf = Vec::new();
    let xflags = (XMIT_HLINKED as u32) << 8;
    encode_flags(&mut buf, xflags, 28, false, false).unwrap();

    let mut cursor = Cursor::new(&buf);
    let (flags, is_end) = decode_flags(&mut cursor, 28, false).unwrap();
    assert!(!is_end);
    assert!(flags & ((XMIT_HLINKED as u32) << 8) != 0);
}

#[test]
fn decode_flags_varint_mode() {
    let mut buf = Vec::new();
    encode_flags(&mut buf, 0x123, 32, true, false).unwrap();

    let mut cursor = Cursor::new(&buf);
    let (flags, is_end) = decode_flags(&mut cursor, 32, true).unwrap();
    assert_eq!(flags, 0x123);
    assert!(!is_end);
}

#[test]
fn decode_flags_end_marker_varint() {
    let mut buf = Vec::new();
    encode_end_marker(&mut buf, true, false, None).unwrap();

    let mut cursor = Cursor::new(&buf);
    let (flags, is_end) = decode_flags(&mut cursor, 32, true).unwrap();
    assert!(is_end);
    assert_eq!(flags, 0);
}

#[test]
fn decode_flags_end_marker_normal() {
    let data = vec![0u8];
    let mut cursor = Cursor::new(&data);
    let (flags, is_end) = decode_flags(&mut cursor, 32, false).unwrap();
    assert_eq!(flags, 0);
    assert!(is_end);
}

#[test]
fn roundtrip_end_marker_simple() {
    let mut buf = Vec::new();
    encode_end_marker(&mut buf, false, false, None).unwrap();

    let mut cursor = Cursor::new(&buf);
    let error = decode_end_marker(&mut cursor, false, false, 0).unwrap();
    assert_eq!(error, None);
}

#[test]
fn roundtrip_end_marker_varint() {
    let mut buf = Vec::new();
    encode_end_marker(&mut buf, true, false, None).unwrap();

    let mut cursor = Cursor::new(&buf);
    let (_, _) = decode_flags(&mut cursor, 32, true).unwrap();
    let error = read_varint(&mut cursor).unwrap();
    assert_eq!(error, 0);
}

#[test]
fn roundtrip_end_marker_varint_with_error() {
    let mut buf = Vec::new();
    encode_end_marker(&mut buf, true, false, Some(23)).unwrap();

    let mut cursor = Cursor::new(&buf);
    let (flags, is_end) = decode_flags(&mut cursor, 32, true).unwrap();
    assert!(is_end);
    assert_eq!(flags, 0);
    let error = decode_end_marker(&mut cursor, true, false, 0).unwrap();
    assert_eq!(error, Some(23));
}

#[test]
fn roundtrip_end_marker_safe_file_list_with_error() {
    let mut buf = Vec::new();
    encode_end_marker(&mut buf, false, true, Some(42)).unwrap();

    let mut cursor = Cursor::new(&buf);
    let (flags, _) = decode_flags(&mut cursor, 31, false).unwrap();
    let error = decode_end_marker(&mut cursor, false, true, flags).unwrap();
    assert_eq!(error, Some(42));
}

#[test]
fn roundtrip_name_no_compression() {
    let mut buf = Vec::new();
    encode_name(&mut buf, b"test.txt", 0, 0, 32).unwrap();

    let mut cursor = Cursor::new(&buf);
    let name = decode_name(&mut cursor, 0, b"", 32).unwrap();
    assert_eq!(name, b"test.txt");
}

#[test]
fn roundtrip_name_with_compression() {
    let mut buf = Vec::new();
    encode_name(&mut buf, b"dir/file2.txt", 8, XMIT_SAME_NAME as u32, 32).unwrap();

    let mut cursor = Cursor::new(&buf);
    let name = decode_name(&mut cursor, XMIT_SAME_NAME as u32, b"dir/file1.txt", 32).unwrap();
    assert_eq!(name, b"dir/file2.txt");
}

#[test]
fn roundtrip_name_long_name_modern() {
    let mut buf = Vec::new();
    let long_name = vec![b'a'; 300];
    encode_name(&mut buf, &long_name, 0, XMIT_LONG_NAME as u32, 32).unwrap();

    let mut cursor = Cursor::new(&buf);
    let name = decode_name(&mut cursor, XMIT_LONG_NAME as u32, b"", 32).unwrap();
    assert_eq!(name, long_name);
}

#[test]
fn roundtrip_name_long_name_legacy() {
    let mut buf = Vec::new();
    let long_name = vec![b'a'; 300];
    encode_name(&mut buf, &long_name, 0, XMIT_LONG_NAME as u32, 29).unwrap();

    let mut cursor = Cursor::new(&buf);
    let name = decode_name(&mut cursor, XMIT_LONG_NAME as u32, b"", 29).unwrap();
    assert_eq!(name, long_name);
}

#[test]
fn roundtrip_size_modern() {
    let mut buf = Vec::new();
    encode_size(&mut buf, 1000, 32).unwrap();

    let mut cursor = Cursor::new(&buf);
    let size = decode_size(&mut cursor, 32).unwrap();
    assert_eq!(size, 1000);
}

#[test]
fn roundtrip_size_legacy() {
    let mut buf = Vec::new();
    encode_size(&mut buf, 1000, 29).unwrap();

    let mut cursor = Cursor::new(&buf);
    let size = decode_size(&mut cursor, 29).unwrap();
    assert_eq!(size, 1000);
}

#[test]
fn roundtrip_size_large_legacy() {
    let mut buf = Vec::new();
    let large = 0x1_0000_0000u64;
    encode_size(&mut buf, large, 29).unwrap();

    let mut cursor = Cursor::new(&buf);
    let size = decode_size(&mut cursor, 29).unwrap();
    assert_eq!(size, large as i64);
}

#[test]
fn roundtrip_mtime_modern() {
    let mut buf = Vec::new();
    encode_mtime(&mut buf, 1700000000, 32).unwrap();

    let mut cursor = Cursor::new(&buf);
    let mtime = decode_mtime(&mut cursor, 0, 0, 32).unwrap();
    assert_eq!(mtime, Some(1700000000));
}

#[test]
fn roundtrip_mtime_legacy() {
    let mut buf = Vec::new();
    encode_mtime(&mut buf, 1700000000, 29).unwrap();

    let mut cursor = Cursor::new(&buf);
    let mtime = decode_mtime(&mut cursor, 0, 0, 29).unwrap();
    assert_eq!(mtime, Some(1700000000));
}

#[test]
fn roundtrip_mtime_same_as_previous() {
    let mut cursor = Cursor::new(Vec::new());
    let mtime = decode_mtime(&mut cursor, XMIT_SAME_TIME as u32, 1600000000, 32).unwrap();
    assert_eq!(mtime, Some(1600000000));
}

#[test]
fn roundtrip_mtime_nsec() {
    let mut buf = Vec::new();
    encode_mtime_nsec(&mut buf, 123456789).unwrap();

    let mut cursor = Cursor::new(&buf);
    let flags = (XMIT_MOD_NSEC as u32) << 8;
    let nsec = decode_mtime_nsec(&mut cursor, flags).unwrap();
    assert_eq!(nsec, Some(123456789));
}

#[test]
fn roundtrip_atime() {
    let mut buf = Vec::new();
    encode_atime(&mut buf, 1700000001).unwrap();

    let mut cursor = Cursor::new(&buf);
    let atime = decode_atime(&mut cursor, 0, 0).unwrap();
    assert_eq!(atime, Some(1700000001));
}

#[test]
fn roundtrip_atime_same_as_previous() {
    let mut cursor = Cursor::new(Vec::new());
    let flags = (XMIT_SAME_ATIME as u32) << 8;
    let atime = decode_atime(&mut cursor, flags, 1600000000).unwrap();
    assert_eq!(atime, Some(1600000000));
}

#[test]
fn roundtrip_crtime() {
    let mut buf = Vec::new();
    encode_crtime(&mut buf, 1600000000).unwrap();

    let mut cursor = Cursor::new(&buf);
    let crtime = decode_crtime(&mut cursor, 0, 0).unwrap();
    assert_eq!(crtime, Some(1600000000));
}

#[test]
fn roundtrip_crtime_eq_mtime() {
    let mut cursor = Cursor::new(Vec::new());
    let flags = (XMIT_CRTIME_EQ_MTIME as u32) << 16;
    let crtime = decode_crtime(&mut cursor, flags, 1700000000).unwrap();
    assert_eq!(crtime, Some(1700000000));
}

#[test]
fn roundtrip_mode_regular_file() {
    let mut buf = Vec::new();
    encode_mode(&mut buf, 0o100644).unwrap();

    let mut cursor = Cursor::new(&buf);
    let mode = decode_mode(&mut cursor, 0, 0).unwrap();
    assert_eq!(mode, Some(0o100644));
}

#[test]
fn roundtrip_mode_same_as_previous() {
    let mut cursor = Cursor::new(Vec::new());
    let mode = decode_mode(&mut cursor, XMIT_SAME_MODE as u32, 0o100755).unwrap();
    assert_eq!(mode, Some(0o100755));
}

#[test]
fn roundtrip_uid_modern() {
    let mut buf = Vec::new();
    encode_uid(&mut buf, 1000, 32).unwrap();

    let mut cursor = Cursor::new(&buf);
    let result = decode_uid(&mut cursor, 0, 0, 32).unwrap();
    assert_eq!(result, Some((1000, None)));
}

#[test]
fn roundtrip_uid_legacy() {
    let mut buf = Vec::new();
    encode_uid(&mut buf, 1000, 29).unwrap();

    let mut cursor = Cursor::new(&buf);
    let result = decode_uid(&mut cursor, 0, 0, 29).unwrap();
    assert_eq!(result, Some((1000, None)));
}

#[test]
fn roundtrip_uid_same_as_previous() {
    let mut cursor = Cursor::new(Vec::new());
    let result = decode_uid(&mut cursor, XMIT_SAME_UID as u32, 500, 32).unwrap();
    assert_eq!(result, Some((500, None)));
}

#[test]
fn roundtrip_uid_with_name() {
    let mut buf = Vec::new();
    encode_uid(&mut buf, 1000, 32).unwrap();
    encode_owner_name(&mut buf, "testuser").unwrap();

    let mut cursor = Cursor::new(&buf);
    let flags = (XMIT_USER_NAME_FOLLOWS as u32) << 8;
    let result = decode_uid(&mut cursor, flags, 0, 32).unwrap();
    assert_eq!(result, Some((1000, Some("testuser".to_string()))));
}

#[test]
fn roundtrip_gid_modern() {
    let mut buf = Vec::new();
    encode_gid(&mut buf, 500, 30).unwrap();

    let mut cursor = Cursor::new(&buf);
    let result = decode_gid(&mut cursor, 0, 0, 30).unwrap();
    assert_eq!(result, Some((500, None)));
}

#[test]
fn roundtrip_gid_with_name() {
    let mut buf = Vec::new();
    encode_gid(&mut buf, 500, 32).unwrap();
    encode_owner_name(&mut buf, "testgroup").unwrap();

    let mut cursor = Cursor::new(&buf);
    let flags = (XMIT_GROUP_NAME_FOLLOWS as u32) << 8;
    let result = decode_gid(&mut cursor, flags, 0, 32).unwrap();
    assert_eq!(result, Some((500, Some("testgroup".to_string()))));
}

#[test]
fn roundtrip_rdev_protocol_30() {
    let mut buf = Vec::new();
    encode_rdev(&mut buf, 8, 1, 0, 30).unwrap();

    let mut cursor = Cursor::new(&buf);
    let (major, minor) = decode_rdev(&mut cursor, 0, 0, 30).unwrap();
    assert_eq!(major, 8);
    assert_eq!(minor, 1);
}

#[test]
fn roundtrip_rdev_same_major() {
    let mut buf = Vec::new();
    let xflags = (XMIT_SAME_RDEV_MAJOR as u32) << 8;
    encode_rdev(&mut buf, 8, 1, xflags, 30).unwrap();

    let mut cursor = Cursor::new(&buf);
    let (major, minor) = decode_rdev(&mut cursor, xflags, 8, 30).unwrap();
    assert_eq!(major, 8);
    assert_eq!(minor, 1);
}

#[test]
fn roundtrip_rdev_protocol_29_minor_8bit() {
    let mut buf = Vec::new();
    let xflags = (XMIT_RDEV_MINOR_8_PRE30 as u32) << 8;
    encode_rdev(&mut buf, 8, 5, xflags, 29).unwrap();

    let mut cursor = Cursor::new(&buf);
    let (major, minor) = decode_rdev(&mut cursor, xflags, 0, 29).unwrap();
    assert_eq!(major, 8);
    assert_eq!(minor, 5);
}

#[test]
fn roundtrip_symlink_target() {
    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, b"/target/path", 32).unwrap();

    let mut cursor = Cursor::new(&buf);
    let target = decode_symlink_target(&mut cursor, 32).unwrap();
    assert_eq!(target, b"/target/path");
}

#[test]
fn roundtrip_symlink_target_relative() {
    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, b"../lib/libfoo.so", 32).unwrap();

    let mut cursor = Cursor::new(&buf);
    let target = decode_symlink_target(&mut cursor, 32).unwrap();
    assert_eq!(target, b"../lib/libfoo.so");
}

#[test]
fn roundtrip_symlink_target_empty() {
    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, b"", 32).unwrap();

    let mut cursor = Cursor::new(&buf);
    let target = decode_symlink_target(&mut cursor, 32).unwrap();
    assert_eq!(target, b"");
}

#[test]
fn roundtrip_symlink_target_protocol_29() {
    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, b"/usr/bin/python3", 29).unwrap();

    let mut cursor = Cursor::new(&buf);
    let target = decode_symlink_target(&mut cursor, 29).unwrap();
    assert_eq!(target, b"/usr/bin/python3");
}

#[test]
fn roundtrip_symlink_target_protocol_30() {
    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, b"/usr/bin/python3", 30).unwrap();

    let mut cursor = Cursor::new(&buf);
    let target = decode_symlink_target(&mut cursor, 30).unwrap();
    assert_eq!(target, b"/usr/bin/python3");
}

#[test]
fn roundtrip_symlink_target_all_protocols() {
    let target = b"../relative/link/target";
    for proto in [28u8, 29, 30, 31, 32] {
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, target, proto).unwrap();

        let mut cursor = Cursor::new(&buf);
        let decoded = decode_symlink_target(&mut cursor, proto).unwrap();
        assert_eq!(decoded, target, "roundtrip failed for protocol {proto}");
        assert_eq!(cursor.position() as usize, buf.len());
    }
}

#[test]
fn roundtrip_symlink_target_with_unicode() {
    let target = "\u{65e5}\u{672c}\u{8a9e}/\u{30d5}\u{30a1}\u{30a4}\u{30eb}".as_bytes();
    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, target, 32).unwrap();

    let mut cursor = Cursor::new(&buf);
    let decoded = decode_symlink_target(&mut cursor, 32).unwrap();
    assert_eq!(decoded, target);
}

#[test]
fn roundtrip_symlink_target_binary_data() {
    let target: Vec<u8> = (1u8..=255).collect();
    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, &target, 32).unwrap();

    let mut cursor = Cursor::new(&buf);
    let decoded = decode_symlink_target(&mut cursor, 32).unwrap();
    assert_eq!(decoded, target);
}

#[test]
fn roundtrip_symlink_target_long() {
    let target = vec![b'x'; 4096];
    for proto in [29u8, 30, 32] {
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, &target, proto).unwrap();

        let mut cursor = Cursor::new(&buf);
        let decoded = decode_symlink_target(&mut cursor, proto).unwrap();
        assert_eq!(decoded, target, "long target failed for protocol {proto}");
    }
}

#[test]
fn roundtrip_symlink_target_path_separators_preserved() {
    let target = b"dir/subdir\\file";
    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, target, 32).unwrap();

    let mut cursor = Cursor::new(&buf);
    let decoded = decode_symlink_target(&mut cursor, 32).unwrap();
    assert_eq!(decoded, target);
}

#[test]
fn decode_symlink_target_known_bytes_proto29() {
    // Protocol 29: fixed 4-byte LE int (3) + "tgt"
    let data = vec![0x03, 0x00, 0x00, 0x00, b't', b'g', b't'];
    let mut cursor = Cursor::new(&data);
    let target = decode_symlink_target(&mut cursor, 29).unwrap();
    assert_eq!(target, b"tgt");
}

#[test]
fn decode_symlink_target_known_bytes_proto30() {
    // Protocol 30: varint (0x03) + "tgt"
    let data = vec![0x03, b't', b'g', b't'];
    let mut cursor = Cursor::new(&data);
    let target = decode_symlink_target(&mut cursor, 30).unwrap();
    assert_eq!(target, b"tgt");
}

#[test]
fn roundtrip_hardlink_idx_follower() {
    let mut buf = Vec::new();
    encode_hardlink_idx(&mut buf, 5).unwrap();

    let mut cursor = Cursor::new(&buf);
    let flags = (XMIT_HLINKED as u32) << 8;
    let idx = decode_hardlink_idx(&mut cursor, flags).unwrap();
    assert_eq!(idx, Some(5));
}

#[test]
fn roundtrip_hardlink_idx_leader() {
    let mut cursor = Cursor::new(Vec::new());
    let flags = ((XMIT_HLINKED as u32) << 8) | ((XMIT_HLINK_FIRST as u32) << 8);
    let idx = decode_hardlink_idx(&mut cursor, flags).unwrap();
    assert_eq!(idx, None);
}

#[test]
fn roundtrip_hardlink_dev_ino_different_dev() {
    let mut buf = Vec::new();
    encode_hardlink_dev_ino(&mut buf, 100, 12345, false).unwrap();

    let mut cursor = Cursor::new(&buf);
    let (dev, ino) = decode_hardlink_dev_ino(&mut cursor, 0, 0).unwrap();
    assert_eq!(dev, 100);
    assert_eq!(ino, 12345);
}

#[test]
fn roundtrip_hardlink_dev_ino_same_dev() {
    let mut buf = Vec::new();
    encode_hardlink_dev_ino(&mut buf, 100, 12345, true).unwrap();

    let mut cursor = Cursor::new(&buf);
    let flags = (XMIT_SAME_DEV_PRE30 as u32) << 8;
    let (dev, ino) = decode_hardlink_dev_ino(&mut cursor, flags, 100).unwrap();
    assert_eq!(dev, 100);
    assert_eq!(ino, 12345);
}

#[test]
fn roundtrip_checksum() {
    let mut buf = Vec::new();
    let checksum = vec![0xAA, 0xBB, 0xCC, 0xDD];
    encode_checksum(&mut buf, Some(&checksum), 4).unwrap();

    let mut cursor = Cursor::new(&buf);
    let decoded = decode_checksum(&mut cursor, 4).unwrap();
    assert_eq!(decoded, checksum);
}

#[test]
fn roundtrip_checksum_zeros() {
    let mut buf = Vec::new();
    encode_checksum(&mut buf, None, 4).unwrap();

    let mut cursor = Cursor::new(&buf);
    let decoded = decode_checksum(&mut cursor, 4).unwrap();
    assert_eq!(decoded, vec![0x00, 0x00, 0x00, 0x00]);
}

#[test]
fn decode_symlink_target_at_max_length() {
    let target = vec![b'a'; MAX_SYMLINK_TARGET_LEN];
    for proto in [29u8, 30, 32] {
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, &target, proto).unwrap();

        let mut cursor = Cursor::new(&buf);
        let decoded = decode_symlink_target(&mut cursor, proto).unwrap();
        assert_eq!(
            decoded, target,
            "target at MAX_SYMLINK_TARGET_LEN should decode for proto {proto}"
        );
    }
}

#[test]
fn decode_symlink_target_exceeding_max_length() {
    let target = vec![b'a'; MAX_SYMLINK_TARGET_LEN + 1];
    for proto in [29u8, 30, 32] {
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, &target, proto).unwrap();

        let mut cursor = Cursor::new(&buf);
        let result = decode_symlink_target(&mut cursor, proto);
        assert!(
            result.is_err(),
            "should reject target exceeding max for proto {proto}"
        );
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("exceeds maximum"),
            "error message should mention exceeding maximum, got: {err}",
        );
    }
}

#[test]
fn decode_symlink_target_far_exceeding_max_length() {
    // Simulate a malicious sender claiming a 1 MiB target
    let huge_len = 1_048_576usize;
    for proto in [29u8, 30, 32] {
        let mut buf = Vec::new();
        encode_symlink_target(&mut buf, &vec![b'x'; huge_len], proto).unwrap();

        let mut cursor = Cursor::new(&buf);
        let result = decode_symlink_target(&mut cursor, proto);
        assert!(
            result.is_err(),
            "should reject 1 MiB target for proto {proto}"
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}
