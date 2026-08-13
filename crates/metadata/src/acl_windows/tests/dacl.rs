//! Tests for the DACL read/apply pipeline, including
//! [`reconstruct_acl`] fill-in semantics and the `apply_acls_from_cache`
//! short-circuit branches.

use std::fs::File;
use tempfile::tempdir;

use protocol::acl::{AclCache, IdAccess, IdaEntries, NO_ENTRY, RsyncAcl};

use crate::acl_windows::dacl::{apply_acls_from_cache, get_rsync_acl, reconstruct_acl};

/// Diagnostic helper exposed for unit tests: returns whether a given
/// [`IdaEntries`] has any name annotation. Keeps the test surface stable
/// even if internal helpers are refactored.
fn entries_have_names(entries: &IdaEntries) -> bool {
    entries.iter().any(|e| e.name.is_some())
}

#[test]
fn reconstruct_acl_fills_base_entries_from_mode() {
    let stripped = RsyncAcl::default();
    let restored = reconstruct_acl(&stripped, Some(0o751));
    assert_eq!(restored.user_obj, 0o7);
    assert_eq!(restored.group_obj, 0o5);
    assert_eq!(restored.other_obj, 0o1);
}

#[test]
fn reconstruct_acl_keeps_existing_entries() {
    let acl = RsyncAcl {
        user_obj: 0o4,
        ..Default::default()
    };
    let restored = reconstruct_acl(&acl, Some(0o777));
    assert_eq!(restored.user_obj, 0o4);
    assert_eq!(restored.group_obj, 0o7);
    assert_eq!(restored.other_obj, 0o7);
}

#[test]
fn reconstruct_acl_no_mode_passes_through() {
    let acl = RsyncAcl {
        user_obj: 0o7,
        ..Default::default()
    };
    let restored = reconstruct_acl(&acl, None);
    assert_eq!(restored.user_obj, 0o7);
    assert_eq!(restored.group_obj, NO_ENTRY);
}

#[test]
fn apply_acls_from_cache_skips_when_not_following() {
    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("test");
    File::create(&file).expect("file");
    let cache = AclCache::new();
    let result = apply_acls_from_cache(&file, &cache, 0, None, false, None, None);
    assert!(result.is_ok());
}

#[test]
fn apply_acls_from_cache_missing_index_is_noop() {
    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("test");
    File::create(&file).expect("file");
    let cache = AclCache::new();
    let result = apply_acls_from_cache(&file, &cache, 99, None, true, Some(0o644), None);
    assert!(result.is_ok());
}

#[test]
fn apply_acls_from_cache_empty_cache_no_op() {
    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("test");
    File::create(&file).expect("file");
    let mut cache = AclCache::new();
    let acl = RsyncAcl::from_mode(0o644);
    let ndx = cache.store_access(acl);
    let result = apply_acls_from_cache(&file, &cache, ndx, None, true, Some(0o644), None);
    assert!(result.is_ok());
}

#[test]
fn get_rsync_acl_default_returns_empty() {
    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("test");
    File::create(&file).expect("file");
    let acl = get_rsync_acl(&file, 0o644, true);
    assert!(acl.is_empty());
}

#[test]
fn entries_have_names_helper() {
    let mut entries = IdaEntries::new();
    assert!(!entries_have_names(&entries));
    entries.push(IdAccess::user_with_name(1000, 0o5, b"alice".to_vec()));
    assert!(entries_have_names(&entries));
}

#[cfg(windows)]
#[test]
fn resolve_acl_aces_records_unmappable_sids() {
    use crate::acl_windows::dacl::resolve_acl_aces;

    // Two drop reasons, both reachable on any Windows host without a live
    // domain: a named principal that resolves to no local account (standing in
    // for a foreign-domain SID on a cross-domain transfer, where
    // LookupAccountNameW fails), and an entry that arrived without a name.
    let bogus = "oc-rsync-nonexistent-principal-\u{2713}-42"
        .as_bytes()
        .to_vec();
    let mut acl = RsyncAcl::default();
    acl.names.push(IdAccess::user(7000, 0o4));
    acl.names.push(IdAccess::group_with_name(4242, 0o5, bogus));

    let (sids, masks, dropped) = resolve_acl_aces(&acl);

    // Nothing mappable survived, so no DACL would be written.
    assert!(sids.is_empty(), "unmappable entries must not yield SIDs");
    assert!(masks.is_empty());

    // The dropped entries must be surfaced as a structured audit record rather
    // than silently swallowed, and must identify each lost principal.
    assert!(
        !dropped.is_empty(),
        "dropped SIDs must be recorded, not silently discarded"
    );
    assert_eq!(dropped.descriptions.len(), 2, "both entries recorded");
    assert!(
        dropped.descriptions.iter().any(|d| d.contains("uid 7000")),
        "unnamed entry must be identified: {:?}",
        dropped.descriptions
    );
    assert!(
        dropped.descriptions.iter().any(|d| d.contains("gid 4242")),
        "unmappable named entry must be identified: {:?}",
        dropped.descriptions
    );
}

