//! Tests for ACL wire protocol encoding and decoding.

use std::io::Cursor;

use super::encoding::{decode_access, encode_access};
use super::recv::{recv_acl, recv_ida_entries, recv_rsync_acl};
use super::send::{send_acl, send_ida_entries, send_rsync_acl};
use super::types::{AclType, RecvAclResult};

use crate::acl::constants::{
    ACCESS_SHIFT, NAME_IS_USER, NO_ENTRY, XFLAG_NAME_FOLLOWS, XFLAG_NAME_IS_USER, XMIT_GROUP_OBJ,
    XMIT_USER_OBJ,
};
use crate::acl::entry::{AclCache, IdAccess, IdaEntries, RsyncAcl};

#[test]
fn encode_decode_access_roundtrip() {
    // User entry with rwx
    let access = 0x07 | NAME_IS_USER;
    let encoded = encode_access(access, false);
    let (decoded, name_follows) = decode_access(encoded, true);
    assert_eq!(decoded & !NAME_IS_USER, access & !NAME_IS_USER);
    assert!(decoded & NAME_IS_USER != 0);
    assert!(!name_follows);

    // Group entry with rx
    let access = 0x05;
    let encoded = encode_access(access, true);
    let (decoded, name_follows) = decode_access(encoded, true);
    assert_eq!(decoded, access);
    assert!(name_follows);
}

#[test]
fn send_recv_empty_acl() {
    let acl = RsyncAcl::new();
    let mut cache = AclCache::new();
    let mut buf = Vec::new();

    send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_rsync_acl(&mut cursor).unwrap();

    match result {
        RecvAclResult::Literal(received) => {
            assert!(received.is_empty());
        }
        RecvAclResult::CacheHit(_) => panic!("Expected literal ACL"),
    }
}

#[test]
fn send_recv_acl_with_entries() {
    let mut acl = RsyncAcl::new();
    acl.user_obj = 0x07; // rwx
    acl.group_obj = 0x05; // r-x
    acl.other_obj = 0x04; // r--

    let mut cache = AclCache::new();
    let mut buf = Vec::new();

    send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_rsync_acl(&mut cursor).unwrap();

    match result {
        RecvAclResult::Literal(received) => {
            assert_eq!(received.user_obj, 0x07);
            assert_eq!(received.group_obj, 0x05);
            assert_eq!(received.other_obj, 0x04);
            assert_eq!(received.mask_obj, NO_ENTRY);
        }
        RecvAclResult::CacheHit(_) => panic!("Expected literal ACL"),
    }
}

#[test]
fn cache_hit_on_second_send() {
    let mut acl = RsyncAcl::new();
    acl.user_obj = 0x07;

    let mut cache = AclCache::new();
    let mut buf = Vec::new();

    // First send - should be literal
    send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();
    let first_len = buf.len();

    // Second send of same ACL - should be cache hit (shorter)
    buf.clear();
    send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

    assert!(buf.len() < first_len, "Cache hit should be shorter");

    let mut cursor = Cursor::new(buf);
    let result = recv_rsync_acl(&mut cursor).unwrap();

    match result {
        RecvAclResult::CacheHit(idx) => {
            assert_eq!(idx, 0);
        }
        RecvAclResult::Literal(_) => panic!("Expected cache hit"),
    }
}

#[test]
fn send_recv_ida_entries_roundtrip() {
    let mut entries = IdaEntries::new();
    entries.push(IdAccess::user(1000, 0x07));
    entries.push(IdAccess::group(100, 0x05));

    let mut buf = Vec::new();
    send_ida_entries(&mut buf, &entries, false).unwrap();

    let mut cursor = Cursor::new(buf);
    let (received, mask) = recv_ida_entries(&mut cursor).unwrap();

    assert_eq!(received.len(), 2);
    assert_eq!(mask, 0x07); // OR of all permissions
}

#[test]
fn send_recv_directory_acl() {
    let access_acl = {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.group_obj = 0x05;
        acl.other_obj = 0x05;
        acl
    };

    let default_acl = {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.group_obj = 0x05;
        acl.other_obj = 0x00;
        acl
    };

    let mut cache = AclCache::new();
    let mut buf = Vec::new();

    send_acl(&mut buf, &access_acl, Some(&default_acl), true, &mut cache).unwrap();

    let mut cursor = Cursor::new(buf);
    let (access_result, default_result) = recv_acl(&mut cursor, true).unwrap();

    assert!(matches!(access_result, RecvAclResult::Literal(_)));
    assert!(default_result.is_some());
    assert!(matches!(default_result.unwrap(), RecvAclResult::Literal(_)));
}

