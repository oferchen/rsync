use super::*;
use std::io::{Cursor, Read};

#[test]
fn encode_flags_single_byte() {
    let mut buf = Vec::new();
    encode_flags(&mut buf, XMIT_SAME_MODE as u32, 32, false, false).unwrap();
    assert_eq!(buf, vec![XMIT_SAME_MODE]);
}

#[test]
fn encode_flags_two_bytes_protocol_28() {
    let mut buf = Vec::new();
    let xflags = (XMIT_HLINKED as u32) << 8; // Extended flags set
    encode_flags(&mut buf, xflags, 28, false, false).unwrap();
    // Should write XMIT_EXTENDED_FLAGS in low byte
    assert_eq!(buf.len(), 2);
    assert_eq!(buf[0] & XMIT_EXTENDED_FLAGS, XMIT_EXTENDED_FLAGS);
}

#[test]
fn encode_flags_varint_mode() {
    let mut buf = Vec::new();
    encode_flags(&mut buf, 0x123, 32, true, false).unwrap();
    // Should use varint encoding
    let mut cursor = Cursor::new(&buf);
    let decoded = crate::varint::read_varint(&mut cursor).unwrap();
    assert_eq!(decoded, 0x123);
}

#[test]
fn encode_flags_zero_becomes_extended_in_varint_mode() {
    let mut buf = Vec::new();
    encode_flags(&mut buf, 0, 32, true, false).unwrap();
    let mut cursor = Cursor::new(&buf);
    let decoded = crate::varint::read_varint(&mut cursor).unwrap();
    assert_eq!(decoded, XMIT_EXTENDED_FLAGS as i32);
}

#[test]
fn encode_flags_zero_for_file_uses_top_dir_in_protocol_28() {
    let mut buf = Vec::new();
    encode_flags(&mut buf, 0, 28, false, false).unwrap();
    // For files with zero flags, should use XMIT_TOP_DIR
    assert!(buf[0] & XMIT_TOP_DIR != 0 || buf[0] & XMIT_EXTENDED_FLAGS != 0);
}

#[test]
fn encode_flags_zero_for_dir_stays_zero_in_protocol_28() {
    let mut buf = Vec::new();
    encode_flags(&mut buf, 0, 28, false, true).unwrap();
    // For directories with zero flags, extended flags bit should be set
    // to distinguish from end-of-list marker
    assert!(buf.len() == 2 || buf[0] == XMIT_EXTENDED_FLAGS);
}

#[test]
fn encode_end_marker_simple() {
    let mut buf = Vec::new();
    encode_end_marker(&mut buf, false, false, None).unwrap();
    assert_eq!(buf, vec![0u8]);
}

#[test]
fn encode_end_marker_varint() {
    let mut buf = Vec::new();
    encode_end_marker(&mut buf, true, false, None).unwrap();
    // Two varints: 0 and 0
    let mut cursor = Cursor::new(&buf);
    assert_eq!(crate::varint::read_varint(&mut cursor).unwrap(), 0);
    assert_eq!(crate::varint::read_varint(&mut cursor).unwrap(), 0);
}

#[test]
fn encode_end_marker_varint_with_error() {
    let mut buf = Vec::new();
    encode_end_marker(&mut buf, true, false, Some(23)).unwrap();
    let mut cursor = Cursor::new(&buf);
    assert_eq!(crate::varint::read_varint(&mut cursor).unwrap(), 0);
    assert_eq!(crate::varint::read_varint(&mut cursor).unwrap(), 23);
}

#[test]
fn encode_end_marker_safe_file_list_with_error() {
    let mut buf = Vec::new();
    encode_end_marker(&mut buf, false, true, Some(42)).unwrap();
    assert_eq!(buf[0], XMIT_EXTENDED_FLAGS);
    assert_eq!(buf[1], XMIT_IO_ERROR_ENDLIST);
    let mut cursor = Cursor::new(&buf[2..]);
    assert_eq!(crate::varint::read_varint(&mut cursor).unwrap(), 42);
}

