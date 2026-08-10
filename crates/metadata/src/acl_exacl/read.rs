//! Read filesystem ACLs and translate them into the rsync wire representation.

use std::path::Path;

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use exacl::AclOption;
use exacl::{AclEntryKind, getfacl};
use protocol::acl::{IdAccess, NO_ENTRY, RsyncAcl};

use super::error::is_unsupported_error;
use super::perms::exacl_perms_to_rsync;
use crate::id_lookup::{lookup_group_by_name, lookup_user_by_name};

/// Resolves an `exacl` named-entry string into a numeric id plus, when the OS
/// supplied a real (non-numeric) name, the name bytes to ship on the wire.
///
/// `exacl` reports a named user/group entry either as a bare numeric id string
/// (when the uid/gid has no passwd/group entry) or as a resolved name. A
/// numeric string is used directly with no wire name. A real name is resolved
/// to its local id via NSS (`getpwnam_r`/`getgrnam_r`) and preserved so the
/// receiver can remap by name - mirroring upstream `add_uid`/`add_gid` at
/// `acls.c:591-602`, which sends the name unless `--numeric-ids`.
///
/// The previous `name.parse().unwrap_or(0)` silently coerced every real name to
/// id 0 (root) and dropped the name, so the receiver's name-based remap was
/// starved and named ACL entries collapsed onto root.
fn resolve_named_entry(name: &str, is_user: bool) -> (u32, Option<Vec<u8>>) {
    if let Ok(id) = name.parse::<u32>() {
        return (id, None);
    }
    let resolved = if is_user {
        lookup_user_by_name(name.as_bytes()).ok().flatten()
    } else {
        lookup_group_by_name(name.as_bytes()).ok().flatten()
    };
    (resolved.unwrap_or(0), Some(name.as_bytes().to_vec()))
}