#[cfg(windows)]
#[test]
fn resolve_acl_aces_no_drops_for_empty_acl() {
    use crate::acl_windows::dacl::resolve_acl_aces;

    let acl = RsyncAcl::default();
    let (sids, masks, dropped) = resolve_acl_aces(&acl);
    assert!(sids.is_empty());
    assert!(masks.is_empty());
    assert!(dropped.is_empty(), "no entries means no dropped record");
}

/// A deny ACE has no POSIX-style representation, so the rsync wire format
/// cannot carry it. Dropping it is a documented scope choice; dropping it
/// SILENTLY is the defect - an operator syncing an NTFS tree has no way to
/// learn that a denial did not travel.
///
/// Symmetric with `resolve_acl_aces_records_unmappable_sids`, which pins the
/// same contract for the opposite direction.
#[cfg(windows)]
#[test]
#[allow(unsafe_code)]
fn dacl_to_rsync_acl_audits_deny_aces_instead_of_dropping_them_silently() {
    use windows::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAce, AddAccessDeniedAce,
        CreateWellKnownSid, InitializeAcl, PSID, WinWorldSid,
    };

    use crate::acl_windows::dacl::dacl_to_rsync_acl;

    /// `FILE_GENERIC_READ` - maps to the rsync `r` bit, so the allow ACE
    /// survives `access_mask_to_rsync_perms` and the cell distinguishes
    /// "dropped because deny" from "dropped because unmappable mask".
    const FILE_GENERIC_READ: u32 = 0x0012_0089;

    // `WinWorldSid` (Everyone, S-1-1-0) exists on every Windows host and is
    // locale-independent, so this cell needs no domain and no live account.
    let mut sid_buf = vec![0u8; 256];
    let mut sid_len = sid_buf.len() as u32;
    // SAFETY: the buffer is far larger than SECURITY_MAX_SID_SIZE and
    // `sid_len` carries its capacity in and the written length out.
    unsafe {
        CreateWellKnownSid(
            WinWorldSid,
            None,
            Some(PSID(sid_buf.as_mut_ptr().cast())),
            &mut sid_len,
        )
    }
    .expect("materialise the Everyone SID");
    let psid = PSID(sid_buf.as_mut_ptr().cast());

    // Same sizing rule as `apply_rsync_acl_to_path`: ACL header plus, per ACE,
    // the ACE struct and SID minus the 4-byte inline `SidStart` placeholder.
    let ace_size = std::mem::size_of::<ACCESS_ALLOWED_ACE>() as u32 + sid_len
        - std::mem::size_of::<u32>() as u32;
    let dacl_size = std::mem::size_of::<ACL>() as u32 + 2 * ace_size;
    let mut dacl_buf = vec![0u8; dacl_size as usize];
    let pacl = dacl_buf.as_mut_ptr().cast::<ACL>();

    // SAFETY: `dacl_buf` is sized for the header plus both ACEs, and every
    // call below writes into that same initialised ACL.
    unsafe {
        InitializeAcl(pacl, dacl_size, ACL_REVISION).expect("initialize ACL");
        AddAccessAllowedAce(pacl, ACL_REVISION, FILE_GENERIC_READ, psid)
            .expect("add the allow ACE");
        AddAccessDeniedAce(pacl, ACL_REVISION, FILE_GENERIC_READ, psid).expect("add the deny ACE");
    }

    let (acl, dropped) = dacl_to_rsync_acl(pacl);

    assert_eq!(
        acl.names.len(),
        1,
        "the expressible allow ACE must still convert; auditing the deny ACE \
         must not cost the entry that does travel"
    );
    assert_eq!(
        dropped.descriptions.len(),
        1,
        "exactly the deny ACE is dropped, and it must be recorded: {:?}",
        dropped.descriptions
    );
    assert!(
        dropped.descriptions[0].contains("deny"),
        "the audit record must name the ACE type that was lost: {:?}",
        dropped.descriptions
    );
}

#[cfg(windows)]
#[test]
fn read_dacl_on_temp_file_returns_dacl() {
    use crate::acl_windows::dacl::read_dacl;

    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("test");
    File::create(&file).expect("file");
    let result = read_dacl(&file);
    assert!(result.is_ok(), "read_dacl failed: {:?}", result.err());
    let (sd, pdacl) = result.unwrap();
    // NTFS volumes always return a DACL; ReFS/FAT may return null.
    assert!(!pdacl.is_null() || sd.pd.0.is_null());
}