#[test]
fn encode_name_no_compression() {
    let mut buf = Vec::new();
    encode_name(&mut buf, b"test.txt", 0, 0, 32).unwrap();
    // suffix_len byte + "test.txt"
    assert_eq!(buf.len(), 1 + 8);
    assert_eq!(buf[0], 8); // suffix length
    assert_eq!(&buf[1..], b"test.txt");
}

#[test]
fn encode_name_with_compression() {
    let mut buf = Vec::new();
    encode_name(&mut buf, b"dir/file2.txt", 8, XMIT_SAME_NAME as u32, 32).unwrap();
    // same_len byte + suffix_len byte + "2.txt"
    assert_eq!(buf.len(), 1 + 1 + 5);
    assert_eq!(buf[0], 8); // same_len
    assert_eq!(buf[1], 5); // suffix_len
    assert_eq!(&buf[2..], b"2.txt");
}

#[test]
fn encode_name_long_name_modern() {
    let mut buf = Vec::new();
    let long_name = vec![b'a'; 300];
    encode_name(&mut buf, &long_name, 0, XMIT_LONG_NAME as u32, 32).unwrap();
    // varint(300) + 300 bytes
    let mut cursor = Cursor::new(&buf);
    let len = crate::varint::read_varint(&mut cursor).unwrap();
    assert_eq!(len, 300);
}

#[test]
fn encode_name_long_name_legacy() {
    let mut buf = Vec::new();
    let long_name = vec![b'a'; 300];
    encode_name(&mut buf, &long_name, 0, XMIT_LONG_NAME as u32, 29).unwrap();
    // 4-byte length + 300 bytes
    assert_eq!(buf.len(), 4 + 300);
    let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    assert_eq!(len, 300);
}

#[test]
fn encode_size_modern() {
    let mut buf = Vec::new();
    encode_size(&mut buf, 1000, 32).unwrap();
    // varlong30 with min_bytes=3
    let mut cursor = Cursor::new(&buf);
    let decoded = crate::read_varlong(&mut cursor, 3).unwrap();
    assert_eq!(decoded, 1000);
}

#[test]
fn encode_size_legacy() {
    let mut buf = Vec::new();
    encode_size(&mut buf, 1000, 29).unwrap();
    // longint: 4 bytes for small values
    assert_eq!(buf.len(), 4);
    let decoded = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    assert_eq!(decoded, 1000);
}

#[test]
fn encode_size_large_legacy() {
    let mut buf = Vec::new();
    let large = 0x1_0000_0000u64;
    encode_size(&mut buf, large, 29).unwrap();
    // longint: 4-byte marker + 8-byte value
    assert_eq!(buf.len(), 12);
}

#[test]
fn encode_mode_regular_file() {
    let mut buf = Vec::new();
    encode_mode(&mut buf, 0o100644).unwrap();
    assert_eq!(buf.len(), 4);
    let decoded = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    assert_eq!(decoded as u32, 0o100644);
}

#[test]
fn encode_mode_directory() {
    let mut buf = Vec::new();
    encode_mode(&mut buf, 0o040755).unwrap();
    let decoded = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    assert_eq!(decoded as u32, 0o040755);
}

#[test]
fn encode_uid_modern() {
    let mut buf = Vec::new();
    encode_uid(&mut buf, 1000, 32).unwrap();
    let mut cursor = Cursor::new(&buf);
    let decoded = crate::varint::read_varint(&mut cursor).unwrap();
    assert_eq!(decoded, 1000);
}

#[test]
fn encode_uid_legacy() {
    let mut buf = Vec::new();
    encode_uid(&mut buf, 1000, 29).unwrap();
    assert_eq!(buf.len(), 4);
    let decoded = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    assert_eq!(decoded, 1000);
}

