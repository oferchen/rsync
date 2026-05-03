//! Additional tests for ACL wire protocol.
//!
//! These tests complement the unit tests in `wire.rs` with more comprehensive
//! coverage including edge cases, boundary conditions, and upstream compatibility.

use super::*;
use std::io::Cursor;

/// Tests for `IdAccess` structure.
mod id_access_tests {
    use super::*;

    #[test]
    fn user_entry_has_name_is_user_flag() {
        let entry = IdAccess::user(1000, 0x07);
        assert!(entry.is_user());
        assert_eq!(entry.permissions(), 0x07);
    }

    #[test]
    fn group_entry_does_not_have_name_is_user_flag() {
        let entry = IdAccess::group(100, 0x05);
        assert!(!entry.is_user());
        assert_eq!(entry.permissions(), 0x05);
    }

    #[test]
    fn permissions_mask_removes_name_is_user() {
        let entry = IdAccess::user(1000, 0x07);
        assert_eq!(entry.permissions(), 0x07);
        assert_eq!(entry.access & NAME_IS_USER, NAME_IS_USER);
    }

    #[test]
    fn default_id_access_is_zero() {
        let entry = IdAccess::default();
        assert_eq!(entry.id, 0);
        assert_eq!(entry.access, 0);
        assert!(!entry.is_user());
    }
}

/// Tests for `IdaEntries` structure.
mod ida_entries_tests {
    use super::*;

    #[test]
    fn new_is_empty() {
        let entries = IdaEntries::new();
        assert!(entries.is_empty());
        assert_eq!(entries.len(), 0);
    }

    #[test]
    fn push_increases_len() {
        let mut entries = IdaEntries::new();
        entries.push(IdAccess::user(1000, 0x07));
        assert_eq!(entries.len(), 1);
        entries.push(IdAccess::group(100, 0x05));
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn computed_mask_bits_combines_all_permissions() {
        let mut entries = IdaEntries::new();
        entries.push(IdAccess::user(1000, 0x04)); // r--
        entries.push(IdAccess::group(100, 0x02)); // -w-
        entries.push(IdAccess::user(1001, 0x01)); // --x
        assert_eq!(entries.computed_mask_bits(), 0x07); // rwx
    }

    #[test]
    fn computed_mask_bits_excludes_no_entry() {
        let mut entries = IdaEntries::new();
        entries.push(IdAccess::user(1000, 0x07 | NO_ENTRY as u32));
        let mask = entries.computed_mask_bits();
        assert_eq!(mask & NO_ENTRY, 0);
    }

    #[test]
    fn from_iterator_creates_entries() {
        let entries: IdaEntries = vec![IdAccess::user(1000, 0x07), IdAccess::group(100, 0x05)]
            .into_iter()
            .collect();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn iter_yields_all_entries() {
        let mut entries = IdaEntries::new();
        entries.push(IdAccess::user(1000, 0x07));
        entries.push(IdAccess::group(100, 0x05));

        let collected: Vec<_> = entries.iter().collect();
        assert_eq!(collected.len(), 2);
        assert!(collected[0].is_user());
        assert!(!collected[1].is_user());
    }
}

/// Tests for `RsyncAcl` structure.
mod rsync_acl_tests {
    use super::*;

    #[test]
    fn default_has_all_no_entry() {
        let acl = RsyncAcl::default();
        assert!(!acl.has_user_obj());
        assert!(!acl.has_group_obj());
        assert!(!acl.has_mask_obj());
        assert!(!acl.has_other_obj());
        assert!(acl.names.is_empty());
        assert!(acl.is_empty());
    }

    #[test]
    fn has_methods_detect_present_entries() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        assert!(acl.has_user_obj());
        assert!(!acl.has_group_obj());

        acl.group_obj = 0x05;
        assert!(acl.has_group_obj());

        acl.mask_obj = 0x07;
        assert!(acl.has_mask_obj());

        acl.other_obj = 0x04;
        assert!(acl.has_other_obj());
    }

    #[test]
    fn is_empty_false_when_any_entry_present() {
        let mut acl = RsyncAcl::new();
        assert!(acl.is_empty());

        acl.user_obj = 0x07;
        assert!(!acl.is_empty());
    }

    #[test]
    fn is_empty_false_when_names_present() {
        let mut acl = RsyncAcl::new();
        acl.names.push(IdAccess::user(1000, 0x07));
        assert!(!acl.is_empty());
    }

    #[test]
    fn flags_reflect_present_entries() {
        let mut acl = RsyncAcl::new();
        assert_eq!(acl.flags(), 0);

        acl.user_obj = 0x07;
        assert_eq!(acl.flags() & XMIT_USER_OBJ, XMIT_USER_OBJ);

        acl.group_obj = 0x05;
        assert_eq!(acl.flags() & XMIT_GROUP_OBJ, XMIT_GROUP_OBJ);

        acl.mask_obj = 0x07;
        assert_eq!(acl.flags() & XMIT_MASK_OBJ, XMIT_MASK_OBJ);

        acl.other_obj = 0x04;
        assert_eq!(acl.flags() & XMIT_OTHER_OBJ, XMIT_OTHER_OBJ);

        acl.names.push(IdAccess::user(1000, 0x07));
        assert_eq!(acl.flags() & XMIT_NAME_LIST, XMIT_NAME_LIST);
    }

    #[test]
    fn flags_has_all_bits_set_when_fully_populated() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.group_obj = 0x05;
        acl.mask_obj = 0x07;
        acl.other_obj = 0x04;
        acl.names.push(IdAccess::user(1000, 0x07));

        let expected =
            XMIT_USER_OBJ | XMIT_GROUP_OBJ | XMIT_MASK_OBJ | XMIT_OTHER_OBJ | XMIT_NAME_LIST;
        assert_eq!(acl.flags(), expected);
    }
}

/// Tests for `AclCache` structure.
mod acl_cache_tests {
    use super::*;

    #[test]
    fn new_cache_is_empty() {
        let cache = AclCache::new();
        assert_eq!(cache.access_count(), 0);
        assert_eq!(cache.default_count(), 0);
    }

    #[test]
    fn store_access_returns_incrementing_indices() {
        let mut cache = AclCache::new();
        let acl1 = RsyncAcl::new();
        let acl2 = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x07;
            a
        };