#[test]
fn encode_access_all_permission_bits() {
    // Test all permission combinations
    for perms in 0..=7 {
        let encoded = encode_access(perms, false);
        let (decoded, _) = decode_access(encoded, true);
        assert_eq!(
            decoded & !NAME_IS_USER,
            perms,
            "Perms 0x{perms:02X} roundtrip failed"
        );
    }
}

#[test]
fn encode_access_name_is_user_flag() {
    let access = 0x05 | NAME_IS_USER;
    let encoded = encode_access(access, true);

    // Encoded value should have both flags set
    assert!(encoded & XFLAG_NAME_FOLLOWS != 0);
    assert!(encoded & XFLAG_NAME_IS_USER != 0);

    let (decoded, name_follows) = decode_access(encoded, true);
    assert!(name_follows);
    assert!(decoded & NAME_IS_USER != 0);
    assert_eq!(decoded & !NAME_IS_USER, 0x05);
}

#[test]
fn decode_access_non_name_entry() {
    // Non-name entries return the raw value without flag interpretation
    let value = 0x1234;
    let (decoded, name_follows) = decode_access(value, false);
    assert_eq!(decoded, value);
    assert!(!name_follows);
}

#[test]
fn encode_access_shifts_correctly() {
    // Verify ACCESS_SHIFT is applied correctly
    let perms = 0x07; // rwx
    let encoded = encode_access(perms, false);

    // Perms should be shifted left by ACCESS_SHIFT (2)
    assert_eq!(encoded >> ACCESS_SHIFT, perms);
    // Lower 2 bits should be clear (no flags)
    assert_eq!(encoded & 0x03, 0);
}

#[test]
fn recv_ida_entries_eof_reading_count() {
    let mut cursor = Cursor::new(Vec::<u8>::new());
    let result = recv_ida_entries(&mut cursor);
    assert!(result.is_err());
}

#[test]
fn recv_ida_entries_eof_reading_id() {
    // Count says 1 entry but no id follows
    let data = vec![0x01]; // count = 1
    let mut cursor = Cursor::new(data);
    let result = recv_ida_entries(&mut cursor);
    assert!(result.is_err());
}

#[test]
fn recv_ida_entries_eof_reading_access() {
    // Count says 1 entry, id present, but no access
    let data = vec![0x01, 0x64]; // count = 1, id = 100
    let mut cursor = Cursor::new(data);
    let result = recv_ida_entries(&mut cursor);
    assert!(result.is_err());
}

#[test]
fn recv_rsync_acl_eof_reading_ndx() {
    let mut cursor = Cursor::new(Vec::<u8>::new());
    let result = recv_rsync_acl(&mut cursor);
    assert!(result.is_err());
}

#[test]
fn recv_rsync_acl_eof_reading_flags() {
    // ndx = 0 (literal) but no flags byte
    let data = vec![0x00]; // ndx + 1 = 0, so ndx = -1
    let mut cursor = Cursor::new(data);
    let result = recv_rsync_acl(&mut cursor);
    assert!(result.is_err());
}

#[test]
fn recv_rsync_acl_eof_reading_user_obj() {
    // ndx = 0 (literal), flags indicate user_obj, but no data
    let data = vec![0x00, XMIT_USER_OBJ]; // ndx = -1, flags = XMIT_USER_OBJ
    let mut cursor = Cursor::new(data);
    let result = recv_rsync_acl(&mut cursor);
    assert!(result.is_err());
}

#[test]
fn recv_rsync_acl_eof_reading_group_obj() {
    // flags indicate group_obj, but no data after user_obj
    let data = vec![0x00, XMIT_USER_OBJ | XMIT_GROUP_OBJ, 0x07]; // user_obj = 7
    let mut cursor = Cursor::new(data);
    let result = recv_rsync_acl(&mut cursor);
    assert!(result.is_err());
}

#[test]
fn recv_acl_result_debug_format() {
    let cache_hit = RecvAclResult::CacheHit(42);
    let debug = format!("{cache_hit:?}");
    assert!(debug.contains("CacheHit"));
    assert!(debug.contains("42"));

    let literal = RecvAclResult::Literal(RsyncAcl::new());
    let debug = format!("{literal:?}");
    assert!(debug.contains("Literal"));
}

#[test]
fn acl_type_equality_and_copy() {
    let a = AclType::Access;
    let b = AclType::Access;
    let c = AclType::Default;

    assert_eq!(a, b);
    assert_ne!(a, c);

    // Test Clone/Copy
    let d = a;
    assert_eq!(a, d);
}

#[test]
fn acl_type_debug_format() {
    let access = AclType::Access;
    let default = AclType::Default;

    assert!(format!("{access:?}").contains("Access"));
    assert!(format!("{default:?}").contains("Default"));
}

