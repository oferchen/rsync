//! Reconstruction of system ACL entries from the rsync wire representation.
//!
//! The sender strips base permission entries (user_obj, group_obj, other_obj)
//! that can be inferred from the file mode. The receiver must rebuild them
//! before applying the ACL to the destination filesystem.

use exacl::AclEntry;
use protocol::acl::{NAME_IS_USER, NO_ENTRY, RsyncAcl};

use super::perms::rsync_perms_to_exacl;

/// Reconstructs a full ACL from stripped wire data and the file mode.
///
/// The sender strips base permission entries (user_obj, group_obj, other_obj)
/// that can be inferred from the file mode. The receiver must reconstruct
/// them before applying the ACL to the destination filesystem.
///
/// # Upstream Reference
///
/// Mirrors `change_sacl_perms()` in `acls.c` lines 857-933 which fills
/// base entries from the file mode when the ACL was received stripped.
pub(super) fn reconstruct_acl(acl: &RsyncAcl, mode: Option<u32>) -> RsyncAcl {
    let mode = match mode {
        Some(m) => m,
        None => return acl.clone(),
    };

    let mut result = acl.clone();

    // upstream: acls.c:892 - user_obj from mode bits 8-6
    if result.user_obj == NO_ENTRY {
        result.user_obj = ((mode >> 6) & 7) as u8;
    }
    // upstream: acls.c:898 - group_obj from mode bits 5-3
    if result.group_obj == NO_ENTRY {
        result.group_obj = ((mode >> 3) & 7) as u8;
    }
    // upstream: acls.c:911 - other_obj from mode bits 2-0
    if result.other_obj == NO_ENTRY {
        result.other_obj = (mode & 7) as u8;
    }
    // upstream: acls.c:900-908 - mask from mode bits 5-3 when needed
    if !result.names.is_empty() && result.mask_obj == NO_ENTRY {
        result.mask_obj = ((mode >> 3) & 7) as u8;
    }

    result
}

/// Converts a [`RsyncAcl`] from the wire protocol into a list of [`AclEntry`]
/// values suitable for [`exacl::setfacl`].
///
/// On Linux/FreeBSD, the resulting list contains POSIX ACL entries (user_obj,
/// group_obj, mask, other, plus named user/group entries). On macOS, only
/// named user/group entries are emitted as extended ACL entries since the
/// base permissions are managed separately through file mode bits.
///
/// # Upstream Reference
///
/// Mirrors `set_rsync_acl()` in `acls.c` lines 835-928 which reconstructs
/// a system ACL from the wire protocol `rsync_acl` struct.
pub(super) fn rsync_acl_to_entries(acl: &RsyncAcl) -> Vec<AclEntry> {
    let mut entries = Vec::new();

    // upstream: acls.c:866-878 - base entries for POSIX ACLs only (Linux/FreeBSD).
    // macOS manages base mode bits separately from extended ACL entries.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        if acl.has_user_obj() {
            entries.push(AclEntry::allow_user(
                "",
                rsync_perms_to_exacl(acl.user_obj),
                None,
            ));
        }
        if acl.has_group_obj() {
            entries.push(AclEntry::allow_group(
                "",
                rsync_perms_to_exacl(acl.group_obj),
                None,
            ));
        }
        if acl.has_mask_obj() {
            entries.push(AclEntry::allow_mask(
                rsync_perms_to_exacl(acl.mask_obj),
                None,
            ));
        }
        if acl.has_other_obj() {
            entries.push(AclEntry::allow_other(
                rsync_perms_to_exacl(acl.other_obj),
                None,
            ));
        }
    }

    for ida in acl.names.iter() {
        let perms = rsync_perms_to_exacl(ida.permissions() as u8);
        let name = ida.id.to_string();

        if ida.access & NAME_IS_USER != 0 {
            entries.push(AclEntry::allow_user(&name, perms, None));
        } else {
            entries.push(AclEntry::allow_group(&name, perms, None));
        }
    }

    entries
}
