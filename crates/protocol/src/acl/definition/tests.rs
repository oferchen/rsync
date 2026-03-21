use super::*;
use crate::acl::constants::{ACCESS_SHIFT, XFLAG_NAME_IS_USER};
use crate::acl::constants::{
    XMIT_GROUP_OBJ, XMIT_MASK_OBJ, XMIT_NAME_LIST, XMIT_OTHER_OBJ, XMIT_USER_OBJ,
};
use crate::acl::entry::{IdAccess, RsyncAcl};
use crate::varint::write_varint;
use std::io::Cursor;

// --- AclPerms tests ---

#[test]
fn perms_from_bits_masks_to_three_bits() {
    assert_eq!(AclPerms::from_bits(0xFF).bits(), 0x07);
    assert_eq!(AclPerms::from_bits(0x00).bits(), 0x00);
    assert_eq!(AclPerms::from_bits(0x05).bits(), 0x05);
}

#[test]
fn perms_read_write_execute() {
    let rwx = AclPerms::from_bits(7);
    assert!(rwx.read());
    assert!(rwx.write());
    assert!(rwx.execute());

    let r_only = AclPerms::from_bits(4);
    assert!(r_only.read());
    assert!(!r_only.write());
    assert!(!r_only.execute());

    let none = AclPerms::from_bits(0);
    assert!(!none.read());
    assert!(!none.write());
    assert!(!none.execute());
}

#[test]
fn perms_display() {
    assert_eq!(format!("{}", AclPerms::from_bits(7)), "rwx");
    assert_eq!(format!("{}", AclPerms::from_bits(5)), "r-x");
    assert_eq!(format!("{}", AclPerms::from_bits(6)), "rw-");
    assert_eq!(format!("{}", AclPerms::from_bits(0)), "---");
    assert_eq!(format!("{}", AclPerms::from_bits(1)), "--x");
}

// --- AclTag tests ---

#[test]
fn tag_equality() {
    assert_eq!(AclTag::UserObj, AclTag::UserObj);
    assert_ne!(AclTag::UserObj, AclTag::GroupObj);
    assert_eq!(AclTag::User(1000), AclTag::User(1000));
    assert_ne!(AclTag::User(1000), AclTag::User(1001));
    assert_ne!(AclTag::User(1000), AclTag::Group(1000));
}

#[test]
fn tag_debug_format() {
    assert!(format!("{:?}", AclTag::UserObj).contains("UserObj"));
    assert!(format!("{:?}", AclTag::User(42)).contains("42"));
    assert!(format!("{:?}", AclTag::Group(100)).contains("Group"));
}

// --- AclEntry tests ---

#[test]
fn entry_construction() {
    let entry = AclEntry::new(AclTag::UserObj, AclPerms::from_bits(7));
    assert_eq!(entry.tag, AclTag::UserObj);
    assert_eq!(entry.perms.bits(), 7);
}

// --- AclDefinition tests ---

#[test]
fn empty_definition() {
    let def = AclDefinition::new();
    assert!(def.is_empty());
    assert_eq!(def.len(), 0);
    assert!(!def.mask_set());
    assert!(def.entries().is_empty());
}

#[test]
fn from_rsync_acl_minimal() {
    let acl = RsyncAcl::from_mode(0o755);
    let def = AclDefinition::from_rsync_acl(&acl);

    assert_eq!(def.len(), 3); // user_obj, group_obj, other_obj
    assert!(!def.mask_set());
    assert_eq!(def.entries()[0].tag, AclTag::UserObj);
    assert_eq!(def.entries()[0].perms.bits(), 7); // rwx
    assert_eq!(def.entries()[1].tag, AclTag::GroupObj);
    assert_eq!(def.entries()[1].perms.bits(), 5); // r-x
    assert_eq!(def.entries()[2].tag, AclTag::Other);
    assert_eq!(def.entries()[2].perms.bits(), 5); // r-x
}