#[test]
fn recv_ida_entries_with_name_follows() {
    use crate::varint::write_varint;

    let mut data = Vec::new();
    // count = 1
    write_varint(&mut data, 1).unwrap();
    // id = 1000
    write_varint(&mut data, 1000).unwrap();
    // access with XFLAG_NAME_FOLLOWS set: perms=7, flags=1 (name follows)
    let encoded = (0x07 << ACCESS_SHIFT) | XFLAG_NAME_FOLLOWS;
    write_varint(&mut data, encoded as i32).unwrap();
    // name length = 4
    data.push(4);
    // name = "test"
    data.extend_from_slice(b"test");

    let mut cursor = Cursor::new(data);
    let (entries, mask) = recv_ida_entries(&mut cursor).unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries.iter().next().unwrap().id, 1000);
    assert_eq!(mask, 0x07);
}

#[test]
fn recv_ida_entries_name_follows_eof_in_length() {
    use crate::varint::write_varint;

    let mut data = Vec::new();
    write_varint(&mut data, 1).unwrap(); // count
    write_varint(&mut data, 1000).unwrap(); // id
    let encoded = (0x07 << ACCESS_SHIFT) | XFLAG_NAME_FOLLOWS;
    write_varint(&mut data, encoded as i32).unwrap();
    // Missing name length byte

    let mut cursor = Cursor::new(data);
    let result = recv_ida_entries(&mut cursor);
    assert!(result.is_err());
}

#[test]
fn recv_ida_entries_name_follows_eof_in_name() {
    use crate::varint::write_varint;

    let mut data = Vec::new();
    write_varint(&mut data, 1).unwrap(); // count
    write_varint(&mut data, 1000).unwrap(); // id
    let encoded = (0x07 << ACCESS_SHIFT) | XFLAG_NAME_FOLLOWS;
    write_varint(&mut data, encoded as i32).unwrap();
    data.push(10); // name length = 10
    data.extend_from_slice(b"abc"); // Only 3 bytes instead of 10

    let mut cursor = Cursor::new(data);
    let result = recv_ida_entries(&mut cursor);
    assert!(result.is_err());
}

#[test]
fn separate_caches_for_access_and_default() {
    let mut acl = RsyncAcl::new();
    acl.user_obj = 0x07;

    let mut cache = AclCache::new();

    // Send as access ACL first
    let mut buf1 = Vec::new();
    send_rsync_acl(&mut buf1, &acl, AclType::Access, &mut cache, false).unwrap();

    // Send same ACL as default - should NOT hit cache (different type)
    let mut buf2 = Vec::new();
    send_rsync_acl(&mut buf2, &acl, AclType::Default, &mut cache, false).unwrap();

    // Both should be full literals (not cache hits)
    let mut cursor1 = Cursor::new(buf1);
    let result1 = recv_rsync_acl(&mut cursor1).unwrap();
    assert!(matches!(result1, RecvAclResult::Literal(_)));

    let mut cursor2 = Cursor::new(buf2);
    let result2 = recv_rsync_acl(&mut cursor2).unwrap();
    assert!(matches!(result2, RecvAclResult::Literal(_)));
}

#[test]
fn send_recv_file_acl_no_default() {
    let access_acl = {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x06;
        acl.group_obj = 0x04;
        acl.other_obj = 0x04;
        acl
    };

    let mut cache = AclCache::new();
    let mut buf = Vec::new();

    // File (not directory) - no default ACL sent
    send_acl(&mut buf, &access_acl, None, false, &mut cache).unwrap();

    let mut cursor = Cursor::new(buf);
    let (access_result, default_result) = recv_acl(&mut cursor, false).unwrap();

    assert!(matches!(access_result, RecvAclResult::Literal(_)));
    assert!(default_result.is_none());
}

#[test]
fn send_recv_acl_with_mask_obj() {
    let mut acl = RsyncAcl::new();
    acl.user_obj = 0x07;
    acl.group_obj = 0x07;
    acl.mask_obj = 0x05; // Effective permissions masked to r-x
    acl.other_obj = 0x04;

    let mut cache = AclCache::new();
    let mut buf = Vec::new();

    send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

    let mut cursor = Cursor::new(buf);
    let result = recv_rsync_acl(&mut cursor).unwrap();

    match result {
        RecvAclResult::Literal(received) => {
            assert_eq!(received.user_obj, 0x07);
            assert_eq!(received.group_obj, 0x07);
            assert_eq!(received.mask_obj, 0x05);
            assert_eq!(received.other_obj, 0x04);
        }
        RecvAclResult::CacheHit(_) => panic!("Expected literal ACL"),
    }
}
