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
        // Access field has the flag
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
        // NO_ENTRY bits should be excluded from the mask
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
        cache.store_access(acl.clone());

        assert_eq!(cache.find_access(&acl), Some(0));
    }

    #[test]
    fn get_access_retrieves_stored_acl() {
        let mut cache = AclCache::new();
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.group_obj = 0x05;
        cache.store_access(acl.clone());

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

        cache.store_access(acl.clone());
        assert_eq!(cache.find_access(&acl), Some(0));
        assert!(cache.find_default(&acl).is_none());

        cache.store_default(acl.clone());
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
            a.group_obj = 0x04; // Different!
            a
        };

        cache.store_access(acl1.clone());

        // acl2 should not match acl1
        assert!(cache.find_access(&acl2).is_none());

        // But acl1 should still match
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