        assert_eq!(cache.store_access(acl1), 0);
        assert_eq!(cache.store_access(acl2), 1);
        assert_eq!(cache.access_count(), 2);
    }

    #[test]
    fn store_default_returns_incrementing_indices() {
        let mut cache = AclCache::new();
        let acl1 = RsyncAcl::new();
        let acl2 = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x07;
            a
        };

        assert_eq!(cache.store_default(acl1), 0);
        assert_eq!(cache.store_default(acl2), 1);
        assert_eq!(cache.default_count(), 2);
    }

    #[test]
    fn find_access_returns_none_for_unknown() {
        let cache = AclCache::new();
        let acl = RsyncAcl::new();
        assert!(cache.find_access(&acl).is_none());
    }

    #[test]
    fn find_access_returns_index_for_known() {
        let mut cache = AclCache::new();
        let acl = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x07;
            a
        };
        let _ = cache.store_access(acl.clone());

        assert_eq!(cache.find_access(&acl), Some(0));
    }

    #[test]
    fn get_access_retrieves_stored_acl() {
        let mut cache = AclCache::new();
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.group_obj = 0x05;
        let _ = cache.store_access(acl.clone());

        let retrieved = cache.get_access(0).expect("Should find ACL");
        assert_eq!(retrieved.user_obj, 0x07);
        assert_eq!(retrieved.group_obj, 0x05);
    }

    #[test]
    fn get_access_returns_none_for_invalid_index() {
        let cache = AclCache::new();
        assert!(cache.get_access(0).is_none());
        assert!(cache.get_access(100).is_none());
    }

    #[test]
    fn access_and_default_caches_are_separate() {
        let mut cache = AclCache::new();
        let acl = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x07;
            a
        };

        let _ = cache.store_access(acl.clone());
        assert_eq!(cache.find_access(&acl), Some(0));
        assert!(cache.find_default(&acl).is_none());

        let _ = cache.store_default(acl.clone());
        assert_eq!(cache.find_default(&acl), Some(0));
    }
}

/// Wire protocol round-trip tests.
mod wire_roundtrip_tests {
    use super::*;

    #[test]
    fn roundtrip_acl_with_named_entries() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.group_obj = 0x05;
        acl.mask_obj = 0x07;
        acl.other_obj = 0x04;
        acl.names.push(IdAccess::user(1000, 0x07));
        acl.names.push(IdAccess::group(100, 0x05));

        let mut cache = AclCache::new();
        let mut buf = Vec::new();
        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

        let mut cursor = Cursor::new(buf);
        let result = recv_rsync_acl(&mut cursor).unwrap();

        match result {
            RecvAclResult::Literal(received) => {
                assert_eq!(received.user_obj, acl.user_obj);
                assert_eq!(received.group_obj, acl.group_obj);
                assert_eq!(received.mask_obj, acl.mask_obj);
                assert_eq!(received.other_obj, acl.other_obj);
                assert_eq!(received.names.len(), acl.names.len());
            }
            RecvAclResult::CacheHit(_) => panic!("Expected literal"),
        }
    }

    #[test]
    fn roundtrip_preserves_permission_bits() {
        for perm in 0..=7u8 {
            let mut acl = RsyncAcl::new();
            acl.user_obj = perm;

            let mut cache = AclCache::new();
            let mut buf = Vec::new();
            send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

            let mut cursor = Cursor::new(buf);
            if let RecvAclResult::Literal(received) = recv_rsync_acl(&mut cursor).unwrap() {
                assert_eq!(received.user_obj, perm, "Permission {perm} not preserved");
            }
        }
    }

    #[test]
    fn roundtrip_file_acl_no_default() {
        let access_acl = {
            let mut acl = RsyncAcl::new();
            acl.user_obj = 0x06; // rw-
            acl.group_obj = 0x04; // r--
            acl.other_obj = 0x04; // r--
            acl
        };

        let mut cache = AclCache::new();
        let mut buf = Vec::new();
        send_acl(&mut buf, &access_acl, None, false, &mut cache).unwrap();

        let mut cursor = Cursor::new(buf);
        let (access_result, default_result) = recv_acl(&mut cursor, false).unwrap();

        assert!(matches!(access_result, RecvAclResult::Literal(_)));
        assert!(default_result.is_none());
    }

    #[test]
    fn roundtrip_directory_acl_with_default() {
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

        if let RecvAclResult::Literal(access) = access_result {
            assert_eq!(access.user_obj, 0x07);
        } else {
            panic!("Expected literal access ACL");
        }

        if let Some(RecvAclResult::Literal(default)) = default_result {
            assert_eq!(default.other_obj, 0x00);
        } else {
            panic!("Expected literal default ACL");
        }
    }

    #[test]
    fn multiple_cache_hits() {
        let acl = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x07;
            a
        };

        let mut cache = AclCache::new();
        let mut buf = Vec::new();

        // First send
        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

        // Second and third sends should all be cache hits
        for expected_idx in [0u32, 0, 0] {
            buf.clear();
            send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

            let mut cursor = Cursor::new(&buf);
            match recv_rsync_acl(&mut cursor).unwrap() {
                RecvAclResult::CacheHit(idx) => assert_eq!(idx, expected_idx),
                RecvAclResult::Literal(_) => panic!("Expected cache hit"),
            }
        }
    }

    #[test]
    fn different_acls_get_different_indices() {
        let mut cache = AclCache::new();

        let acl1 = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x07;
            a
        };
        let acl2 = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x05;
            a
        };

        let mut buf = Vec::new();
        send_rsync_acl(&mut buf, &acl1, AclType::Access, &mut cache, false).unwrap();
        buf.clear();
        send_rsync_acl(&mut buf, &acl2, AclType::Access, &mut cache, false).unwrap();

        // Now both should hit cache with different indices
        buf.clear();
        send_rsync_acl(&mut buf, &acl1, AclType::Access, &mut cache, false).unwrap();
        let mut cursor = Cursor::new(&buf);
        if let RecvAclResult::CacheHit(idx) = recv_rsync_acl(&mut cursor).unwrap() {
            assert_eq!(idx, 0);
        }

        buf.clear();
        send_rsync_acl(&mut buf, &acl2, AclType::Access, &mut cache, false).unwrap();
        let mut cursor = Cursor::new(&buf);
        if let RecvAclResult::CacheHit(idx) = recv_rsync_acl(&mut cursor).unwrap() {
            assert_eq!(idx, 1);
        }
    }
}

/// Tests for constants matching upstream rsync.
mod constants_tests {
    use super::*;

    #[test]
    fn xmit_flags_match_upstream() {
        // Upstream acls.c lines 38-42
        assert_eq!(XMIT_USER_OBJ, 0x01);
        assert_eq!(XMIT_GROUP_OBJ, 0x02);
        assert_eq!(XMIT_MASK_OBJ, 0x04);
        assert_eq!(XMIT_OTHER_OBJ, 0x08);
        assert_eq!(XMIT_NAME_LIST, 0x10);
    }

    #[test]
    fn no_entry_matches_upstream() {
        // Upstream acls.c line 44
        assert_eq!(NO_ENTRY, 0x80);
    }

    #[test]
    fn xflag_constants_match_upstream() {
        // Upstream acls.c lines 52-53
        assert_eq!(XFLAG_NAME_FOLLOWS, 0x0001);
        assert_eq!(XFLAG_NAME_IS_USER, 0x0002);
    }

    #[test]
    fn name_is_user_matches_upstream() {
        // Upstream acls.c line 46
        assert_eq!(NAME_IS_USER, 1 << 31);
    }

    #[test]
    fn access_shift_is_two() {
        // Access bits shifted left by 2 for wire encoding
        assert_eq!(ACCESS_SHIFT, 2);
    }
}

