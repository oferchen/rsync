use super::apply::apply_acls_from_cache;
use super::default_perms::default_perms_for_dir;
use super::error::is_unsupported_error;
use super::perms::rsync_perms_to_exacl;
use super::reconstruct::{reconstruct_acl, rsync_acl_to_entries};
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use super::reset::clear_default_acl;
use super::reset::reset_acl_from_mode;
use super::sync::sync_acls;

use exacl::{AclEntryKind, Perm};
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use exacl::{AclOption, setfacl};
use protocol::acl::{AclCache, RsyncAcl};
use std::fs::File;
use tempfile::tempdir;

#[test]
fn sync_acls_skips_when_not_following_symlinks() {
    let dir = tempdir().expect("tempdir");
    let source = dir.path().join("src");
    let destination = dir.path().join("dst");
    File::create(&source).expect("create src");
    File::create(&destination).expect("create dst");

    let result = sync_acls(&source, &destination, false);
    assert!(result.is_ok());
}

#[test]
fn sync_acls_copies_between_regular_files() {
    let dir = tempdir().expect("tempdir");
    let source = dir.path().join("src");
    let destination = dir.path().join("dst");
    File::create(&source).expect("create src");
    File::create(&destination).expect("create dst");

    let result = sync_acls(&source, &destination, true);
    assert!(result.is_ok());
}

#[test]
fn sync_acls_works_with_directories() {
    let dir = tempdir().expect("tempdir");
    let source = dir.path().join("src_dir");
    let destination = dir.path().join("dst_dir");
    std::fs::create_dir(&source).expect("create src_dir");
    std::fs::create_dir(&destination).expect("create dst_dir");

    let result = sync_acls(&source, &destination, true);
    assert!(result.is_ok());
}