#[test]
fn encode_gid_modern() {
    let mut buf = Vec::new();
    encode_gid(&mut buf, 500, 30).unwrap();
    let mut cursor = Cursor::new(&buf);
    let decoded = crate::varint::read_varint(&mut cursor).unwrap();
    assert_eq!(decoded, 500);
}

#[test]
fn encode_owner_name_short() {
    let mut buf = Vec::new();
    encode_owner_name(&mut buf, "user").unwrap();
    assert_eq!(buf[0], 4); // length
    assert_eq!(&buf[1..], b"user");
}

#[test]
fn encode_owner_name_truncated() {
    let mut buf = Vec::new();
    let long_name = "a".repeat(300);
    encode_owner_name(&mut buf, &long_name).unwrap();
    assert_eq!(buf[0], 255); // max length
    assert_eq!(buf.len(), 256);
}

#[test]
fn encode_rdev_protocol_30() {
    let mut buf = Vec::new();
    encode_rdev(&mut buf, 8, 1, 0, 30).unwrap();
    // varint30(major) + varint(minor)
    let mut cursor = Cursor::new(&buf);
    let major = crate::varint::read_varint30_int(&mut cursor, 30).unwrap();
    let minor = crate::varint::read_varint(&mut cursor).unwrap();
    assert_eq!(major, 8);
    assert_eq!(minor, 1);
}

#[test]
fn encode_rdev_same_major() {
    let mut buf = Vec::new();
    let xflags = (XMIT_SAME_RDEV_MAJOR as u32) << 8;
    encode_rdev(&mut buf, 8, 1, xflags, 30).unwrap();
    // Only minor written
    let mut cursor = Cursor::new(&buf);
    let minor = crate::varint::read_varint(&mut cursor).unwrap();
    assert_eq!(minor, 1);
}

#[test]
fn encode_rdev_protocol_29_minor_8bit() {
    let mut buf = Vec::new();
    let xflags = (XMIT_RDEV_MINOR_8_PRE30 as u32) << 8;
    encode_rdev(&mut buf, 8, 5, xflags, 29).unwrap();
    // varint30(major) + u8(minor)
    // Find where minor is (after major)
    let minor_offset = buf.len() - 1;
    assert_eq!(buf[minor_offset], 5);
}

#[test]
fn encode_symlink_target_simple() {
    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, b"/target/path", 32).unwrap();
    // varint30(len) + bytes
    let mut cursor = Cursor::new(&buf);
    let len = crate::varint::read_varint30_int(&mut cursor, 32).unwrap();
    assert_eq!(len, 12);
}

#[test]
fn encode_symlink_target_relative() {
    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, b"../lib/libfoo.so", 32).unwrap();
    let mut cursor = Cursor::new(&buf);
    let len = crate::varint::read_varint30_int(&mut cursor, 32).unwrap();
    assert_eq!(len, 16);
    let mut target = vec![0u8; len as usize];
    cursor.read_exact(&mut target).unwrap();
    assert_eq!(&target, b"../lib/libfoo.so");
}

#[test]
fn encode_symlink_target_empty() {
    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, b"", 32).unwrap();
    let mut cursor = Cursor::new(&buf);
    let len = crate::varint::read_varint30_int(&mut cursor, 32).unwrap();
    assert_eq!(len, 0);
}

#[test]
fn encode_symlink_target_with_spaces_and_unicode() {
    let target = "path/to/my file/\u{00e9}t\u{00e9}".as_bytes();
    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, target, 32).unwrap();
    let mut cursor = Cursor::new(&buf);
    let len = crate::varint::read_varint30_int(&mut cursor, 32).unwrap();
    assert_eq!(len as usize, target.len());
    let mut decoded = vec![0u8; len as usize];
    cursor.read_exact(&mut decoded).unwrap();
    assert_eq!(&decoded, target);
}