#[test]
fn from_rsync_acl_with_mask_and_named_entries() {
    let mut acl = RsyncAcl::new();
    acl.user_obj = 7;
    acl.group_obj = 5;
    acl.mask_obj = 7;
    acl.other_obj = 0;
    acl.names.push(IdAccess::user(1000, 7));
    acl.names.push(IdAccess::group(100, 5));

    let def = AclDefinition::from_rsync_acl(&acl);
    assert_eq!(def.len(), 6);
    assert!(def.mask_set());

    // Standard entries
    assert_eq!(def.entries()[0].tag, AclTag::UserObj);
    assert_eq!(def.entries()[1].tag, AclTag::GroupObj);
    assert_eq!(def.entries()[2].tag, AclTag::Mask);
    assert_eq!(def.entries()[3].tag, AclTag::Other);

    // Named entries
    assert_eq!(def.entries()[4].tag, AclTag::User(1000));
    assert_eq!(def.entries()[4].perms.bits(), 7);
    assert_eq!(def.entries()[5].tag, AclTag::Group(100));
    assert_eq!(def.entries()[5].perms.bits(), 5);
}

#[test]
fn from_rsync_acl_empty() {
    let acl = RsyncAcl::new();
    let def = AclDefinition::from_rsync_acl(&acl);
    assert!(def.is_empty());
    assert!(!def.mask_set());
}

#[test]
fn definition_into_entries() {
    let acl = RsyncAcl::from_mode(0o644);
    let def = AclDefinition::from_rsync_acl(&acl);
    let entries = def.into_entries();
    assert_eq!(entries.len(), 3);
}

#[test]
fn definition_iter() {
    let acl = RsyncAcl::from_mode(0o700);
    let def = AclDefinition::from_rsync_acl(&acl);
    let tags: Vec<_> = def.iter().map(|e| e.tag).collect();
    assert_eq!(tags, vec![AclTag::UserObj, AclTag::GroupObj, AclTag::Other]);
}

#[test]
fn definition_into_iterator_ref() {
    let acl = RsyncAcl::from_mode(0o750);
    let def = AclDefinition::from_rsync_acl(&acl);
    let count = (&def).into_iter().count();
    assert_eq!(count, 3);
}

#[test]
fn definition_into_iterator_owned() {
    let acl = RsyncAcl::from_mode(0o750);
    let def = AclDefinition::from_rsync_acl(&acl);
    let entries: Vec<_> = def.into_iter().collect();
    assert_eq!(entries.len(), 3);
}

// --- Wire parsing tests ---

/// Helper: builds wire bytes for a flags-only ACL (no entries).
fn wire_empty_acl() -> Vec<u8> {
    vec![0x00] // flags = 0, no entries
}

/// Helper: builds wire bytes for an ACL with standard entries only.
fn wire_standard_acl(user: u8, group: u8, other: u8) -> Vec<u8> {
    let flags = XMIT_USER_OBJ | XMIT_GROUP_OBJ | XMIT_OTHER_OBJ;
    let mut data = vec![flags];
    // Each standard entry is a varint; single-byte for values 0-7
    data.push(user);
    data.push(group);
    data.push(other);
    data
}

/// Helper: builds wire bytes for an ACL with mask.
fn wire_acl_with_mask(user: u8, group: u8, mask: u8, other: u8) -> Vec<u8> {
    let flags = XMIT_USER_OBJ | XMIT_GROUP_OBJ | XMIT_MASK_OBJ | XMIT_OTHER_OBJ;
    let mut data = vec![flags];
    data.push(user);
    data.push(group);
    data.push(mask);
    data.push(other);
    data
}

#[test]
fn read_empty_acl() {
    let data = wire_empty_acl();
    let mut cursor = Cursor::new(data);
    let def = read_acl_definition(&mut cursor).unwrap();

    assert!(def.is_empty());
    assert!(!def.mask_set());
}

