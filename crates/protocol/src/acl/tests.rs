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

/// Tests for the `--fake-super` xattr byte format (`%aacl`/`%dacl` blobs).
///
/// Upstream: `acls.c:472-509` `get_rsync_acl()` and `acls.c:933-970`
/// `set_rsync_acl()` (the `am_root < 0` branches).
mod fake_super_bytes_tests {
    use super::*;

    #[test]
    fn empty_acl_encodes_to_16_byte_header() {
        let acl = RsyncAcl::new();
        let bytes = acl.to_fake_super_bytes();
        assert_eq!(bytes.len(), 16);
        // All four base fields are NO_ENTRY (0x80), zero-extended to u32 LE.
        assert_eq!(&bytes[0..4], &[0x80, 0, 0, 0]);
        assert_eq!(&bytes[4..8], &[0x80, 0, 0, 0]);
        assert_eq!(&bytes[8..12], &[0x80, 0, 0, 0]);
        assert_eq!(&bytes[12..16], &[0x80, 0, 0, 0]);
    }

    #[test]
    fn roundtrip_base_entries_only() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.group_obj = 0x05;
        acl.mask_obj = 0x07;
        acl.other_obj = 0x04;

        let bytes = acl.to_fake_super_bytes();
        let decoded = RsyncAcl::from_fake_super_bytes(&bytes).expect("valid blob");
        assert_eq!(decoded, acl);
    }

    #[test]
    fn roundtrip_with_named_entries() {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.group_obj = 0x05;
        acl.mask_obj = 0x07;
        acl.other_obj = 0x04;
        acl.names.push(IdAccess::user(1000, 0x07));
        acl.names.push(IdAccess::group(100, 0x05));

        let bytes = acl.to_fake_super_bytes();
        assert_eq!(bytes.len(), 16 + 2 * 8);
        let decoded = RsyncAcl::from_fake_super_bytes(&bytes).expect("valid blob");
        assert_eq!(decoded, acl);
        // The user entry's NAME_IS_USER bit (bit 31) must survive the round trip
        // since it distinguishes user from group entries in the ida list.
        assert!(decoded.names.iter().next().unwrap().is_user());
    }

    #[test]
    fn golden_bytes_mixed_base_and_named_entries() {
        // Non-trivial base perms (mask_obj absent) plus one named-user and
        // one named-group entry, pinned against a hand-computed byte
        // literal so a byte-order or field-order regression in either
        // encode or decode direction cannot hide behind a self-consistent
        // round trip.
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07; // rwx
        acl.group_obj = 0x05; // r-x
        acl.other_obj = 0x04; // r--
        // mask_obj left at NO_ENTRY (0x80) from RsyncAcl::new().
        acl.names.push(IdAccess::user(1000, 0x07)); // named user, rwx
        acl.names.push(IdAccess::group(100, 0x05)); // named group, r-x

        #[rustfmt::skip]
        let expected: Vec<u8> = vec![
            // upstream: acls.c:962 SIVAL(buf, 0, user_obj) -> 0x07
            0x07, 0x00, 0x00, 0x00,
            // upstream: acls.c:963 SIVAL(buf, 4, group_obj) -> 0x05
            0x05, 0x00, 0x00, 0x00,
            // upstream: acls.c:964 SIVAL(buf, 8, mask_obj) -> NO_ENTRY (0x80)
            0x80, 0x00, 0x00, 0x00,
            // upstream: acls.c:965 SIVAL(buf, 12, other_obj) -> 0x04
            0x04, 0x00, 0x00, 0x00,
            // upstream: acls.c:971 SIVAL(bp, 0, ida->id) -> 1000 (named user)
            0xE8, 0x03, 0x00, 0x00,
            // upstream: acls.c:972 SIVAL(bp, 4, ida->access) -> 0x07 | NAME_IS_USER
            0x07, 0x00, 0x00, 0x80,
            // upstream: acls.c:971 SIVAL(bp, 0, ida->id) -> 100 (named group)
            0x64, 0x00, 0x00, 0x00,
            // upstream: acls.c:972 SIVAL(bp, 4, ida->access) -> 0x05 (no NAME_IS_USER)
            0x05, 0x00, 0x00, 0x00,
        ];

        assert_eq!(acl.to_fake_super_bytes(), expected);

        // Decode the SAME literal (not the just-produced `bytes`) so the
        // two directions are pinned against one shared ground truth rather
        // than merely agreeing with each other.
        let decoded = RsyncAcl::from_fake_super_bytes(&expected).expect("valid blob");
        assert_eq!(decoded, acl);
        let mut names = decoded.names.iter();
        let user_entry = names.next().expect("named user entry");
        assert!(user_entry.is_user());
        assert_eq!(user_entry.id, 1000);
        let group_entry = names.next().expect("named group entry");
        assert!(!group_entry.is_user());
        assert_eq!(group_entry.id, 100);
    }

    #[test]
    fn from_bytes_rejects_short_header() {
        assert!(RsyncAcl::from_fake_super_bytes(&[0u8; 15]).is_none());
        assert!(RsyncAcl::from_fake_super_bytes(&[]).is_none());
    }

    #[test]
    fn from_bytes_rejects_misaligned_trailer() {
        // 16-byte header plus a partial (non-multiple-of-8) named entry.
        assert!(RsyncAcl::from_fake_super_bytes(&[0u8; 20]).is_none());
        assert!(RsyncAcl::from_fake_super_bytes(&[0u8; 23]).is_none());
    }

    #[test]
    fn from_bytes_accepts_exact_multiple_of_8_trailer() {
        assert!(RsyncAcl::from_fake_super_bytes(&[0u8; 16]).is_some());
        assert!(RsyncAcl::from_fake_super_bytes(&[0u8; 24]).is_some());
        assert!(RsyncAcl::from_fake_super_bytes(&[0u8; 32]).is_some());
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
        let mut cache = AclCache::new();
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
        let result = recv_rsync_acl(&mut cursor, AclType::Access).unwrap();

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
            if let RecvAclResult::Literal(received) =
                recv_rsync_acl(&mut cursor, AclType::Access).unwrap()
            {
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
        send_acl(&mut buf, &access_acl, None, false, &mut cache, false).unwrap();

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
        send_acl(
            &mut buf,
            &access_acl,
            Some(&default_acl),
            true,
            &mut cache,
            false,
        )
        .unwrap();

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
            match recv_rsync_acl(&mut cursor, AclType::Access).unwrap() {
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
        if let RecvAclResult::CacheHit(idx) = recv_rsync_acl(&mut cursor, AclType::Access).unwrap()
        {
            assert_eq!(idx, 0);
        }

        buf.clear();
        send_rsync_acl(&mut buf, &acl2, AclType::Access, &mut cache, false).unwrap();
        let mut cursor = Cursor::new(&buf);
        if let RecvAclResult::CacheHit(idx) = recv_rsync_acl(&mut cursor, AclType::Access).unwrap()
        {
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
        if let RecvAclResult::Literal(received) =
            recv_rsync_acl(&mut cursor, AclType::Access).unwrap()
        {
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
        if let RecvAclResult::Literal(received) =
            recv_rsync_acl(&mut cursor, AclType::Access).unwrap()
        {
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
        if let RecvAclResult::Literal(received) =
            recv_rsync_acl(&mut cursor, AclType::Access).unwrap()
        {
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

        send_acl(
            &mut buf,
            &access_acl,
            Some(&default_acl),
            true,
            &mut cache,
            false,
        )
        .unwrap();

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

        send_acl(&mut buf, &access_acl, None, false, &mut cache, false).unwrap();

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
    fn recv_access_acl_leaves_mask_unset_for_mode_reconstruction() {
        // ACCESS ACL with named entries but no explicit mask on the wire.
        //
        // upstream: recv_rsync_acl(type=ACCESS) sets mask_obj = (mode>>3)&7,
        // the authoritative source, because rsync_acl_strip_perms() only drops
        // the mask when it equals those mode bits (acls.c:150-151, 770-773).
        // oc defers that mode-based fill to reconstruct_acl() at apply time, so
        // the wire decode must leave the mask as NO_ENTRY rather than folding in
        // the OR of the named-entry access bits (which would narrow the mask to
        // the named user's perms whenever the true mask exceeds them - bug #251).
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.names.push(IdAccess::user(1000, 0x05)); // r-x
        acl.names.push(IdAccess::group(200, 0x03)); // -wx
        // mask_obj stays NO_ENTRY (not set)

        let mut cache = AclCache::new();
        let mut buf = Vec::new();
        send_rsync_acl(&mut buf, &acl, AclType::Access, &mut cache, false).unwrap();

        let mut cursor = Cursor::new(buf);
        if let RecvAclResult::Literal(received) =
            recv_rsync_acl(&mut cursor, AclType::Access).unwrap()
        {
            // The OR of the named entries would be 0x05 | 0x03 = 0x07. The old
            // (buggy) decode stored that here, pre-empting the correct
            // mode-based mask. The fixed decode leaves it unset for
            // reconstruct_acl() to fill from the file mode.
            assert_eq!(received.mask_obj, NO_ENTRY);
        } else {
            panic!("Expected literal ACL");
        }
    }

    #[test]
    fn recv_default_acl_computes_mask_from_named_entries_and_group() {
        // DEFAULT ACL with named entries but no explicit mask. No file mode is
        // available (upstream passes mode 0), so upstream folds the group object
        // into the OR of the named-entry access bits:
        //   computed_mask_bits |= group_obj & ~NO_ENTRY   (acls.c:774-777)
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0x07;
        acl.group_obj = 0x04; // r--
        acl.other_obj = 0x00;
        acl.names.push(IdAccess::user(1000, 0x05)); // r-x
        acl.names.push(IdAccess::group(200, 0x03)); // -wx
        // mask_obj stays NO_ENTRY (not set)

        let mut cache = AclCache::new();
        let mut buf = Vec::new();
        send_rsync_acl(&mut buf, &acl, AclType::Default, &mut cache, false).unwrap();

        let mut cursor = Cursor::new(buf);
        if let RecvAclResult::Literal(received) =
            recv_rsync_acl(&mut cursor, AclType::Default).unwrap()
        {
            // 0x05 | 0x03 (named OR) | 0x04 (group_obj) = 0x07
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
        if let RecvAclResult::Literal(received) =
            recv_rsync_acl(&mut cursor, AclType::Access).unwrap()
        {
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
            false,
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
/// Encodes the exact contract of upstream `rsync_acl_equal_enough()`
/// (`acls.c` lines 205-224): the first ACL is fully populated, the second may
/// be a condensed ACL with `NO_ENTRY` fields, and the file mode recovers a
/// stripped `group_obj`. Only `mask_obj` presence, the `group_obj` extended
/// entry, and the named user/group entries participate; `user_obj` and
/// `other_obj` are deliberately left to the mode-preservation code.
///
/// Each `WHY` note records the upstream semantics the case pins. The
/// discriminating cases below (all but the two named-entry guards) return the
/// opposite result under the old, non-upstream logic that compared
/// `user_obj`/`other_obj` and mask values - proving the fix is exercised.
mod equal_enough_tests {
    use super::*;

    #[test]
    fn reflexive() {
        // WHY: an ACL is trivially equal enough to itself.
        let acl = RsyncAcl::from_mode(0o755);
        assert!(acl.equal_enough(&acl, 0o755));
    }

    #[test]
    fn empty_acls_are_equal() {
        // WHY: acls.c:208 - both masks NO_ENTRY (xor bit clear), no group
        // check (no mask), no named entries -> equal enough.
        let a = RsyncAcl::new();
        let b = RsyncAcl::new();
        assert!(a.equal_enough(&b, 0o644));
    }

    #[test]
    fn user_obj_differs_but_extended_entries_equal_is_equal_enough() {
        // WHY: acls.c:205-206,208-223 - user_obj is NOT part of the
        // comparison; upstream leaves it to the mode. Mask absent on both,
        // named entries identical -> equal enough despite differing user_obj
        // (and other_obj). The old code compared user_obj first and wrongly
        // returned false here.
        let mut a = RsyncAcl::new();
        a.user_obj = 7;
        a.group_obj = 5;
        a.mask_obj = NO_ENTRY;
        a.other_obj = 5;
        a.names.push(IdAccess::user(1000, 0x07));

        let mut b = a.clone();
        b.user_obj = 1; // differs - ignored by upstream
        b.other_obj = 0; // differs - ignored by upstream

        assert!(a.equal_enough(&b, 0o755));
    }

    #[test]
    fn other_obj_differs_no_mask_is_equal_enough() {
        // WHY: acls.c:205-206 - other_obj is left to the mode and never
        // compared. The old code compared other_obj and wrongly rejected.
        let mut a = RsyncAcl::new();
        a.user_obj = 7;
        a.group_obj = 5;
        a.mask_obj = NO_ENTRY;
        a.other_obj = 5;

        let mut b = a.clone();
        b.other_obj = 0; // differs - ignored by upstream

        assert!(a.equal_enough(&b, 0o750));
    }

    #[test]
    fn stripped_group_obj_recovered_from_mode_matches_is_equal() {
        // WHY: acls.c:216-219 - a condensed ACL omits group_obj (NO_ENTRY)
        // only when it equalled the mask and thus the mode's group bits.
        // With mode 0o750 the group bits are 5, and racl1.group_obj is 5, so
        // the recovered comparison holds. The old code, seeing named entries,
        // compared user_obj (7 vs NO_ENTRY) and wrongly returned false.
        let mut a = RsyncAcl::new();
        a.user_obj = 7;
        a.group_obj = 5; // == (0o750 >> 3) & 7
        a.mask_obj = 7;
        a.other_obj = 0;
        a.names.push(IdAccess::user(1000, 0x07));

        let mut b = RsyncAcl::new();
        b.user_obj = NO_ENTRY;
        b.group_obj = NO_ENTRY; // stripped: recovered from mode
        b.mask_obj = 7;
        b.other_obj = NO_ENTRY;
        b.names.push(IdAccess::user(1000, 0x07));

        assert!(a.equal_enough(&b, 0o750));
    }

    #[test]
    fn stripped_group_obj_mismatching_mode_is_not_equal() {
        // WHY: acls.c:216-219 - when racl2 omits group_obj, racl1.group_obj
        // must equal the mode's group bits. Here group_obj is 4 but mode
        // 0o700 has group bits 0, so upstream returns false. The old code
        // ignored group_obj entirely when there were no named entries (it
        // compared only the mask), so it wrongly returned true - this case
        // proves the mode-recovery branch is honored.
        let mut a = RsyncAcl::new();
        a.user_obj = 7;
        a.group_obj = 4; // != (0o700 >> 3) & 7 == 0
        a.mask_obj = 6;
        a.other_obj = 0;

        let mut b = RsyncAcl::new();
        b.user_obj = 7;
        b.group_obj = NO_ENTRY; // stripped
        b.mask_obj = 6;
        b.other_obj = 0;

        assert!(!a.equal_enough(&b, 0o700));
    }

    #[test]
    fn present_group_obj_compared_directly() {
        // WHY: acls.c:220-221 - when both carry a mask and racl2 keeps its
        // group_obj, the two group_obj values are compared directly (the mode
        // is not consulted).
        let mut a = RsyncAcl::new();
        a.user_obj = 7;
        a.group_obj = 5;
        a.mask_obj = 7;
        a.other_obj = 0;

        let mut b = a.clone();
        b.group_obj = 3; // differs

        assert!(!a.equal_enough(&b, 0o777));
    }

    #[test]
    fn mask_presence_differs_is_not_equal() {
        // WHY: acls.c:208-209 - if one ACL has a mask and the other doesn't,
        // they are never equal enough. The old code, with no named entries,
        // collapsed both to their "effective group" (mask if present, else
        // group_obj); with both effective groups 6 it wrongly returned true.
        let mut a = RsyncAcl::new();
        a.user_obj = 7;
        a.group_obj = 2;
        a.mask_obj = 6; // has a mask
        a.other_obj = 5;

        let mut b = RsyncAcl::new();
        b.user_obj = 7;
        b.group_obj = 6;
        b.mask_obj = NO_ENTRY; // no mask
        b.other_obj = 5;

        assert!(!a.equal_enough(&b, 0o000));
    }

    #[test]
    fn differing_mask_values_do_not_matter_when_group_and_names_match() {
        // WHY: acls.c:208 only tests mask PRESENCE (the NO_ENTRY bit), never
        // the mask value; the actual mask is reconstructed by the
        // mode-preservation code. With both masks present, matching group_obj,
        // and equal named entries, upstream returns true even though the mask
        // values differ. The old code compared mask VALUES (5 vs 7) and
        // wrongly returned false.
        let mut a = RsyncAcl::new();
        a.user_obj = 7;
        a.group_obj = 4;
        a.mask_obj = 5;
        a.other_obj = 0;

        let mut b = a.clone();
        b.mask_obj = 7; // different mask value - upstream ignores it

        assert!(a.equal_enough(&b, 0o740));
    }

    #[test]
    fn different_named_entry_perms_not_equal() {
        // WHY: acls.c:223 (ida_entries_equal) - named entries must match in
        // access and id. No mask, so only the named entries are compared.
        // This guard also holds under the old code (both compared named
        // entries), so it protects shared behavior rather than the fix.
        let mut a = RsyncAcl::new();
        a.group_obj = 5;
        a.names.push(IdAccess::user(1000, 0x07));

        let mut b = a.clone();
        b.names.clear();
        b.names.push(IdAccess::user(1000, 0x05)); // different perms

        assert!(!a.equal_enough(&b, 0o755));
    }

    #[test]
    fn different_named_entry_count_not_equal() {
        // WHY: acls.c:223 (ida_entries_equal) - a differing entry count is
        // never equal. No mask, so only the named entries are compared.
        // Shared-behavior guard, like the case above.
        let mut a = RsyncAcl::new();
        a.group_obj = 5;
        a.names.push(IdAccess::user(1000, 0x07));

        let mut b = RsyncAcl::new();
        b.group_obj = 5;

        assert!(!a.equal_enough(&b, 0o755));
    }

    #[test]
    fn resolved_name_is_ignored_in_named_entry_comparison() {
        // WHY: acls.c:223 (ida_entries_equal) compares only access and id,
        // not the resolved name. Two entries with the same id/access but
        // different names remain equal enough. No mask here, so the group
        // branch is skipped and only the named entries are compared.
        let mut a = RsyncAcl::new();
        a.group_obj = 5;
        a.names
            .push(IdAccess::user_with_name(1000, 0x07, b"alice".to_vec()));

        let mut b = RsyncAcl::new();
        b.group_obj = 5;
        b.names.push(IdAccess::user(1000, 0x07));

        assert!(a.equal_enough(&b, 0o755));
    }
}

/// Tests for `RsyncAcl::equal`, the strict comparison used for a directory's
/// default ACL (upstream `rsync_acl_equal()`, `acls.c` lines 190-197). Unlike
/// `equal_enough`, every object and named entry participates.
mod equal_tests {
    use super::*;

    #[test]
    fn reflexive() {
        let acl = RsyncAcl::from_mode(0o755);
        assert!(acl.equal(&acl));
    }

    #[test]
    fn differing_user_obj_is_not_equal() {
        // WHY: acls.c:192 - user_obj IS part of the strict comparison (it is
        // not for equal_enough), so a difference here rejects.
        let a = RsyncAcl::from_mode(0o755);
        let mut b = a.clone();
        b.user_obj = 1;
        assert!(!a.equal(&b));
    }

    #[test]
    fn differing_other_obj_is_not_equal() {
        // WHY: acls.c:195 - other_obj participates in the strict comparison.
        let a = RsyncAcl::from_mode(0o755);
        let mut b = a.clone();
        b.other_obj = 0;
        assert!(!a.equal(&b));
    }

    #[test]
    fn identical_named_entries_are_equal() {
        let mut a = RsyncAcl::from_mode(0o644);
        a.mask_obj = 7;
        a.names.push(IdAccess::user(1000, 0x06));
        a.names.push(IdAccess::group(2000, 0x04));
        let b = a.clone();
        assert!(a.equal(&b));
    }

    #[test]
    fn differing_named_entry_id_is_not_equal() {
        // WHY: acls.c:196 - ida_entries_equal compares each (access, id) pair.
        let mut a = RsyncAcl::from_mode(0o644);
        a.names.push(IdAccess::user(1000, 0x06));
        let mut b = a.clone();
        b.names = std::iter::once(IdAccess::user(1001, 0x06)).collect();
        assert!(!a.equal(&b));
    }

    #[test]
    fn differing_named_entry_count_is_not_equal() {
        let mut a = RsyncAcl::from_mode(0o644);
        a.names.push(IdAccess::user(1000, 0x06));
        let b = RsyncAcl::from_mode(0o644);
        assert!(!a.equal(&b));
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
        let result = recv_rsync_acl(&mut reader, wire::AclType::Access).unwrap();

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
        let result = recv_rsync_acl(&mut reader, wire::AclType::Access).unwrap();

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

/// Byte-level oracle for the `--fake-super` ACL blobs, measured against the
/// real upstream rsync 3.5.0 binary rather than derived from oc's own encoder.
///
/// A store/load round trip through [`RsyncAcl::to_fake_super_bytes`] and
/// [`RsyncAcl::from_fake_super_bytes`] cannot see the defect these tests exist
/// for: oc's encoder and decoder are symmetric, so a blob that omits upstream's
/// `NO_ENTRY` slots round-trips perfectly while being byte-incompatible with a
/// tree upstream wrote or reads.
///
/// Each `UPSTREAM_*` constant below is the `user.rsync.%aacl` / `%dacl` value
/// decoded as little-endian `u32`s from a real run of
/// `rsync-3.5.0/rsync -rA -M--fake-super src/ dst/` on Linux (`-M--fake-super`
/// puts the receiving side, and only the receiving side, into fake-super mode -
/// the invocation `testsuite/fake-super-acl-xattr_test.py` leg 1 uses).
mod fake_super_store_oracle_tests {
    use super::*;

    /// Named-entry access value as it appears in a stored blob: the in-memory
    /// `NAME_IS_USER` bit is written verbatim, so a named *user* with `rwx`
    /// encodes as `0x8000_0007`.
    const NAMED_USER_RWX: u32 = NAME_IS_USER | 0x07;
    const NAMED_USER_RX: u32 = NAME_IS_USER | 0x05;
    const NOBODY: u32 = 65534;

    /// `chmod 0644 f; setfacl -m u:nobody:rwx f` - setfacl raises the mode's
    /// group bits to the new mask, leaving mode 0o674.
    const UPSTREAM_FILE_AACL: [u32; 6] = [128, 4, 7, 128, NOBODY, NAMED_USER_RWX];

    /// `chmod 0755 d; setfacl -m u:nobody:rx d` on a directory (mode 0o755).
    const UPSTREAM_DIR_AACL: [u32; 6] = [128, 128, 5, 128, NOBODY, NAMED_USER_RX];

    /// `setfacl -d -m u:nobody:rwx d` on the same directory.
    const UPSTREAM_DIR_DACL: [u32; 6] = [7, 5, 7, 5, NOBODY, NAMED_USER_RWX];

    fn le_u32s(bytes: &[u8]) -> Vec<u32> {
        bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    /// The access ACL of `f` as `getfacl` reports it: `user::rw-`,
    /// `user:nobody:rwx`, `group::r--`, `mask::rwx`, `other::r--`.
    fn file_access_acl() -> RsyncAcl {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0o6;
        acl.group_obj = 0o4;
        acl.mask_obj = 0o7;
        acl.other_obj = 0o4;
        acl.names.push(IdAccess::user(NOBODY, 0o7));
        acl
    }

    /// The access ACL of `d`: `user::rwx`, `user:nobody:r-x`, `group::r-x`,
    /// `mask::r-x`, `other::r-x`.
    fn dir_access_acl() -> RsyncAcl {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0o7;
        acl.group_obj = 0o5;
        acl.mask_obj = 0o5;
        acl.other_obj = 0o5;
        acl.names.push(IdAccess::user(NOBODY, 0o5));
        acl
    }

    /// The default ACL of `d`: `user::rwx`, `user:nobody:rwx`, `group::r-x`,
    /// `mask::rwx`, `other::r-x`.
    fn dir_default_acl() -> RsyncAcl {
        let mut acl = RsyncAcl::new();
        acl.user_obj = 0o7;
        acl.group_obj = 0o5;
        acl.mask_obj = 0o7;
        acl.other_obj = 0o5;
        acl.names.push(IdAccess::user(NOBODY, 0o7));
        acl
    }

    #[test]
    fn file_access_blob_matches_upstream_3_5_0() {
        let mut acl = file_access_acl();
        acl.condense_for_fake_super_store(0o674);
        assert_eq!(le_u32s(&acl.to_fake_super_bytes()), UPSTREAM_FILE_AACL);
    }

    #[test]
    fn dir_access_blob_matches_upstream_3_5_0() {
        let mut acl = dir_access_acl();
        acl.condense_for_fake_super_store(0o755);
        assert_eq!(le_u32s(&acl.to_fake_super_bytes()), UPSTREAM_DIR_AACL);
    }

    // Upstream's sender strips only the access ACL (`send_acl()`, acls.c:888
    // passes `sxp->acc_acl`), so the default ACL reaches the receiver fully
    // populated and is stored verbatim. Condensing it would be a divergence in
    // the opposite direction, so this pins that it is NOT condensed.
    #[test]
    fn dir_default_blob_is_stored_verbatim() {
        let acl = dir_default_acl();
        assert_eq!(le_u32s(&acl.to_fake_super_bytes()), UPSTREAM_DIR_DACL);
    }

    // Non-vacuity: without the condensing step the same two ACLs encode to
    // something upstream never writes. If this ever stops holding, the two
    // assertions above have become tautologies.
    #[test]
    fn uncondensed_access_blobs_differ_from_upstream() {
        assert_ne!(
            le_u32s(&file_access_acl().to_fake_super_bytes()),
            UPSTREAM_FILE_AACL
        );
        assert_ne!(
            le_u32s(&dir_access_acl().to_fake_super_bytes()),
            UPSTREAM_DIR_AACL
        );
    }

    // The composition is idempotent, which is what lets one storage site serve
    // both the local-copy path (a freshly read filesystem ACL) and the network
    // path (an ACL that already took upstream's strip on the wire).
    #[test]
    fn condensing_is_idempotent() {
        for (mut acl, mode) in [(file_access_acl(), 0o674), (dir_access_acl(), 0o755)] {
            acl.condense_for_fake_super_store(mode);
            let once = acl.clone();
            acl.condense_for_fake_super_store(mode);
            assert_eq!(acl, once);
        }
    }

    // An access ACL with no named entries has no mask to restore, so the whole
    // group slot is derivable and only the mask stays NO_ENTRY.
    #[test]
    fn base_only_access_acl_keeps_nothing_but_the_absent_mask() {
        let mut acl = RsyncAcl::from_mode(0o644);
        acl.condense_for_fake_super_store(0o644);
        assert_eq!(
            le_u32s(&acl.to_fake_super_bytes()),
            vec![128, 128, 128, 128]
        );
    }
}