#[test]
fn encode_symlink_target_protocol_29_uses_fixed_int() {
    let target = b"/target";
    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, target, 29).unwrap();
    // Protocol < 30: 4 bytes for length + target bytes
    assert_eq!(buf.len(), 4 + target.len());
    let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    assert_eq!(len, 7);
    assert_eq!(&buf[4..], target);
}

#[test]
fn encode_symlink_target_protocol_30_uses_varint() {
    let target = b"/target";
    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, target, 30).unwrap();
    // Protocol 30+: varint (1 byte for small values) + target bytes
    assert!(buf.len() < 4 + target.len()); // More compact than fixed int
    assert!(buf.ends_with(target));
}

#[test]
fn encode_symlink_target_long_path() {
    let target = vec![b'a'; 4096];
    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, &target, 32).unwrap();
    let mut cursor = Cursor::new(&buf);
    let len = crate::varint::read_varint30_int(&mut cursor, 32).unwrap();
    assert_eq!(len, 4096);
}

#[test]
fn encode_symlink_target_path_separators_preserved() {
    // Verify both forward and backslash are preserved as-is (no conversion)
    let target = b"dir/subdir\\file";
    let mut buf = Vec::new();
    encode_symlink_target(&mut buf, target, 32).unwrap();
    let mut cursor = Cursor::new(&buf);
    let len = crate::varint::read_varint30_int(&mut cursor, 32).unwrap();
    let mut decoded = vec![0u8; len as usize];
    cursor.read_exact(&mut decoded).unwrap();
    assert_eq!(&decoded, target);
}

#[test]
fn encode_hardlink_idx_simple() {
    let mut buf = Vec::new();
    encode_hardlink_idx(&mut buf, 5).unwrap();
    let mut cursor = Cursor::new(&buf);
    let idx = crate::varint::read_varint(&mut cursor).unwrap();
    assert_eq!(idx, 5);
}

#[test]
fn encode_hardlink_dev_ino_different_dev() {
    let mut buf = Vec::new();
    encode_hardlink_dev_ino(&mut buf, 100, 12345, false).unwrap();
    // longint(dev+1) + longint(ino)
    let mut cursor = Cursor::new(&buf);
    let dev_plus_one = crate::read_longint(&mut cursor).unwrap();
    let ino = crate::read_longint(&mut cursor).unwrap();
    assert_eq!(dev_plus_one, 101); // dev + 1
    assert_eq!(ino, 12345);
}

#[test]
fn encode_hardlink_dev_ino_same_dev() {
    let mut buf = Vec::new();
    encode_hardlink_dev_ino(&mut buf, 100, 12345, true).unwrap();
    // Only longint(ino)
    let mut cursor = Cursor::new(&buf);
    let ino = crate::read_longint(&mut cursor).unwrap();
    assert_eq!(ino, 12345);
}

#[test]
fn encode_checksum_with_data() {
    let mut buf = Vec::new();
    let checksum = vec![0xAA, 0xBB, 0xCC, 0xDD];
    encode_checksum(&mut buf, Some(&checksum), 4).unwrap();
    assert_eq!(buf, checksum);
}

#[test]
fn encode_checksum_padded() {
    let mut buf = Vec::new();
    let checksum = vec![0xAA, 0xBB];
    encode_checksum(&mut buf, Some(&checksum), 4).unwrap();
    assert_eq!(buf, vec![0xAA, 0xBB, 0x00, 0x00]);
}

#[test]
fn encode_checksum_none() {
    let mut buf = Vec::new();
    encode_checksum(&mut buf, None, 4).unwrap();
    assert_eq!(buf, vec![0x00, 0x00, 0x00, 0x00]);
}

#[test]
fn calculate_name_prefix_len_full_match() {
    assert_eq!(calculate_name_prefix_len(b"test.txt", b"test.txt"), 8);
}

#[test]
fn calculate_name_prefix_len_partial() {
    assert_eq!(
        calculate_name_prefix_len(b"dir/file1.txt", b"dir/file2.txt"),
        8
    );
}