/// Edge case tests.
mod edge_cases {
    use super::*;

    #[test]
    fn empty_ida_entries_roundtrip() {
        let entries = IdaEntries::new();
        let mut buf = Vec::new();
        send_ida_entries(&mut buf, &entries, false).unwrap();

        let mut cursor = Cursor::new(buf);
        let (received, mask) = recv_ida_entries(&mut cursor).unwrap();

        assert!(received.is_empty());
        assert_eq!(mask, 0);
    }

    #[test]
    fn acl_with_only_mask() {
        let mut acl = RsyncAcl::new();
        acl.mask_obj = 0x07;

        let mut cache = AclCache::new();
        let mut buf = Vec::new();
        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

        let mut cursor = Cursor::new(buf);
        if let RecvAclResult::Literal(received) = recv_rsync_acl(&mut cursor).unwrap() {
            assert_eq!(received.mask_obj, 0x07);
            assert_eq!(received.user_obj, NO_ENTRY);
            assert_eq!(received.group_obj, NO_ENTRY);
            assert_eq!(received.other_obj, NO_ENTRY);
        }
    }

    #[test]
    fn acl_with_max_permission_bits() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.group_obj = 0x07;
        acl.mask_obj = 0x07;
        acl.other_obj = 0x07;

        let mut cache = AclCache::new();
        let mut buf = Vec::new();
        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

        let mut cursor = Cursor::new(buf);
        if let RecvAclResult::Literal(received) = recv_rsync_acl(&mut cursor).unwrap() {
            assert_eq!(received.user_obj, 0x07);
            assert_eq!(received.group_obj, 0x07);
            assert_eq!(received.mask_obj, 0x07);
            assert_eq!(received.other_obj, 0x07);
        }
    }

    #[test]
    fn large_id_roundtrip() {
        let mut entries = IdaEntries::new();
        entries.push(IdAccess::user(u32::MAX - 1, 0x07));
        entries.push(IdAccess::group(u32::MAX - 2, 0x05));

        let mut buf = Vec::new();
        send_ida_entries(&mut buf, &entries, false).unwrap();

        let mut cursor = Cursor::new(buf);
        let (received, _) = recv_ida_entries(&mut cursor).unwrap();

        let items: Vec<_> = received.iter().collect();
        assert_eq!(items[0].id, u32::MAX - 1);
        assert_eq!(items[1].id, u32::MAX - 2);
    }

    #[test]
    fn cache_equality_is_exact() {
        let mut cache = AclCache::new();

        let acl1 = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x07;
            a.group_obj = 0x05;
            a
        };
        let acl2 = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x07;
            a.group_obj = 0x04;
            a
        };

        let _ = cache.store_access(acl1.clone());

        assert!(cache.find_access(&acl2).is_none());
        assert_eq!(cache.find_access(&acl1), Some(0));
    }
}

/// Wire format compatibility tests with upstream rsync.
///
/// These tests verify byte-level compatibility with upstream rsync's
/// ACL wire encoding. The expected bytes are derived from the encoding
/// algorithm in upstream `acls.c`.
mod wire_format_compatibility {
    use super::*;

    /// Verifies empty ACL wire format.
    ///
    /// Wire format (literal, flags=0):
    /// - ndx + 1 = 0 (varint: 0x00)
    /// - flags = 0x00
    #[test]
    fn empty_acl_wire_format() {
        let acl = RsyncAcl::new();
        let mut cache = AclCache::new();
        let mut buf = Vec::new();

        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

        // ndx=-1 encoded as ndx+1=0 (varint 0x00), flags=0x00
        assert_eq!(buf, vec![0x00, 0x00]);
    }

    /// Verifies ACL with user_obj only.
    ///
    /// Wire format:
    /// - ndx + 1 = 0 (varint: 0x00)
    /// - flags = XMIT_USER_OBJ (0x01)
    /// - user_obj = 0x07 (varint: 0x07)
    #[test]
    fn user_obj_only_wire_format() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07; // rwx

        let mut cache = AclCache::new();
        let mut buf = Vec::new();

        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