#[test]
fn read_standard_entries_only() {
    let data = wire_standard_acl(7, 5, 5);
    let mut cursor = Cursor::new(data);
    let def = read_acl_definition(&mut cursor).unwrap();

    assert_eq!(def.len(), 3);
    assert!(!def.mask_set());

    assert_eq!(def.entries()[0].tag, AclTag::UserObj);
    assert_eq!(def.entries()[0].perms.bits(), 7);
    assert_eq!(def.entries()[1].tag, AclTag::GroupObj);
    assert_eq!(def.entries()[1].perms.bits(), 5);
    assert_eq!(def.entries()[2].tag, AclTag::Other);
    assert_eq!(def.entries()[2].perms.bits(), 5);
}

#[test]
fn read_acl_with_explicit_mask() {
    let data = wire_acl_with_mask(7, 7, 5, 4);
    let mut cursor = Cursor::new(data);
    let def = read_acl_definition(&mut cursor).unwrap();

    assert_eq!(def.len(), 4);
    assert!(def.mask_set());

    assert_eq!(def.entries()[0].tag, AclTag::UserObj);
    assert_eq!(def.entries()[0].perms.bits(), 7);
    assert_eq!(def.entries()[1].tag, AclTag::GroupObj);
    assert_eq!(def.entries()[1].perms.bits(), 7);
    assert_eq!(def.entries()[2].tag, AclTag::Mask);
    assert_eq!(def.entries()[2].perms.bits(), 5);
    assert_eq!(def.entries()[3].tag, AclTag::Other);
    assert_eq!(def.entries()[3].perms.bits(), 4);
}

#[test]
fn read_acl_with_named_entries() {
    let mut data = Vec::new();

    // flags: user_obj + group_obj + other_obj + name_list
    let flags = XMIT_USER_OBJ | XMIT_GROUP_OBJ | XMIT_OTHER_OBJ | XMIT_NAME_LIST;
    data.push(flags);

    // Standard entries (single-byte varints)
    data.push(7); // user_obj = rwx
    data.push(5); // group_obj = r-x
    data.push(4); // other_obj = r--

    // ida_entries: count=2
    write_varint(&mut data, 2).unwrap();

    // Entry 1: user uid=1000, perms=rwx
    write_varint(&mut data, 1000).unwrap();
    let encoded = (0x07u32 << ACCESS_SHIFT) | XFLAG_NAME_IS_USER;
    write_varint(&mut data, encoded as i32).unwrap();

    // Entry 2: group gid=100, perms=r-x
    write_varint(&mut data, 100).unwrap();
    let encoded = 0x05u32 << ACCESS_SHIFT;
    write_varint(&mut data, encoded as i32).unwrap();

    let mut cursor = Cursor::new(data);
    let def = read_acl_definition(&mut cursor).unwrap();

    // 3 standard + 2 named + 1 computed mask = 6
    assert_eq!(def.len(), 6);
    assert!(!def.mask_set()); // mask was computed, not explicit

    assert_eq!(def.entries()[0].tag, AclTag::UserObj);
    assert_eq!(def.entries()[1].tag, AclTag::GroupObj);
    assert_eq!(def.entries()[2].tag, AclTag::Other);
    assert_eq!(def.entries()[3].tag, AclTag::User(1000));
    assert_eq!(def.entries()[3].perms.bits(), 7);
    assert_eq!(def.entries()[4].tag, AclTag::Group(100));
    assert_eq!(def.entries()[4].perms.bits(), 5);

    // Computed mask should be union of named entry permissions: 7 | 5 = 7
    assert_eq!(def.entries()[5].tag, AclTag::Mask);
    assert_eq!(def.entries()[5].perms.bits(), 7);
}