#[test]
fn reset_acl_from_mode_works() {
    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("test");
    File::create(&file).expect("create file");

    let result = reset_acl_from_mode(&file);
    assert!(result.is_ok());
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[test]
fn clear_default_acl_works_on_directory() {
    let dir = tempdir().expect("tempdir");
    let subdir = dir.path().join("subdir");
    std::fs::create_dir(&subdir).expect("create subdir");

    let result = clear_default_acl(&subdir);
    assert!(result.is_ok());
}

#[test]
fn is_unsupported_error_detects_common_messages() {
    let patterns = [
        "operation not supported",
        "Invalid argument",
        "No data available",
    ];

    for pattern in patterns {
        let err = std::io::Error::other(pattern);
        assert!(
            is_unsupported_error(&err),
            "should detect '{pattern}' as unsupported"
        );
    }
}

#[cfg(unix)]
#[test]
fn is_unsupported_error_detects_os_error_codes() {
    // upstream: lib/sysacls.c:2778-2799 - no_acl_syscall_error swallows
    // ENOSYS, ENOTSUP, EINVAL; ENOENT only on macOS.
    let codes = [libc::ENOTSUP, libc::ENOSYS, libc::EINVAL, libc::ENODATA];

    for code in codes {
        let err = std::io::Error::from_raw_os_error(code);
        assert!(
            is_unsupported_error(&err),
            "should detect OS error code {code} as unsupported"
        );
    }
}

#[cfg(unix)]
#[test]
fn is_unsupported_error_surfaces_eperm() {
    // upstream: acls.c:994-997 - EPERM from sys_acl_set_file surfaces via
    // rsyserr(FERROR_XFER, ...) rather than being swallowed. Without this,
    // a non-root receiver carrying an unmappable UID in a named ACL entry
    // would silently drop the entire ACL apply.
    let err = std::io::Error::from_raw_os_error(libc::EPERM);
    assert!(
        !is_unsupported_error(&err),
        "EPERM must propagate to caller as a real ACL apply failure"
    );
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn is_unsupported_error_surfaces_enoent_on_linux_freebsd() {
    // upstream: lib/sysacls.c:2780-2782 - the ENOENT swallow is macOS-only.
    let err = std::io::Error::from_raw_os_error(libc::ENOENT);
    assert!(
        !is_unsupported_error(&err),
        "ENOENT must propagate on Linux/FreeBSD (macOS-only quirk upstream)"
    );
}

#[test]
fn rsync_perms_to_exacl_all_bits() {
    assert_eq!(rsync_perms_to_exacl(0x00), Perm::empty());
    assert_eq!(rsync_perms_to_exacl(0x01), Perm::EXECUTE);
    assert_eq!(rsync_perms_to_exacl(0x02), Perm::WRITE);
    assert_eq!(rsync_perms_to_exacl(0x04), Perm::READ);
    assert_eq!(
        rsync_perms_to_exacl(0x07),
        Perm::READ | Perm::WRITE | Perm::EXECUTE
    );
    assert_eq!(rsync_perms_to_exacl(0x05), Perm::READ | Perm::EXECUTE);
}

#[test]
fn rsync_acl_to_entries_empty_acl() {
    let acl = RsyncAcl::new();
    let entries = rsync_acl_to_entries(&acl, None);
    assert!(entries.is_empty());
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[test]
fn rsync_acl_to_entries_base_entries() {
    let acl = RsyncAcl::from_mode(0o754);
    let entries = rsync_acl_to_entries(&acl, None);

    // user_obj(rwx) + group_obj(r-x) + other_obj(r--)
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].kind, AclEntryKind::User);
    assert_eq!(entries[0].name, "");
    assert_eq!(entries[0].perms, Perm::READ | Perm::WRITE | Perm::EXECUTE);
    assert_eq!(entries[1].kind, AclEntryKind::Group);
    assert_eq!(entries[1].name, "");
    assert_eq!(entries[1].perms, Perm::READ | Perm::EXECUTE);
    assert_eq!(entries[2].kind, AclEntryKind::Other);
    assert_eq!(entries[2].name, "");
    assert_eq!(entries[2].perms, Perm::READ);
}

#[test]
fn rsync_acl_to_entries_named_user_and_group() {
    use protocol::acl::IdAccess;

    let mut acl = RsyncAcl::from_mode(0o755);
    acl.names.push(IdAccess::user(1000, 0x07));
    acl.names.push(IdAccess::group(100, 0x05));

    let entries = rsync_acl_to_entries(&acl, None);

    // Find named entries (skip base entries on Linux/FreeBSD)
    let named: Vec<_> = entries.iter().filter(|e| !e.name.is_empty()).collect();
    assert_eq!(named.len(), 2);
    assert_eq!(named[0].kind, AclEntryKind::User);
    assert_eq!(named[0].name, "1000");
    assert_eq!(named[0].perms, Perm::READ | Perm::WRITE | Perm::EXECUTE);
    assert_eq!(named[1].kind, AclEntryKind::Group);
    assert_eq!(named[1].name, "100");
    assert_eq!(named[1].perms, Perm::READ | Perm::EXECUTE);
}

#[test]
fn reconstruct_access_acl_preserves_full_mask_over_narrower_named_entry() {
    // Bug #251: the receiver narrowed the ACL mask to a named entry's perms
    // instead of the transmitted (mode-derived) mask, silently downgrading
    // group access (exit 0, no error).
    //
    // Fixture (mirrors the interop repro): source mode 0o664 with
    // `user:root:r--`, `group::rw-`, `mask::rw-`. The sender strips the mask and
    // group_obj because both equal the mode's group bits (rw-), so the received
    // wire ACL carries only the named user (r--) with mask/base entries unset.
    // reconstruct_acl() must restore the mask from the mode group bits
    // ((0o664 >> 3) & 7 = 0o6 = rw-), NOT collapse it to the named user's r--.
    // upstream: acls.c:770-773 recv_rsync_acl(type=ACCESS).
    use protocol::acl::{IdAccess, NO_ENTRY};

    let mut wire = RsyncAcl::new();
    // Named user root with r-- only. Its access bits (0x04) are what the buggy
    // decode collapsed the mask onto.
    wire.names.push(IdAccess::user(0, 0x04));
    // Everything else arrives stripped (inferred from the mode).
    assert_eq!(wire.mask_obj, NO_ENTRY);

    let reconstructed = reconstruct_acl(&wire, Some(0o664));

    // The mask must equal the transmitted union mask rw- (0x06), pinned by
    // value - proving it is not narrowed to the named user's r-- (0x04).
    assert_eq!(
        reconstructed.mask_obj, 0x06,
        "mask must be restored from mode group bits (rw-), not the named entry (r--)"
    );
    assert_eq!(
        reconstructed.group_obj, 0x06,
        "group_obj rw- reconstructed from mode"
    );
    assert_eq!(reconstructed.user_obj, 0x06);
    assert_eq!(reconstructed.other_obj, 0x04);
}

#[test]
fn reconstruct_access_acl_keeps_explicit_transmitted_mask_verbatim() {
    // When the mask is not strippable (it differs from the mode group bits) the
    // sender transmits it explicitly and reconstruct_acl() must leave it
    // untouched rather than recomputing from the mode. Mask r-x (0x05), mode
    // group bits rw- (0x06): the two differ, so a recompute would corrupt it.
    use protocol::acl::IdAccess;

    let mut wire = RsyncAcl::new();
    wire.names.push(IdAccess::user(0, 0x04));
    wire.mask_obj = 0x05; // explicit r-x on the wire

    let reconstructed = reconstruct_acl(&wire, Some(0o664));
    assert_eq!(
        reconstructed.mask_obj, 0x05,
        "an explicitly transmitted mask is preserved verbatim"
    );
}

#[test]
fn rsync_acl_to_entries_remaps_named_ids_via_id_map() {
    // WHY: on a cross-host `-A` transfer the sender ships the ACL entry's own
    // namespace uid/gid (1000). Without the id-list remap the ACL would land on
    // whatever principal owns 1000 on the receiver. The AclIdMapper (built from
    // the received uid/gid id-lists) must convert 1000 -> 2000, exactly as
    // upstream match_acl_ids()/match_uid() do for file owners.
    // upstream: acls.c:1069-1072, uidlist.c:483-484.
    use crate::AclIdMapper;
    use protocol::acl::IdAccess;
    use std::collections::HashMap;

    let mut uid = HashMap::new();
    uid.insert(1000u32, 2000u32);
    let mut gid = HashMap::new();
    gid.insert(1000u32, 3000u32);

    #[cfg(unix)]
    let mapper = AclIdMapper::new(uid, gid, None, None, false);
    #[cfg(not(unix))]
    let mapper = AclIdMapper::new(uid, gid, false);

    let mut acl = RsyncAcl::from_mode(0o755);
    // No wire name: the non-inc_recurse path relies entirely on the id-list.
    acl.names.push(IdAccess::user(1000, 0x07));
    acl.names.push(IdAccess::group(1000, 0x05));

    let entries = rsync_acl_to_entries(&acl, Some(&mapper));
    let named: Vec<_> = entries.iter().filter(|e| !e.name.is_empty()).collect();
    assert_eq!(named.len(), 2);
    assert_eq!(named[0].kind, AclEntryKind::User);
    assert_eq!(
        named[0].name, "2000",
        "named user id must be remapped through the id-list (1000 -> 2000)"
    );
    assert_eq!(named[1].kind, AclEntryKind::Group);
    assert_eq!(
        named[1].name, "3000",
        "named group id must be remapped through the id-list (1000 -> 3000)"
    );
}

#[test]
fn rsync_acl_to_entries_id_map_numeric_ids_passthrough() {
    // WHY: with --numeric-ids upstream exchanges no id-list and applies ids
    // verbatim (recv_id_list guard `numeric_ids <= 0`). The mapper must be a
    // no-op even when a stale table is present.
    use crate::AclIdMapper;
    use protocol::acl::IdAccess;
    use std::collections::HashMap;

    let mut uid = HashMap::new();
    uid.insert(1000u32, 2000u32);

    #[cfg(unix)]
    let mapper = AclIdMapper::new(uid, HashMap::new(), None, None, true);
    #[cfg(not(unix))]
    let mapper = AclIdMapper::new(uid, HashMap::new(), true);

    let mut acl = RsyncAcl::from_mode(0o755);
    acl.names.push(IdAccess::user(1000, 0x07));

    let entries = rsync_acl_to_entries(&acl, Some(&mapper));
    let named: Vec<_> = entries.iter().filter(|e| !e.name.is_empty()).collect();
    assert_eq!(named.len(), 1);
    assert_eq!(
        named[0].name, "1000",
        "numeric-ids must keep the raw wire id (no remap)"
    );
}

#[cfg(unix)]
#[test]
fn rsync_acl_to_entries_preserves_unmappable_named_user() {
    // upstream: uidlist.c:282 - recv_add_id falls back to id2 = id when
    // user_to_uid(name, ...) fails, and acls.c:404 hands that raw id to
    // sys_acl_set_info(). The entry must reach setfacl, not be dropped.
    use protocol::acl::IdAccess;

    // Pick a UID that is overwhelmingly unlikely to exist locally.
    let unmappable_uid = 0x7FFF_FF42_u32;
    let unknown_name = b"oc_rsync_test_no_such_user_zzz".to_vec();

    let mut acl = RsyncAcl::from_mode(0o755);
    acl.names
        .push(IdAccess::user_with_name(unmappable_uid, 0x07, unknown_name));

    let entries = rsync_acl_to_entries(&acl, None);
    let named: Vec<_> = entries.iter().filter(|e| !e.name.is_empty()).collect();
    assert_eq!(
        named.len(),
        1,
        "named user with unresolvable wire name must not be dropped"
    );
    assert_eq!(named[0].kind, AclEntryKind::User);
    assert_eq!(
        named[0].name,
        unmappable_uid.to_string(),
        "unmappable wire UID must pass through verbatim (upstream id2 = id)"
    );
    assert_eq!(named[0].perms, Perm::READ | Perm::WRITE | Perm::EXECUTE);
}

#[cfg(unix)]
#[test]
fn rsync_acl_to_entries_remap_emits_own_debug() {
    // upstream: uidlist.c:287-291 - DEBUG_GTE(OWN, 2) emits
    // "uid %u(%s) maps to %u" / "gid %u(%s) maps to %u" for every
    // remap attempt, including the unmappable-id fallthrough.
    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};
    use protocol::acl::IdAccess;

    let mut cfg = VerbosityConfig::default();
    cfg.debug.own = 2;
    init(cfg);
    let _ = drain_events();

    let unmappable_uid = 0x7FFF_FF11_u32;
    let unmappable_gid = 0x7FFF_FF22_u32;

    let mut acl = RsyncAcl::from_mode(0o755);
    acl.names.push(IdAccess::user_with_name(
        unmappable_uid,
        0x07,
        b"oc_rsync_test_ghost_user".to_vec(),
    ));
    acl.names.push(IdAccess::group_with_name(
        unmappable_gid,
        0x05,
        b"oc_rsync_test_ghost_group".to_vec(),
    ));

    let _ = rsync_acl_to_entries(&acl, None);

    let messages: Vec<String> = drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Debug {
                flag: DebugFlag::Own,
                message,
                ..
            } => Some(message),
            _ => None,
        })
        .collect();
    let expected_uid =
        format!("uid {unmappable_uid}(oc_rsync_test_ghost_user) maps to {unmappable_uid}");
    let expected_gid =
        format!("gid {unmappable_gid}(oc_rsync_test_ghost_group) maps to {unmappable_gid}");
    assert!(
        messages.iter().any(|m| m == &expected_uid),
        "missing UID remap emission {expected_uid:?}: {messages:?}"
    );
    assert!(
        messages.iter().any(|m| m == &expected_gid),
        "missing GID remap emission {expected_gid:?}: {messages:?}"
    );
}