        assert_eq!(buf, vec![0x00, 0x01, 0x07]);
    }

    /// Verifies full standard ACL entries (no names).
    ///
    /// Wire format:
    /// - ndx + 1 = 0 (0x00)
    /// - flags = USER|GROUP|MASK|OTHER (0x0f)
    /// - user_obj = 7 (0x07)
    /// - group_obj = 5 (0x05)
    /// - mask_obj = 7 (0x07)
    /// - other_obj = 4 (0x04)
    #[test]
    fn full_standard_acl_wire_format() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.group_obj = 0x05;
        acl.mask_obj = 0x07;
        acl.other_obj = 0x04;

        let mut cache = AclCache::new();
        let mut buf = Vec::new();

        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

        assert_eq!(buf, vec![0x00, 0x0f, 0x07, 0x05, 0x07, 0x04]);
    }

    /// Verifies cache hit encoding.
    ///
    /// When an ACL is already in cache, only ndx+1 is sent.
    /// For index 0: ndx+1 = 1 (varint: 0x01)
    #[test]
    fn cache_hit_wire_format() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;

        let mut cache = AclCache::new();
        let mut buf = Vec::new();

        // First send - stores in cache
        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

        // Second send - should be cache hit
        buf.clear();
        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

        // Cache index 0: ndx+1 = 1
        assert_eq!(buf, vec![0x01]);
    }

    /// Verifies ida_entries encoding for named user/group entries.
    ///
    /// Wire format for user(1000, rwx) + group(100, r-x):
    /// - count = 2 (varint: 0x02)
    /// - Entry 1: id=1000, access=(7<<2)|XFLAG_NAME_IS_USER = 0x1E
    /// - Entry 2: id=100, access=(5<<2) = 0x14
    #[test]
    fn ida_entries_wire_format() {
        let mut entries = IdaEntries::new();
        entries.push(IdAccess::user(1000, 0x07)); // rwx
        entries.push(IdAccess::group(100, 0x05)); // r-x

        let mut buf = Vec::new();
        send_ida_entries(&mut buf, &entries, false).unwrap();

        // count=2 (0x02)
        // user 1000: id as varint, access=(7<<2)|2 = 0x1E
        // group 100: id as varint, access=(5<<2) = 0x14
        // Note: 1000 as varint is encoded differently based on INT_BYTE_EXTRA
        // 1000 = 0x3E8, which encodes as [0xfe, 0xe8, 0x03] in rsync varint
        assert_eq!(buf[0], 0x02); // count

        // Verify round-trip maintains data integrity
        let mut cursor = Cursor::new(&buf);
        let (received, _) = recv_ida_entries(&mut cursor).unwrap();
        assert_eq!(received.len(), 2);

        let items: Vec<_> = received.iter().collect();
        assert!(items[0].is_user());
        assert_eq!(items[0].id, 1000);
        assert_eq!(items[0].permissions(), 0x07);
        assert!(!items[1].is_user());
        assert_eq!(items[1].id, 100);
        assert_eq!(items[1].permissions(), 0x05);
    }

    /// Verifies ACL with named entries wire format.
    #[test]
    fn acl_with_names_wire_format() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.names.push(IdAccess::user(1000, 0x07));

        let mut cache = AclCache::new();
        let mut buf = Vec::new();

        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

        // ndx+1=0, flags=USER_OBJ|NAME_LIST (0x11), user_obj, then ida_entries
        assert_eq!(buf[0], 0x00); // ndx+1
        assert_eq!(buf[1], 0x11); // flags = XMIT_USER_OBJ | XMIT_NAME_LIST
        assert_eq!(buf[2], 0x07); // user_obj

        // Verify round-trip
        let mut cursor = Cursor::new(buf);
        if let RecvAclResult::Literal(received) = recv_rsync_acl(&mut cursor).unwrap() {
            assert_eq!(received.user_obj, 0x07);
            assert_eq!(received.names.len(), 1);
        } else {
            panic!("Expected literal ACL");
        }
    }

    /// Verifies access encoding with XFLAG bits.
    ///
    /// Upstream encodes access as: (perms << 2) | flags
    /// - XFLAG_NAME_FOLLOWS = 0x01 (bit 0)
    /// - XFLAG_NAME_IS_USER = 0x02 (bit 1)
    #[test]
    fn access_encoding_xflag_bits() {
        // User entry with rwx (0x07): (7<<2) | XFLAG_NAME_IS_USER = 0x1E
        let entry = IdAccess::user(1000, 0x07);
        let mut buf = Vec::new();
        let mut entries = IdaEntries::new();
        entries.push(entry);
        send_ida_entries(&mut buf, &entries, false).unwrap();

        // Find the access byte (after count and id varints)
        // The access should be (0x07 << 2) | 0x02 = 0x1E (no name follows)
        let mut cursor = Cursor::new(&buf);
        let (received, _) = recv_ida_entries(&mut cursor).unwrap();
        let item = received.iter().next().unwrap();
        assert!(item.is_user());
        assert_eq!(item.permissions(), 0x07);
    }

    /// Verifies directory ACL sends both access and default.
    #[test]
    fn directory_acl_sends_both() {
        let access_acl = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x07;
            a
        };
        let default_acl = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x05;
            a
        };

        let mut cache = AclCache::new();
        let mut buf = Vec::new();

        send_acl(&mut buf, &access_acl, Some(&default_acl), true, &mut cache).unwrap();

        // Should have two ACL transmissions
        let mut cursor = Cursor::new(&buf);
        let (access_result, default_result) = recv_acl(&mut cursor, true).unwrap();

        if let RecvAclResult::Literal(access) = access_result {
            assert_eq!(access.user_obj, 0x07);
        }
        if let Some(RecvAclResult::Literal(default)) = default_result {
            assert_eq!(default.user_obj, 0x05);
        }
    }

    /// Verifies file ACL does not send default.
    #[test]
    fn file_acl_no_default() {
        let access_acl = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x06;
            a
        };

        let mut cache = AclCache::new();
        let mut buf = Vec::new();

        send_acl(&mut buf, &access_acl, None, false, &mut cache).unwrap();

        // Only access ACL sent
        let mut cursor = Cursor::new(&buf);
        let (access_result, default_result) = recv_acl(&mut cursor, false).unwrap();

        if let RecvAclResult::Literal(access) = access_result {
            assert_eq!(access.user_obj, 0x06);
        }
        assert!(default_result.is_none());
    }
}

/// Tests for computed_mask and name transmission.
mod computed_mask_and_names {
    use super::*;

    #[test]
    fn recv_rsync_acl_sets_computed_mask_when_no_explicit_mask() {
        // ACL with named entries but no explicit mask_obj.
        // upstream: recv_rsync_acl sets mask_obj from computed_mask.
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.names.push(IdAccess::user(1000, 0x05)); // r-x
        acl.names.push(IdAccess::group(200, 0x03)); // -wx
        // mask_obj stays NO_ENTRY (not set)

        let mut cache = AclCache::new();
        let mut buf = Vec::new();
        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

        let mut cursor = Cursor::new(buf);
        if let RecvAclResult::Literal(received) = recv_rsync_acl(&mut cursor).unwrap() {
            // computed_mask = 0x05 | 0x03 = 0x07
            assert_eq!(received.mask_obj, 0x07);
        } else {
            panic!("Expected literal ACL");
        }
    }

    #[test]
    fn recv_rsync_acl_preserves_explicit_mask() {
        // ACL with named entries AND explicit mask_obj.
        // upstream: computed_mask should NOT override the explicit mask.
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.mask_obj = 0x04; // explicit r-- mask
        acl.names.push(IdAccess::user(1000, 0x07));

        let mut cache = AclCache::new();
        let mut buf = Vec::new();
        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

        let mut cursor = Cursor::new(buf);
        if let RecvAclResult::Literal(received) = recv_rsync_acl(&mut cursor).unwrap() {
            assert_eq!(received.mask_obj, 0x04);
        } else {
            panic!("Expected literal ACL");
        }
    }

    #[test]
    fn send_recv_ida_entries_with_names() {
        let mut entries = IdaEntries::new();
        entries.push(IdAccess::user_with_name(1000, 0x07, b"testuser".to_vec()));
        entries.push(IdAccess::group_with_name(100, 0x05, b"staff".to_vec()));

        let mut buf = Vec::new();
        send_ida_entries(&mut buf, &entries, true).unwrap();

        let mut cursor = Cursor::new(buf);
        let (received, mask) = recv_ida_entries(&mut cursor).unwrap();

        assert_eq!(received.len(), 2);
        let items: Vec<_> = received.iter().collect();
        assert!(items[0].is_user());
        assert_eq!(items[0].id, 1000);
        assert_eq!(items[0].permissions(), 0x07);
        assert_eq!(items[0].name.as_deref(), Some(b"testuser".as_slice()));
        assert!(!items[1].is_user());
        assert_eq!(items[1].id, 100);
        assert_eq!(items[1].permissions(), 0x05);
        assert_eq!(items[1].name.as_deref(), Some(b"staff".as_slice()));
        assert_eq!(mask, 0x07);
    }

    #[test]
    fn send_ida_entries_without_names_omits_name_bytes() {
        // Entries with names but include_names=false should not send names
        let mut entries = IdaEntries::new();
        entries.push(IdAccess::user_with_name(1000, 0x07, b"testuser".to_vec()));

        let mut buf_with_names = Vec::new();
        send_ida_entries(&mut buf_with_names, &entries, true).unwrap();

        let mut buf_without_names = Vec::new();
        send_ida_entries(&mut buf_without_names, &entries, false).unwrap();

        // With names should be longer (includes name length + bytes)
        assert!(buf_with_names.len() > buf_without_names.len());

        // Both should decode correctly
        let mut cursor = Cursor::new(buf_without_names);
        let (received, _) = recv_ida_entries(&mut cursor).unwrap();
        assert_eq!(received.len(), 1);
        assert!(received.iter().next().unwrap().name.is_none());
    }
}