#[test]
fn calculate_name_prefix_len_no_match() {
    assert_eq!(calculate_name_prefix_len(b"abc", b"xyz"), 0);
}

#[test]
fn calculate_name_prefix_len_capped_at_255() {
    let long = vec![b'a'; 300];
    assert_eq!(calculate_name_prefix_len(&long, &long), 255);
}

#[test]
fn calculate_basic_flags_all_same() {
    let flags = calculate_basic_flags(
        0o100644, 0o100644, // same mode
        1000, 1000, // same mtime
        500, 500, // same uid
        600, 600, // same gid
        5, 3, // some prefix compression
        true, true, false,
    );
    assert!(flags & XMIT_SAME_MODE != 0);
    assert!(flags & XMIT_SAME_TIME != 0);
    assert!(flags & XMIT_SAME_UID != 0);
    assert!(flags & XMIT_SAME_GID != 0);
    assert!(flags & XMIT_SAME_NAME != 0);
}

#[test]
fn calculate_basic_flags_all_different() {
    let flags = calculate_basic_flags(
        0o100644, 0o100755, // different mode
        1000, 2000, // different mtime
        500, 600, // different uid
        700, 800, // different gid
        0, 8, // no prefix compression
        true, true, false,
    );
    assert!(flags & XMIT_SAME_MODE == 0);
    assert!(flags & XMIT_SAME_TIME == 0);
    assert!(flags & XMIT_SAME_UID == 0);
    assert!(flags & XMIT_SAME_GID == 0);
    assert!(flags & XMIT_SAME_NAME == 0);
}

#[test]
fn calculate_basic_flags_long_name() {
    let flags = calculate_basic_flags(
        0o100644, 0o100644, 1000, 1000, 0, 0, 0, 0, 0, 300, // suffix > 255
        false, false, false,
    );
    assert!(flags & XMIT_LONG_NAME != 0);
}

#[test]
fn calculate_basic_flags_top_dir() {
    let flags = calculate_basic_flags(0o040755, 0, 0, 0, 0, 0, 0, 0, 0, 3, false, false, true);
    assert!(flags & XMIT_TOP_DIR != 0);
}

#[test]
fn calculate_device_flags_same_major() {
    let flags = calculate_device_flags(8, 8, 1, 30);
    assert!(flags & XMIT_SAME_RDEV_MAJOR != 0);
}

#[test]
fn calculate_device_flags_minor_8bit_proto29() {
    let flags = calculate_device_flags(8, 0, 5, 29);
    assert!(flags & XMIT_RDEV_MINOR_8_PRE30 != 0);
}

#[test]
fn calculate_device_flags_minor_large_proto29() {
    let flags = calculate_device_flags(8, 0, 300, 29);
    assert!(flags & XMIT_RDEV_MINOR_8_PRE30 == 0);
}

#[test]
fn calculate_hardlink_flags_proto30_first() {
    let flags = calculate_hardlink_flags(Some(u32::MAX), None, 0, 30, false);
    assert!(flags & XMIT_HLINKED != 0);
    assert!(flags & XMIT_HLINK_FIRST != 0);
}

#[test]
fn calculate_hardlink_flags_proto30_follower() {
    let flags = calculate_hardlink_flags(Some(5), None, 0, 30, false);
    assert!(flags & XMIT_HLINKED != 0);
    assert!(flags & XMIT_HLINK_FIRST == 0);
}

#[test]
fn calculate_hardlink_flags_proto29_same_dev() {
    let flags = calculate_hardlink_flags(None, Some(100), 100, 29, false);
    assert!(flags & XMIT_SAME_DEV_PRE30 != 0);
}

#[test]
fn calculate_hardlink_flags_directory_ignored() {
    let flags = calculate_hardlink_flags(Some(5), None, 0, 30, true);
    assert!(flags == 0);
}