#[test]
fn apply_acls_from_cache_skips_symlinks() {
    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("test");
    File::create(&file).expect("create file");

    let cache = AclCache::new();
    let result = apply_acls_from_cache(&file, &cache, 0, None, false, None, None);
    assert!(result.is_ok());
}

#[test]
fn apply_acls_from_cache_applies_basic_acl() {
    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("test");
    File::create(&file).expect("create file");

    let mut cache = AclCache::new();
    let acl = RsyncAcl::from_mode(0o644);
    let ndx = cache.store_access(acl);

    let result = apply_acls_from_cache(&file, &cache, ndx, None, true, Some(0o644), None);
    assert!(result.is_ok());
}

#[test]
fn apply_acls_from_cache_empty_acl_resets() {
    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("test");
    File::create(&file).expect("create file");

    let mut cache = AclCache::new();
    let acl = RsyncAcl::new();
    let ndx = cache.store_access(acl);

    let result = apply_acls_from_cache(&file, &cache, ndx, None, true, Some(0o644), None);
    assert!(result.is_ok());
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[test]
fn apply_acls_from_cache_directory_with_default() {
    let dir = tempdir().expect("tempdir");
    let subdir = dir.path().join("subdir");
    std::fs::create_dir(&subdir).expect("create subdir");

    let mut cache = AclCache::new();
    let access = RsyncAcl::from_mode(0o755);
    let default = RsyncAcl::from_mode(0o755);
    let access_ndx = cache.store_access(access);
    let default_ndx = cache.store_default(default);

    let result = apply_acls_from_cache(
        &subdir,
        &cache,
        access_ndx,
        Some(default_ndx),
        true,
        Some(0o755),
        None,
    );
    assert!(result.is_ok());
}

#[test]
fn apply_acls_from_cache_missing_index_is_noop() {
    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("test");
    File::create(&file).expect("create file");

    let cache = AclCache::new();
    let result = apply_acls_from_cache(&file, &cache, 99, None, true, Some(0o644), None);
    assert!(result.is_ok());
}

#[test]
fn default_perms_for_dir_no_acl_returns_umask_default() {
    // No default ACL: upstream returns ACCESSPERMS & ~orig_umask without
    // emitting `DEBUG_GTE(ACL, 1)`.
    let dir = tempdir().expect("tempdir");
    let target = dir.path().join("no_default_acl");
    std::fs::create_dir(&target).expect("create dir");

    assert_eq!(default_perms_for_dir(&target, 0o022), 0o755);
    assert_eq!(default_perms_for_dir(&target, 0o077), 0o700);
    assert_eq!(default_perms_for_dir(&target, 0), 0o777);
}

#[test]
fn default_perms_for_dir_missing_dir_returns_umask_default() {
    // getfacl error path: upstream falls back to umask-derived default
    // and never emits. Verify the same here.
    let dir = tempdir().expect("tempdir");
    let missing = dir.path().join("nonexistent");

    assert_eq!(default_perms_for_dir(&missing, 0o022), 0o755);
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[test]
fn default_perms_for_dir_emits_when_default_acl_present() {
    // upstream: acls.c:1131-1134 - DEBUG_GTE(ACL, 1) fires when the
    // directory's default ACL unpacked into a user_obj entry.
    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

    let dir = tempdir().expect("tempdir");
    let target = dir.path().join("with_default_acl");
    std::fs::create_dir(&target).expect("create dir");

    // Install a default ACL: user::rwx, group::r-x, other::r-x.
    let default_entries = vec![
        exacl::AclEntry::allow_user("", Perm::READ | Perm::WRITE | Perm::EXECUTE, None),
        exacl::AclEntry::allow_group("", Perm::READ | Perm::EXECUTE, None),
        exacl::AclEntry::allow_other(Perm::READ | Perm::EXECUTE, None),
    ];
    if setfacl(&[&target], &default_entries, Some(AclOption::DEFAULT_ACL)).is_err() {
        // Filesystem doesn't support default ACLs (e.g., tmpfs without acl mount).
        // Skip the emission assertion; this is the same fallback upstream takes.
        return;
    }

    let mut cfg = VerbosityConfig::default();
    cfg.debug.acl = 1;
    init(cfg);
    let _ = drain_events();

    let perms = default_perms_for_dir(&target, 0o022);
    assert_eq!(perms, 0o755);

    let messages: Vec<String> = drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Debug {
                flag: DebugFlag::Acl,
                message,
                ..
            } => Some(message),
            _ => None,
        })
        .collect();
    let expected = format!(
        "got ACL-based default perms 755 for directory {}",
        target.display()
    );
    assert!(
        messages.iter().any(|m| m == &expected),
        "expected emission {expected:?}, got {messages:?}"
    );
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[test]
fn default_perms_for_dir_no_emission_when_disabled() {
    // upstream: DEBUG_GTE(ACL, 1) is gated; level 0 suppresses the line
    // even when the default ACL unpacks successfully.
    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

    let dir = tempdir().expect("tempdir");
    let target = dir.path().join("with_default_acl_silent");
    std::fs::create_dir(&target).expect("create dir");

    let default_entries = vec![
        exacl::AclEntry::allow_user("", Perm::READ | Perm::WRITE | Perm::EXECUTE, None),
        exacl::AclEntry::allow_group("", Perm::READ, None),
        exacl::AclEntry::allow_other(Perm::empty(), None),
    ];
    if setfacl(&[&target], &default_entries, Some(AclOption::DEFAULT_ACL)).is_err() {
        return;
    }

    let mut cfg = VerbosityConfig::default();
    cfg.debug.acl = 0;
    init(cfg);
    let _ = drain_events();

    let _ = default_perms_for_dir(&target, 0o022);
    let messages: Vec<String> = drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Debug {
                flag: DebugFlag::Acl,
                message,
                ..
            } => Some(message),
            _ => None,
        })
        .collect();
    assert!(
        messages.is_empty(),
        "ACL debug must be suppressed at level 0, got {messages:?}"
    );
}