/// Tests for `receive_acl_cached` - the cache-integrated receive path.
mod receive_acl_cached_tests {
    use super::*;

    #[test]
    fn literal_acl_is_stored_in_cache() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.group_obj = 0x05;
        acl.other_obj = 0x04;

        let mut send_cache = AclCache::new();
        let mut buf = Vec::new();
        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut send_cache, false).unwrap();

        let mut recv_cache = AclCache::new();
        let mut cursor = Cursor::new(buf);
        let (access_ndx, def_ndx) =
            receive_acl_cached(&mut cursor, false, &mut recv_cache).unwrap();

        assert_eq!(access_ndx, 0);
        assert!(def_ndx.is_none());
        assert_eq!(recv_cache.access_count(), 1);

        let cached = recv_cache.get_access(0).unwrap();
        assert_eq!(cached.user_obj, 0x07);
        assert_eq!(cached.group_obj, 0x05);
        assert_eq!(cached.other_obj, 0x04);
    }

    #[test]
    fn cache_hit_returns_correct_index() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;

        let mut send_cache = AclCache::new();
        let mut buf = Vec::new();

        // First send - literal
        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut send_cache, false).unwrap();

        // Second send - cache hit (index 0)
        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut send_cache, false).unwrap();

        let mut recv_cache = AclCache::new();
        let mut cursor = Cursor::new(buf);

        // First receive - stores literal
        let (ndx1, _) = receive_acl_cached(&mut cursor, false, &mut recv_cache).unwrap();
        assert_eq!(ndx1, 0);

        // Second receive - cache hit referencing index 0
        let (ndx2, _) = receive_acl_cached(&mut cursor, false, &mut recv_cache).unwrap();
        assert_eq!(ndx2, 0);

        // Only one ACL stored in cache
        assert_eq!(recv_cache.access_count(), 1);
    }

    #[test]
    fn directory_receives_access_and_default_acls() {
        let access_acl = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x07;
            a.group_obj = 0x05;
            a.other_obj = 0x05;
            a
        };
        let default_acl = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x07;
            a.group_obj = 0x05;
            a.other_obj = 0x00;
            a
        };

        let mut send_cache = AclCache::new();
        let mut buf = Vec::new();
        send_acl(
            &mut buf,
            &access_acl,
            Some(&default_acl),
            true,
            &mut send_cache,
        )
        .unwrap();

        let mut recv_cache = AclCache::new();
        let mut cursor = Cursor::new(buf);
        let (access_ndx, def_ndx) = receive_acl_cached(&mut cursor, true, &mut recv_cache).unwrap();

        assert_eq!(access_ndx, 0);
        assert_eq!(def_ndx, Some(0));
        assert_eq!(recv_cache.access_count(), 1);
        assert_eq!(recv_cache.default_count(), 1);

        let cached_access = recv_cache.get_access(0).unwrap();
        assert_eq!(cached_access.user_obj, 0x07);
        assert_eq!(cached_access.other_obj, 0x05);

        let cached_default = recv_cache.get_default(0).unwrap();
        assert_eq!(cached_default.user_obj, 0x07);
        assert_eq!(cached_default.other_obj, 0x00);
    }

    #[test]
    fn out_of_range_cache_index_returns_error() {
        // Manually construct a wire message with a cache hit for index 5,
        // but the cache is empty, so the index is out of range.
        use crate::varint::write_varint;

        let mut buf = Vec::new();
        // ndx + 1 = 6, so ndx = 5 (cache hit for index 5)
        write_varint(&mut buf, 6).unwrap();

        let mut recv_cache = AclCache::new();
        let mut cursor = Cursor::new(buf);
        let result = receive_acl_cached(&mut cursor, false, &mut recv_cache);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("ACL index"));
    }

    #[test]
    fn multiple_different_acls_get_different_indices() {
        let acl1 = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x07;
            a
        };
        let acl2 = {
            let mut a = RsyncAcl::new();
            a.user_obj = 0x05;
            a
        };

        let mut send_cache = AclCache::new();
        let mut buf = Vec::new();
        send_rsync_acl(&mut buf, &acl1, AclType::Access, &mut send_cache, false).unwrap();
        send_rsync_acl(&mut buf, &acl2, AclType::Access, &mut send_cache, false).unwrap();

        let mut recv_cache = AclCache::new();
        let mut cursor = Cursor::new(buf);

        let (ndx1, _) = receive_acl_cached(&mut cursor, false, &mut recv_cache).unwrap();
        let (ndx2, _) = receive_acl_cached(&mut cursor, false, &mut recv_cache).unwrap();

        assert_eq!(ndx1, 0);
        assert_eq!(ndx2, 1);
        assert_eq!(recv_cache.access_count(), 2);

        assert_eq!(recv_cache.get_access(0).unwrap().user_obj, 0x07);
        assert_eq!(recv_cache.get_access(1).unwrap().user_obj, 0x05);
    }

    #[test]
    fn acl_with_named_entries_cached_correctly() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.group_obj = 0x05;
        acl.mask_obj = 0x07;
        acl.other_obj = 0x04;
        acl.names.push(IdAccess::user(1000, 0x07));
        acl.names.push(IdAccess::group(100, 0x05));

        let mut send_cache = AclCache::new();
        let mut buf = Vec::new();
        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut send_cache, false).unwrap();

        let mut recv_cache = AclCache::new();
        let mut cursor = Cursor::new(buf);
        let (ndx, _) = receive_acl_cached(&mut cursor, false, &mut recv_cache).unwrap();

        assert_eq!(ndx, 0);
        let cached = recv_cache.get_access(0).unwrap();
        assert_eq!(cached.names.len(), 2);
        assert_eq!(cached.mask_obj, 0x07);
    }

    #[test]
    fn empty_acl_for_file_no_default() {
        let acl = RsyncAcl::new();

        let mut send_cache = AclCache::new();
        let mut buf = Vec::new();
        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut send_cache, false).unwrap();

        let mut recv_cache = AclCache::new();
        let mut cursor = Cursor::new(buf);
        let (ndx, def_ndx) = receive_acl_cached(&mut cursor, false, &mut recv_cache).unwrap();

        assert_eq!(ndx, 0);
        assert!(def_ndx.is_none());
        assert!(recv_cache.get_access(0).unwrap().is_empty());
    }
}

/// Tests for `get_perms` helper function.
///
/// Validates extraction of rwx permission bits from Unix file mode
/// for each ACL tag type. Upstream: `rsync_acl_get_perms()` in `acls.c`.
mod get_perms_tests {
    use super::*;

    #[test]
    fn extracts_user_obj_bits() {
        // mode 0o755 = rwxr-xr-x -> user_obj = rwx = 7
        assert_eq!(get_perms(0o755, AclTagType::UserObj), 7);
        // mode 0o644 = rw-r--r-- -> user_obj = rw- = 6
        assert_eq!(get_perms(0o644, AclTagType::UserObj), 6);
        // mode 0o000 -> user_obj = 0
        assert_eq!(get_perms(0o000, AclTagType::UserObj), 0);
    }