#[test]
fn calculate_time_flags_same_atime() {
    let flags = calculate_time_flags(1000, 1000, 0, 0, 0, 31, true, false, false);
    assert!(flags & (XMIT_SAME_ATIME as u16) != 0);
}

#[test]
fn calculate_time_flags_crtime_eq_mtime() {
    let flags = calculate_time_flags(0, 0, 5000, 5000, 0, 31, false, true, false);
    assert!(flags & ((XMIT_CRTIME_EQ_MTIME as u16) << 8) != 0);
}

#[test]
fn calculate_time_flags_mtime_nsec() {
    let flags = calculate_time_flags(0, 0, 0, 1000, 123456, 31, false, false, false);
    assert!(flags & (XMIT_MOD_NSEC as u16) != 0);
}

#[test]
fn calculate_time_flags_no_nsec_proto30() {
    let flags = calculate_time_flags(0, 0, 0, 1000, 123456, 30, false, false, false);
    assert!(flags & (XMIT_MOD_NSEC as u16) == 0);
}

#[test]
fn encode_mtime_modern() {
    let mut buf = Vec::new();
    encode_mtime(&mut buf, 1700000000, 32).unwrap();
    let mut cursor = Cursor::new(&buf);
    let decoded = crate::read_varlong(&mut cursor, 4).unwrap();
    assert_eq!(decoded, 1700000000);
}

#[test]
fn encode_mtime_legacy() {
    let mut buf = Vec::new();
    encode_mtime(&mut buf, 1700000000, 29).unwrap();
    assert_eq!(buf.len(), 4);
    let decoded = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    assert_eq!(decoded, 1700000000);
}

#[test]
fn test_encode_mtime_nsec() {
    let mut buf = Vec::new();
    encode_mtime_nsec(&mut buf, 123456789).unwrap();
    let mut cursor = Cursor::new(&buf);
    let decoded = crate::varint::read_varint(&mut cursor).unwrap();
    assert_eq!(decoded, 123456789);
}

#[test]
fn encode_atime_simple() {
    let mut buf = Vec::new();
    encode_atime(&mut buf, 1700000001).unwrap();
    let mut cursor = Cursor::new(&buf);
    let decoded = crate::read_varlong(&mut cursor, 4).unwrap();
    assert_eq!(decoded, 1700000001);
}

#[test]
fn encode_crtime_simple() {
    let mut buf = Vec::new();
    encode_crtime(&mut buf, 1600000000).unwrap();
    let mut cursor = Cursor::new(&buf);
    let decoded = crate::read_varlong(&mut cursor, 4).unwrap();
    assert_eq!(decoded, 1600000000);
}

#[test]
fn roundtrip_flags_and_name() {
    let mut buf = Vec::new();

    // Write flags + name for "dir/file2.txt" with prefix compression
    let xflags = XMIT_SAME_NAME as u32 | XMIT_SAME_MODE as u32;
    encode_flags(&mut buf, xflags, 32, false, false).unwrap();
    encode_name(&mut buf, b"dir/file2.txt", 8, xflags, 32).unwrap();

    // Verify structure
    assert_eq!(buf[0], xflags as u8); // flags byte
    assert_eq!(buf[1], 8); // same_len
    assert_eq!(buf[2], 5); // suffix_len ("2.txt")
    assert_eq!(&buf[3..], b"2.txt");
}

#[test]
fn roundtrip_full_entry_modern() {
    let mut buf = Vec::new();

    // Encode a complete file entry
    let xflags = 0u32; // All fields different from previous
    encode_flags(&mut buf, xflags, 32, false, false).unwrap();
    encode_name(&mut buf, b"test.txt", 0, xflags, 32).unwrap();
    encode_size(&mut buf, 1024, 32).unwrap();
    encode_mtime(&mut buf, 1700000000, 32).unwrap();
    encode_mode(&mut buf, 0o100644).unwrap();

    // Should have produced a valid byte sequence
    assert!(!buf.is_empty());
}
