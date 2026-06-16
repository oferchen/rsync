//! Compute the default permission bits a child of a directory should inherit.

use std::path::Path;

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use exacl::{AclEntryKind, AclOption, getfacl};
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use protocol::acl::trace_default_perms_for_dir;

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use super::perms::exacl_perms_to_rsync;

/// Computes the default permission bits a child of `dir` should inherit.
///
/// Mirrors upstream rsync's `default_perms_for_dir` (`acls.c:1083-1139`):
///
/// 1. Start from `ACCESSPERMS & ~umask` (the POSIX default).
/// 2. Read the directory's default POSIX ACL.
/// 3. When the ACL has a `user_obj` entry, fold its bits into the returned
///    permission mask via `rsync_acl_get_perms`.
/// 4. When `--debug=ACL` is at level 1, emit `got ACL-based default perms %o
///    for directory %s` (`acls.c:1133-1134`).
///
/// Unsupported filesystems and missing default ACLs both fall back to the
/// umask-derived default without emitting; only the successful ACL lookup
/// produces the upstream debug line.
///
/// On platforms that do not expose POSIX default ACLs (e.g. macOS), this
/// function returns the umask-derived default and never emits, matching the
/// `#ifdef SUPPORT_ACLS` gating at `generator.c:1337-1340`.
///
/// # Upstream Reference
///
/// - `acls.c:1083-1139` `default_perms_for_dir`
/// - `generator.c:1337-1340` and `receiver.c:846-851` - the two call sites
///   that fold the returned bits into `dest_mode()` when `--perms` is off.
#[allow(clippy::module_name_repetitions)]
#[must_use]
pub fn default_perms_for_dir(dir: &Path, orig_umask: u32) -> u32 {
    // upstream: acls.c:1093 - perms = ACCESSPERMS & ~orig_umask
    let default_perms = 0o777u32 & !(orig_umask & 0o777);

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        let dir_label = dir.to_string_lossy();
        let entries = match getfacl(dir, Some(AclOption::DEFAULT_ACL)) {
            Ok(e) => e,
            Err(_) => return default_perms,
        };

        if entries.is_empty() {
            return default_perms;
        }

        // upstream: acls.c:1131 - racl.user_obj != NO_ENTRY guard
        let mut user_obj_perms: Option<u8> = None;
        let mut group_obj_perms: Option<u8> = None;
        let mut other_obj_perms: Option<u8> = None;
        let mut mask_obj_perms: Option<u8> = None;
        for entry in &entries {
            let perms = exacl_perms_to_rsync(entry.perms);
            match entry.kind {
                AclEntryKind::User if entry.name.is_empty() && user_obj_perms.is_none() => {
                    user_obj_perms = Some(perms);
                }
                AclEntryKind::Group if entry.name.is_empty() && group_obj_perms.is_none() => {
                    group_obj_perms = Some(perms);
                }
                AclEntryKind::Mask if mask_obj_perms.is_none() => {
                    mask_obj_perms = Some(perms);
                }
                AclEntryKind::Other if other_obj_perms.is_none() => {
                    other_obj_perms = Some(perms);
                }
                _ => {}
            }
        }

        let Some(user_obj) = user_obj_perms else {
            return default_perms;
        };
        // upstream: acls.c:129-134 rsync_acl_get_perms:
        //   = (user_obj << 6)
        //     + ((mask_obj != NO_ENTRY ? mask_obj : group_obj) << 3)
        //     + other_obj
        // When the default ACL carries a `mask` entry, the mask supersedes
        // the group_obj for the middle three bits because the mask is the
        // effective upper bound for named users, named groups, and the
        // group_obj in POSIX ACL semantics.
        let group_bits = mask_obj_perms.unwrap_or_else(|| group_obj_perms.unwrap_or(0));
        let perms = (u32::from(user_obj) << 6)
            | (u32::from(group_bits) << 3)
            | u32::from(other_obj_perms.unwrap_or(0));
        // upstream: acls.c:1133-1134 - DEBUG_GTE(ACL, 1) emission
        trace_default_perms_for_dir(perms, &dir_label);
        perms
    }

    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    {
        // macOS and other targets without POSIX default ACLs: no emission,
        // matches upstream's `#ifdef SUPPORT_ACLS` guard at generator.c:1337.
        let _ = dir;
        default_perms
    }
}