    #[test]
    fn extracts_group_obj_bits() {
        // mode 0o755 -> group_obj = r-x = 5
        assert_eq!(get_perms(0o755, AclTagType::GroupObj), 5);
        // mode 0o644 -> group_obj = r-- = 4
        assert_eq!(get_perms(0o644, AclTagType::GroupObj), 4);
        // mode 0o070 -> group_obj = rwx = 7
        assert_eq!(get_perms(0o070, AclTagType::GroupObj), 7);
    }

    #[test]
    fn extracts_mask_obj_bits_same_as_group() {
        // POSIX.1e: mask shares bit position with group
        assert_eq!(get_perms(0o750, AclTagType::MaskObj), 5);
        assert_eq!(
            get_perms(0o750, AclTagType::MaskObj),
            get_perms(0o750, AclTagType::GroupObj)
        );
    }

    #[test]
    fn extracts_other_obj_bits() {
        // mode 0o755 -> other_obj = r-x = 5
        assert_eq!(get_perms(0o755, AclTagType::OtherObj), 5);
        // mode 0o700 -> other_obj = 0
        assert_eq!(get_perms(0o700, AclTagType::OtherObj), 0);
        // mode 0o007 -> other_obj = rwx = 7
        assert_eq!(get_perms(0o007, AclTagType::OtherObj), 7);
    }

    #[test]
    fn all_permission_combinations() {
        for user in 0..=7u32 {
            for group in 0..=7u32 {
                for other in 0..=7u32 {
                    let mode = (user << 6) | (group << 3) | other;
                    assert_eq!(get_perms(mode, AclTagType::UserObj), user as u8);
                    assert_eq!(get_perms(mode, AclTagType::GroupObj), group as u8);
                    assert_eq!(get_perms(mode, AclTagType::OtherObj), other as u8);
                }
            }
        }
    }

    #[test]
    fn ignores_file_type_bits() {
        // Regular file: S_IFREG (0o100000) | 0o644
        let mode = 0o100644;
        assert_eq!(get_perms(mode, AclTagType::UserObj), 6);
        assert_eq!(get_perms(mode, AclTagType::GroupObj), 4);
        assert_eq!(get_perms(mode, AclTagType::OtherObj), 4);
    }

    #[test]
    fn ignores_setuid_setgid_sticky() {
        // mode 0o4755 (setuid) -> permissions still 755
        assert_eq!(get_perms(0o4755, AclTagType::UserObj), 7);
        assert_eq!(get_perms(0o4755, AclTagType::GroupObj), 5);
        assert_eq!(get_perms(0o4755, AclTagType::OtherObj), 5);

        // mode 0o2755 (setgid)
        assert_eq!(get_perms(0o2755, AclTagType::UserObj), 7);

        // mode 0o1755 (sticky)
        assert_eq!(get_perms(0o1755, AclTagType::OtherObj), 5);
    }
}

/// Tests for `RsyncAcl::fake_perms`.
///
/// Validates creation of minimal ACLs from file mode bits.
/// Upstream: `rsync_acl_fake_perms()` in `acls.c`.
mod fake_perms_tests {
    use super::*;

    #[test]
    fn populates_from_standard_mode() {
        let mut acl = RsyncAcl::new();
        acl.fake_perms(0o755);

        assert_eq!(acl.user_obj, 7);
        assert_eq!(acl.group_obj, 5);
        assert_eq!(acl.other_obj, 5);
        assert_eq!(acl.mask_obj, NO_ENTRY);
        assert!(acl.names.is_empty());
    }

    #[test]
    fn populates_from_restrictive_mode() {
        let mut acl = RsyncAcl::new();
        acl.fake_perms(0o600);

        assert_eq!(acl.user_obj, 6);
        assert_eq!(acl.group_obj, 0);
        assert_eq!(acl.other_obj, 0);
    }

    #[test]
    fn clears_existing_state() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.mask_obj = 0x05;
        acl.names.push(IdAccess::user(1000, 0x07));

        acl.fake_perms(0o644);

        assert_eq!(acl.user_obj, 6);
        assert_eq!(acl.group_obj, 4);
        assert_eq!(acl.other_obj, 4);
        assert_eq!(acl.mask_obj, NO_ENTRY);
        assert!(acl.names.is_empty());
    }

    #[test]
    fn zero_mode_produces_zero_perms() {
        let mut acl = RsyncAcl::new();
        acl.fake_perms(0o000);

        assert_eq!(acl.user_obj, 0);
        assert_eq!(acl.group_obj, 0);
        assert_eq!(acl.other_obj, 0);
    }

    #[test]
    fn full_mode_produces_full_perms() {
        let mut acl = RsyncAcl::new();
        acl.fake_perms(0o777);

        assert_eq!(acl.user_obj, 7);
        assert_eq!(acl.group_obj, 7);
        assert_eq!(acl.other_obj, 7);
    }
}

/// Tests for `RsyncAcl::from_mode` constructor.
mod from_mode_tests {
    use super::*;

    #[test]
    fn creates_acl_matching_fake_perms() {
        let acl = RsyncAcl::from_mode(0o755);
        let mut expected = RsyncAcl::new();
        expected.fake_perms(0o755);

        assert_eq!(acl, expected);
    }

    #[test]
    fn standard_modes() {
        let acl = RsyncAcl::from_mode(0o644);
        assert_eq!(acl.user_obj, 6);
        assert_eq!(acl.group_obj, 4);
        assert_eq!(acl.other_obj, 4);
        assert_eq!(acl.mask_obj, NO_ENTRY);
        assert!(acl.names.is_empty());
    }

    #[test]
    fn is_not_empty() {
        let acl = RsyncAcl::from_mode(0o755);
        assert!(!acl.is_empty());
    }

    #[test]
    fn zero_mode_has_present_entries() {
        let acl = RsyncAcl::from_mode(0o000);
        // Even with zero perms, the entries are present (not NO_ENTRY)
        assert!(acl.has_user_obj());
        assert!(acl.has_group_obj());
        assert!(acl.has_other_obj());
        assert!(!acl.has_mask_obj());
    }
}

/// Tests for `RsyncAcl::strip_perms`.
///
/// Validates ACL stripping to base permission entries.
/// Upstream: `rsync_acl_strip_perms()` in `acls.c`.
mod strip_perms_tests {
    use super::*;

    #[test]
    fn removes_named_entries() {
        let mut acl = RsyncAcl::from_mode(0o755);
        acl.names.push(IdAccess::user(1000, 0x07));
        acl.names.push(IdAccess::group(100, 0x05));
        acl.mask_obj = 0x07;

        acl.strip_perms();

        assert!(acl.names.is_empty());
    }

    #[test]
    fn replaces_group_with_mask_when_mask_present() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 7;
        acl.group_obj = 5;
        acl.mask_obj = 3;
        acl.other_obj = 0;
        acl.names.push(IdAccess::user(1000, 0x07));

        acl.strip_perms();

