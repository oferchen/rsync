//! Read filesystem ACLs and translate them into the rsync wire representation.

use std::path::Path;

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use exacl::AclOption;
use exacl::{AclEntryKind, getfacl};
use protocol::acl::{IdAccess, NO_ENTRY, RsyncAcl};

use super::error::is_unsupported_error;
use super::perms::exacl_perms_to_rsync;

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
                // upstream: acls.c:497-501 - named user ACE
                let uid: u32 = entry.name.parse().unwrap_or(0);
                acl.names.push(IdAccess::user(uid, u32::from(perms)));
            }
            AclEntryKind::Group => {
                // upstream: acls.c:502-506 - named group ACE
                let gid: u32 = entry.name.parse().unwrap_or(0);
                acl.names.push(IdAccess::group(gid, u32::from(perms)));
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