/// Reads the filesystem ACL for `path` and converts it to an [`RsyncAcl`].
///
/// Returns a populated `RsyncAcl` with base entries (user_obj, group_obj,
/// mask_obj, other_obj) and any named user/group entries. When the filesystem
/// does not support ACLs or the path does not exist, returns a fake ACL
/// derived from `mode` (matching upstream's fallback behavior).
///
/// # Arguments
///
/// * `path` - File or directory to read ACLs from
/// * `mode` - File mode bits used as fallback when no ACL is available
/// * `is_default` - If true, reads the default ACL (for directories only)
///
/// # Upstream Reference
///
/// Mirrors `get_rsync_acl()` in `acls.c` lines 472-536.
pub fn get_rsync_acl(path: &Path, mode: u32, is_default: bool) -> RsyncAcl {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    let option = if is_default {
        Some(AclOption::DEFAULT_ACL)
    } else {
        Some(AclOption::ACCESS_ACL)
    };
    #[cfg(target_os = "macos")]
    let option = {
        let _ = is_default;
        None
    };

    let entries = match getfacl(path, option) {
        Ok(e) => e,
        Err(e) => {
            if is_unsupported_error(&e) {
                // upstream: acls.c:525-528 - fake ACL from mode when unsupported
                return if !is_default {
                    RsyncAcl::from_mode(mode)
                } else {
                    RsyncAcl::new()
                };
            }
            // upstream: acls.c:525-528 - mode-based fallback on error
            return if !is_default {
                RsyncAcl::from_mode(mode)
            } else {
                RsyncAcl::new()
            };
        }
    };

    if entries.is_empty() {
        return if !is_default {
            RsyncAcl::from_mode(mode)
        } else {
            RsyncAcl::new()
        };
    }

    let mut acl = RsyncAcl::new();

    for entry in &entries {
        let perms = exacl_perms_to_rsync(entry.perms);
        match entry.kind {
            AclEntryKind::User if entry.name.is_empty() => {
                // upstream: acls.c:481 - user_obj from unnamed User entry
                if acl.user_obj == NO_ENTRY {
                    acl.user_obj = perms;
                }
            }
            AclEntryKind::Group if entry.name.is_empty() => {
                // upstream: acls.c:484 - group_obj from unnamed Group entry
                if acl.group_obj == NO_ENTRY {
                    acl.group_obj = perms;
                }
            }
            // upstream: acls.c:487-490 - mask and other POSIX entries
            #[cfg(any(target_os = "linux", target_os = "freebsd"))]
            AclEntryKind::Mask => {
                if acl.mask_obj == NO_ENTRY {
                    acl.mask_obj = perms;
                }
            }
            #[cfg(any(target_os = "linux", target_os = "freebsd"))]
            AclEntryKind::Other => {
                if acl.other_obj == NO_ENTRY {
                    acl.other_obj = perms;
                }
            }
            AclEntryKind::User => {
                // upstream: acls.c:497-501 - named user ACE. Resolve the name to
                // a numeric id and keep it for the wire so the receiver remaps
                // by name (upstream add_uid, acls.c:593).
                let (uid, name) = resolve_named_entry(&entry.name, true);
                acl.names.push(match name {
                    Some(name) => IdAccess::user_with_name(uid, u32::from(perms), name),
                    None => IdAccess::user(uid, u32::from(perms)),
                });
            }
            AclEntryKind::Group => {
                // upstream: acls.c:502-506 - named group ACE. As above via
                // upstream add_gid (acls.c:595).
                let (gid, name) = resolve_named_entry(&entry.name, false);
                acl.names.push(match name {
                    Some(name) => IdAccess::group_with_name(gid, u32::from(perms), name),
                    None => IdAccess::group(gid, u32::from(perms)),
                });
            }
            _ => {}
        }
    }

    // upstream: acls.c:493-501 - fill NO_ENTRY base entries from mode
    if acl.user_obj == NO_ENTRY {
        acl.user_obj = ((mode >> 6) & 7) as u8;
    }
    if acl.group_obj == NO_ENTRY {
        acl.group_obj = ((mode >> 3) & 7) as u8;
    }
    if acl.other_obj == NO_ENTRY {
        acl.other_obj = (mode & 7) as u8;
    }

    acl
}

#[cfg(test)]
mod tests {
    use super::resolve_named_entry;
    use crate::id_lookup::lookup_user_by_name;

    #[test]
    fn numeric_name_passes_through_without_wire_name() {
        // A bare numeric id (no passwd/group entry) is used as-is and carries
        // no wire name.
        assert_eq!(resolve_named_entry("4242", true), (4242, None));
        assert_eq!(resolve_named_entry("0", false), (0, None));
    }

    #[test]
    fn real_username_resolves_via_nss_and_keeps_name() {
        // "daemon" is uid 1 on both Linux and macOS. The old
        // `parse().unwrap_or(0)` collapsed it to root and dropped the name;
        // the fix must resolve the real uid via NSS and preserve the name so
        // the receiver can remap by name.
        let Some(daemon_uid) = lookup_user_by_name(b"daemon").ok().flatten() else {
            return; // environment without a "daemon" account: skip
        };
        let (id, name) = resolve_named_entry("daemon", true);
        assert_eq!(
            id, daemon_uid,
            "named entry must resolve via NSS, not coerce to 0"
        );
        assert_ne!(id, 0, "regression: a real username silently became root");
        assert_eq!(name.as_deref(), Some(&b"daemon"[..]));
    }

    #[test]
    fn unknown_name_still_ships_so_receiver_can_remap() {
        // An unmappable name must still travel on the wire (id falls back to 0)
        // so the receiver's name-based remap gets a chance, rather than being
        // silently replaced by a nameless root entry.
        let (id, name) = resolve_named_entry("oc_rsync_nonexistent_principal_zzz", true);
        assert_eq!(id, 0);
        assert_eq!(
            name.as_deref(),
            Some(&b"oc_rsync_nonexistent_principal_zzz"[..])
        );
    }
}