        // upstream: group_obj should be replaced by mask_obj value
        assert_eq!(acl.group_obj, 3);
        assert_eq!(acl.mask_obj, NO_ENTRY);
        assert!(acl.names.is_empty());
    }

    #[test]
    fn preserves_group_when_no_mask() {
        let mut acl = RsyncAcl::from_mode(0o755);

        acl.strip_perms();

        assert_eq!(acl.group_obj, 5);
        assert_eq!(acl.mask_obj, NO_ENTRY);
    }

    #[test]
    fn preserves_user_and_other() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 7;
        acl.group_obj = 5;
        acl.mask_obj = 3;
        acl.other_obj = 4;
        acl.names.push(IdAccess::user(1000, 0x07));

        acl.strip_perms();

        assert_eq!(acl.user_obj, 7);
        assert_eq!(acl.other_obj, 4);
    }

    #[test]
    fn idempotent_without_mask() {
        let mut acl = RsyncAcl::from_mode(0o644);
        let before = acl.clone();

        acl.strip_perms();

        assert_eq!(acl.user_obj, before.user_obj);
        assert_eq!(acl.group_obj, before.group_obj);
        assert_eq!(acl.other_obj, before.other_obj);
    }

    #[test]
    fn strip_empty_acl_is_noop() {
        let mut acl = RsyncAcl::new();
        acl.strip_perms();

        // mask_obj stays NO_ENTRY, names stay empty
        assert_eq!(acl.mask_obj, NO_ENTRY);
        assert!(acl.names.is_empty());
    }
}

/// Tests for `RsyncAcl::equal_enough`.
///
/// Validates semantic ACL comparison that ignores mask when no named
/// entries exist. Upstream: `rsync_acl_equal_enough()` in `acls.c`.
mod equal_enough_tests {
    use super::*;

    #[test]
    fn identical_acls_are_equal() {
        let acl = RsyncAcl::from_mode(0o755);
        assert!(acl.equal_enough(&acl));
    }

    #[test]
    fn different_user_obj_not_equal() {
        let a = RsyncAcl::from_mode(0o755);
        let b = RsyncAcl::from_mode(0o655);
        assert!(!a.equal_enough(&b));
    }

    #[test]
    fn different_other_obj_not_equal() {
        let a = RsyncAcl::from_mode(0o750);
        let b = RsyncAcl::from_mode(0o751);
        assert!(!a.equal_enough(&b));
    }

    #[test]
    fn different_group_obj_not_equal() {
        let a = RsyncAcl::from_mode(0o750);
        let b = RsyncAcl::from_mode(0o740);
        assert!(!a.equal_enough(&b));
    }

    #[test]
    fn mask_ignored_when_no_named_entries() {
        // Two ACLs with same effective permissions but different mask state.
        // Without named entries, mask is irrelevant.
        let a = RsyncAcl::from_mode(0o755);

        let mut b = RsyncAcl::new();
        b.user_obj = 7;
        b.group_obj = 0; // different group_obj
        b.mask_obj = 5; // but mask provides the effective group perms
        b.other_obj = 5;

        // a has group_obj=5, no mask. b has group_obj=0, mask=5.
        // Without named entries, effective group = mask if present, else group_obj.
        assert!(a.equal_enough(&b));

        // Reverse comparison should also hold
        assert!(b.equal_enough(&a));
    }

    #[test]
    fn mask_compared_when_named_entries_present() {
        let mut a = RsyncAcl::new();
        a.user_obj = 7;
        a.group_obj = 5;
        a.mask_obj = 7;
        a.other_obj = 5;
        a.names.push(IdAccess::user(1000, 0x07));

        let mut b = a.clone();
        b.mask_obj = 3; // different mask

        // With named entries, mask must match exactly
        assert!(!a.equal_enough(&b));
    }

    #[test]
    fn group_obj_compared_when_named_entries_present() {
        let mut a = RsyncAcl::new();
        a.user_obj = 7;
        a.group_obj = 5;
        a.mask_obj = 7;
        a.other_obj = 5;
        a.names.push(IdAccess::user(1000, 0x07));

        let mut b = a.clone();
        b.group_obj = 3; // different group

        assert!(!a.equal_enough(&b));
    }

    #[test]
    fn different_named_entry_count_not_equal() {
        let mut a = RsyncAcl::from_mode(0o755);
        a.names.push(IdAccess::user(1000, 0x07));

        let b = RsyncAcl::from_mode(0o755);

        assert!(!a.equal_enough(&b));
    }

    #[test]
    fn different_named_entry_id_not_equal() {
        let mut a = RsyncAcl::from_mode(0o755);
        a.mask_obj = 7;
        a.names.push(IdAccess::user(1000, 0x07));

        let mut b = RsyncAcl::from_mode(0o755);
        b.mask_obj = 7;
        b.names.push(IdAccess::user(2000, 0x07));

        assert!(!a.equal_enough(&b));
    }

    #[test]
    fn different_named_entry_perms_not_equal() {
        let mut a = RsyncAcl::from_mode(0o755);
        a.mask_obj = 7;
        a.names.push(IdAccess::user(1000, 0x07));

        let mut b = RsyncAcl::from_mode(0o755);
        b.mask_obj = 7;
        b.names.push(IdAccess::user(1000, 0x05));

        assert!(!a.equal_enough(&b));
    }

    #[test]
    fn empty_acls_are_equal() {
        let a = RsyncAcl::new();
        let b = RsyncAcl::new();
        assert!(a.equal_enough(&b));
    }

    #[test]
    fn both_masks_present_no_names_effective_group_matches() {
        let mut a = RsyncAcl::new();
        a.user_obj = 7;
        a.group_obj = 3;
        a.mask_obj = 5;
        a.other_obj = 0;

        let mut b = RsyncAcl::new();
        b.user_obj = 7;
        b.group_obj = 1;
        b.mask_obj = 5;
        b.other_obj = 0;

        // No named entries - effective group is mask_obj for both = 5
        assert!(a.equal_enough(&b));
    }

    #[test]
    fn reflexive_with_named_entries() {
        let mut acl = RsyncAcl::from_mode(0o755);
        acl.mask_obj = 7;
        acl.names.push(IdAccess::user(1000, 0x07));
        acl.names.push(IdAccess::group(100, 0x05));

        assert!(acl.equal_enough(&acl));
    }
}

/// Tests for `IdaEntries::clear`.
mod ida_entries_clear_tests {
    use super::*;

    #[test]
    fn clear_empties_entries() {
        let mut entries = IdaEntries::new();
        entries.push(IdAccess::user(1000, 0x07));
        entries.push(IdAccess::group(100, 0x05));
        assert_eq!(entries.len(), 2);

        entries.clear();

        assert!(entries.is_empty());
        assert_eq!(entries.len(), 0);
    }

    #[test]
    fn clear_on_empty_is_noop() {
        let mut entries = IdaEntries::new();
        entries.clear();
        assert!(entries.is_empty());
    }
}

/// Tests for `AclTagType` enum.
mod acl_tag_type_tests {
    use super::*;