#[test]
fn read_acl_with_named_entries_and_explicit_mask() {
    let mut data = Vec::new();

    let flags =
        XMIT_USER_OBJ | XMIT_GROUP_OBJ | XMIT_MASK_OBJ | XMIT_OTHER_OBJ | XMIT_NAME_LIST;
    data.push(flags);

    data.push(7); // user_obj
    data.push(7); // group_obj
    data.push(5); // mask_obj (explicit)
    data.push(0); // other_obj

    // ida_entries: count=1
    write_varint(&mut data, 1).unwrap();

    // Entry: user uid=500, perms=rwx
    write_varint(&mut data, 500).unwrap();
    let encoded = (0x07u32 << ACCESS_SHIFT) | XFLAG_NAME_IS_USER;
    write_varint(&mut data, encoded as i32).unwrap();

    let mut cursor = Cursor::new(data);
    let def = read_acl_definition(&mut cursor).unwrap();

    // 4 standard + 1 named = 5 (no computed mask because explicit mask exists)
    assert_eq!(def.len(), 5);
    assert!(def.mask_set());

    assert_eq!(def.entries()[2].tag, AclTag::Mask);
    assert_eq!(def.entries()[2].perms.bits(), 5);
    assert_eq!(def.entries()[4].tag, AclTag::User(500));
}

#[test]
fn read_acl_user_obj_only() {
    let data = vec![XMIT_USER_OBJ, 6]; // rw-
    let mut cursor = Cursor::new(data);
    let def = read_acl_definition(&mut cursor).unwrap();

    assert_eq!(def.len(), 1);
    assert_eq!(def.entries()[0].tag, AclTag::UserObj);
    assert_eq!(def.entries()[0].perms.bits(), 6);
}

#[test]
fn read_acl_mask_only() {
    let data = vec![XMIT_MASK_OBJ, 5]; // r-x
    let mut cursor = Cursor::new(data);
    let def = read_acl_definition(&mut cursor).unwrap();

    assert_eq!(def.len(), 1);
    assert!(def.mask_set());
    assert_eq!(def.entries()[0].tag, AclTag::Mask);
    assert_eq!(def.entries()[0].perms.bits(), 5);
}

#[test]
fn read_acl_other_obj_only() {
    let data = vec![XMIT_OTHER_OBJ, 4]; // r--
    let mut cursor = Cursor::new(data);
    let def = read_acl_definition(&mut cursor).unwrap();

    assert_eq!(def.len(), 1);
    assert_eq!(def.entries()[0].tag, AclTag::Other);
    assert_eq!(def.entries()[0].perms.bits(), 4);
}

#[test]
fn read_acl_all_permission_combinations() {
    for perms in 0..=7u8 {
        let data = vec![XMIT_USER_OBJ, perms];
        let mut cursor = Cursor::new(data);
        let def = read_acl_definition(&mut cursor).unwrap();

        assert_eq!(def.entries()[0].perms.bits(), perms);
    }
}

// --- EOF error tests ---

#[test]
fn read_eof_on_flags() {
    let data: Vec<u8> = vec![];
    let mut cursor = Cursor::new(data);
    assert!(read_acl_definition(&mut cursor).is_err());
}

#[test]
fn read_eof_on_user_obj() {
    let data = vec![XMIT_USER_OBJ]; // flags say user_obj but no data
    let mut cursor = Cursor::new(data);
    assert!(read_acl_definition(&mut cursor).is_err());
}

#[test]
fn read_eof_on_group_obj() {
    let data = vec![XMIT_USER_OBJ | XMIT_GROUP_OBJ, 7]; // user_obj ok, group_obj missing
    let mut cursor = Cursor::new(data);
    assert!(read_acl_definition(&mut cursor).is_err());
}

#[test]
fn read_eof_on_mask_obj() {
    let data = vec![XMIT_MASK_OBJ]; // flags say mask but no data
    let mut cursor = Cursor::new(data);
    assert!(read_acl_definition(&mut cursor).is_err());
}

#[test]
fn read_eof_on_other_obj() {
    let data = vec![XMIT_OTHER_OBJ]; // flags say other but no data
    let mut cursor = Cursor::new(data);
    assert!(read_acl_definition(&mut cursor).is_err());
}

#[test]
fn read_eof_on_ida_count() {
    let data = vec![XMIT_NAME_LIST]; // flags say name list but no count
    let mut cursor = Cursor::new(data);
    assert!(read_acl_definition(&mut cursor).is_err());
}