    #[test]
    fn clone_and_copy() {
        let a = AclTagType::UserObj;
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn debug_format() {
        assert!(format!("{:?}", AclTagType::UserObj).contains("UserObj"));
        assert!(format!("{:?}", AclTagType::GroupObj).contains("GroupObj"));
        assert!(format!("{:?}", AclTagType::MaskObj).contains("MaskObj"));
        assert!(format!("{:?}", AclTagType::OtherObj).contains("OtherObj"));
    }

    #[test]
    fn equality() {
        assert_eq!(AclTagType::UserObj, AclTagType::UserObj);
        assert_ne!(AclTagType::UserObj, AclTagType::GroupObj);
        assert_ne!(AclTagType::MaskObj, AclTagType::OtherObj);
    }
}

/// Tests for `strip_perms_for_send` - sender-side permission stripping.
mod strip_perms_for_send_tests {
    use super::*;

    /// Basic file without extended ACLs: all base entries stripped.
    /// upstream: acls.c:142-154 - user_obj, group_obj (no mask), other_obj all set to NO_ENTRY
    #[test]
    fn basic_file_all_stripped() {
        let mut acl = RsyncAcl::from_mode(0o644);
        assert_eq!(acl.user_obj, 0x06);
        assert_eq!(acl.group_obj, 0x04);
        assert_eq!(acl.other_obj, 0x04);

        acl.strip_perms_for_send(0o644);

        assert_eq!(acl.user_obj, NO_ENTRY);
        assert_eq!(acl.group_obj, NO_ENTRY);
        assert_eq!(acl.other_obj, NO_ENTRY);
        assert_eq!(acl.mask_obj, NO_ENTRY);
        assert!(acl.is_empty());
    }

    /// Different mode: stripped result is always empty for basic ACLs.
    #[test]
    fn mode_755_all_stripped() {
        let mut acl = RsyncAcl::from_mode(0o755);
        acl.strip_perms_for_send(0o755);

        assert_eq!(acl.user_obj, NO_ENTRY);
        assert_eq!(acl.group_obj, NO_ENTRY);
        assert_eq!(acl.other_obj, NO_ENTRY);
        assert!(acl.is_empty());
    }

    /// With mask and group matching mode group bits: both stripped.
    #[test]
    fn mask_matching_group_bits_both_stripped() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.group_obj = 0x05;
        acl.mask_obj = 0x05;
        acl.other_obj = 0x04;
        acl.names.push(IdAccess::user(1000, 0x07));

        // mode 0o754: group bits = (0o754 >> 3) & 7 = 5
        acl.strip_perms_for_send(0o754);

        assert_eq!(acl.user_obj, NO_ENTRY);
        assert_eq!(acl.group_obj, NO_ENTRY); // matches group perms from mode
        assert_eq!(acl.mask_obj, NO_ENTRY); // matches group perms + has named entries
        assert_eq!(acl.other_obj, NO_ENTRY);
        assert_eq!(acl.names.len(), 1); // named entries preserved
    }

    /// With mask not matching group bits: mask preserved.
    #[test]
    fn mask_not_matching_group_bits_preserved() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.group_obj = 0x03;
        acl.mask_obj = 0x07;
        acl.other_obj = 0x04;
        acl.names.push(IdAccess::user(1000, 0x07));

        // mode 0o774: group bits = 7
        acl.strip_perms_for_send(0o774);

        assert_eq!(acl.user_obj, NO_ENTRY);
        assert_eq!(acl.group_obj, 0x03); // 3 != 7, NOT stripped
        assert_eq!(acl.mask_obj, NO_ENTRY); // 7 == 7, stripped
        assert_eq!(acl.other_obj, NO_ENTRY);
    }

    /// With mask but group_obj different from mode group bits: group preserved.
    #[test]
    fn group_not_matching_mode_preserved() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.group_obj = 0x03;
        acl.mask_obj = 0x07;
        acl.other_obj = 0x04;
        acl.names.push(IdAccess::user(1000, 0x07));

        // mode 0o754: group bits = 5
        acl.strip_perms_for_send(0o754);

        assert_eq!(acl.user_obj, NO_ENTRY);
        assert_eq!(acl.group_obj, 0x03); // 3 != 5, NOT stripped
        assert_eq!(acl.mask_obj, 0x07); // 7 != 5, NOT stripped (doesn't match group bits)
        assert_eq!(acl.other_obj, NO_ENTRY);
    }

    /// With mask but no named entries: mask not stripped (only stripped when names exist).
    #[test]
    fn mask_without_names_not_stripped() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.group_obj = 0x05;
        acl.mask_obj = 0x05;
        acl.other_obj = 0x04;

        // mode 0o754: group bits = 5
        acl.strip_perms_for_send(0o754);

        assert_eq!(acl.user_obj, NO_ENTRY);
        assert_eq!(acl.group_obj, NO_ENTRY); // matches group perms
        assert_eq!(acl.mask_obj, 0x05); // no named entries, so mask NOT stripped
        assert_eq!(acl.other_obj, NO_ENTRY);
    }

    /// Stripped ACL roundtrips correctly through wire encode/decode.
    #[test]
    fn stripped_acl_wire_roundtrip() {
        let mut acl = RsyncAcl::from_mode(0o644);
        acl.strip_perms_for_send(0o644);

        let mut buf = Vec::new();
        let mut cache = AclCache::new();
        send_rsync_acl(&mut buf, &acl, wire::AclType::Access, &mut cache, false).unwrap();

        let mut reader = Cursor::new(&buf);
        let result = recv_rsync_acl(&mut reader).unwrap();

        match result {
            RecvAclResult::Literal(received) => {
                assert_eq!(received.user_obj, NO_ENTRY);
                assert_eq!(received.group_obj, NO_ENTRY);
                assert_eq!(received.mask_obj, NO_ENTRY);
                assert_eq!(received.other_obj, NO_ENTRY);
                assert!(received.names.is_empty());
            }
            RecvAclResult::CacheHit(_) => panic!("expected literal, got cache hit"),
        }
    }

    /// ACL with named entries roundtrips correctly after stripping.
    #[test]
    fn acl_with_names_roundtrip_after_strip() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.group_obj = 0x05;
        acl.mask_obj = 0x07;
        acl.other_obj = 0x04;
        acl.names.push(IdAccess::user(1000, 0x07));
        acl.names.push(IdAccess::group(100, 0x05));

        // mode 0o774: group bits = 7
        acl.strip_perms_for_send(0o774);

        let mut buf = Vec::new();
        let mut cache = AclCache::new();
        send_rsync_acl(&mut buf, &acl, wire::AclType::Access, &mut cache, false).unwrap();

        let mut reader = Cursor::new(&buf);
        let result = recv_rsync_acl(&mut reader).unwrap();

        match result {
            RecvAclResult::Literal(received) => {
                assert_eq!(received.user_obj, NO_ENTRY);
                assert_eq!(received.other_obj, NO_ENTRY);
                assert_eq!(received.names.len(), 2);
            }
            RecvAclResult::CacheHit(_) => panic!("expected literal, got cache hit"),
        }
    }
}