// --- Roundtrip tests ---

#[test]
fn roundtrip_empty_acl() {
    let original = AclDefinition::new();
    let mut buf = Vec::new();
    write_acl_definition(&mut buf, &original).unwrap();

    let mut cursor = Cursor::new(buf);
    let parsed = read_acl_definition(&mut cursor).unwrap();

    assert!(parsed.is_empty());
}

#[test]
fn roundtrip_standard_entries() {
    let acl = RsyncAcl::from_mode(0o755);
    let original = AclDefinition::from_rsync_acl(&acl);

    let mut buf = Vec::new();
    write_acl_definition(&mut buf, &original).unwrap();

    let mut cursor = Cursor::new(buf);
    let parsed = read_acl_definition(&mut cursor).unwrap();

    assert_eq!(parsed.len(), original.len());
    for (a, b) in parsed.entries().iter().zip(original.entries().iter()) {
        assert_eq!(a.tag, b.tag);
        assert_eq!(a.perms.bits(), b.perms.bits());
    }
}

#[test]
fn roundtrip_with_mask() {
    let mut acl = RsyncAcl::new();
    acl.user_obj = 7;
    acl.group_obj = 7;
    acl.mask_obj = 5;
    acl.other_obj = 4;

    let original = AclDefinition::from_rsync_acl(&acl);
    assert!(original.mask_set());

    let mut buf = Vec::new();
    write_acl_definition(&mut buf, &original).unwrap();

    let mut cursor = Cursor::new(buf);
    let parsed = read_acl_definition(&mut cursor).unwrap();

    assert!(parsed.mask_set());
    assert_eq!(parsed.len(), original.len());
    for (a, b) in parsed.entries().iter().zip(original.entries().iter()) {
        assert_eq!(a.tag, b.tag);
        assert_eq!(a.perms.bits(), b.perms.bits());
    }
}

#[test]
fn roundtrip_with_named_entries() {
    let mut acl = RsyncAcl::new();
    acl.user_obj = 7;
    acl.group_obj = 5;
    acl.mask_obj = 7;
    acl.other_obj = 0;
    acl.names.push(IdAccess::user(1000, 7));
    acl.names.push(IdAccess::group(100, 5));

    let original = AclDefinition::from_rsync_acl(&acl);

    let mut buf = Vec::new();
    write_acl_definition(&mut buf, &original).unwrap();

    let mut cursor = Cursor::new(buf);
    let parsed = read_acl_definition(&mut cursor).unwrap();

    // The roundtrip may differ slightly in mask handling because
    // write uses the entries as-is but read may add computed mask.
    // With explicit mask in original, the roundtrip should be exact.
    assert!(parsed.mask_set());
    assert_eq!(parsed.entries()[0].tag, AclTag::UserObj);
    assert_eq!(parsed.entries()[1].tag, AclTag::GroupObj);
    assert_eq!(parsed.entries()[2].tag, AclTag::Mask);
    assert_eq!(parsed.entries()[3].tag, AclTag::Other);
    assert_eq!(parsed.entries()[4].tag, AclTag::User(1000));
    assert_eq!(parsed.entries()[5].tag, AclTag::Group(100));
}

#[test]
fn named_entries_empty_list_no_computed_mask() {
    // XMIT_NAME_LIST set but count=0 - no computed mask added
    let mut data = Vec::new();
    data.push(XMIT_NAME_LIST);
    write_varint(&mut data, 0).unwrap(); // count = 0

    let mut cursor = Cursor::new(data);
    let def = read_acl_definition(&mut cursor).unwrap();

    assert!(def.is_empty());
    assert!(!def.mask_set());
}

#[test]
fn perms_default_is_zero() {
    let p = AclPerms::default();
    assert_eq!(p.bits(), 0);
    assert!(!p.read());
    assert!(!p.write());
    assert!(!p.execute());
}

#[test]
fn definition_default_is_empty() {
    let def = AclDefinition::default();
    assert!(def.is_empty());
    assert!(!def.mask_set());
}
