//! Reconstruction of system ACL entries from the rsync wire representation.
//!
//! The sender strips base permission entries (user_obj, group_obj, other_obj)
//! that can be inferred from the file mode. The receiver must rebuild them
//! before applying the ACL to the destination filesystem.

use exacl::AclEntry;
use protocol::acl::{
    IdAccess, NAME_IS_USER, NO_ENTRY, RsyncAcl, trace_acl_gid_remap, trace_acl_uid_remap,
};

use super::perms::rsync_perms_to_exacl;
use crate::AclIdMapper;
use crate::id_lookup::{
    lookup_group_by_name, lookup_group_name, lookup_user_by_name, lookup_user_name,
};

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
/// # Unmappable IDs
///
/// Named user/group entries always pass through to the destination ACL even
/// when the receiver cannot resolve the UID/GID locally. The function never
/// drops a named entry: an unresolved wire ID is forwarded to `setfacl`
/// verbatim, mirroring upstream's `recv_add_id()` fallback at
/// `uidlist.c:282` (`id2 = id`) and `match_uid()`/`match_gid()` semantics at
/// `uidlist.c:297-337`. The receiver-side ID resolution happens here so the
/// upstream `--debug=own2` line (`"uid %u(%s) maps to %u"`,
/// `uidlist.c:287-291`) reaches operators investigating non-root ACL
/// restores, which previously dropped silently.
///
/// # Upstream Reference
///
/// - `acls.c:395-405` `pack_smb_acl` - passes `ida->id` straight to
///   `sys_acl_set_info()` without dropping unresolved IDs.
/// - `acls.c:705-721` `recv_ida_entries` - calls `match_uid`/`match_gid` on
///   incoming named entries.
/// - `uidlist.c:243-294` `recv_add_id` - falls back to the raw wire id when
///   the name does not resolve; emits the `DEBUG_GTE(OWN, 2)` mapping line.
///
/// # Cross-host remapping
///
/// When `id_map` is present, named entry ids are remapped through the received
/// id-list plus `--usermap`/`--groupmap`, mirroring upstream `match_acl_ids()`
/// (`acls.c:1059-1081`). Without a mapper the receiver falls back to the
/// name-based NSS resolution described above.
pub(super) fn rsync_acl_to_entries(acl: &RsyncAcl, id_map: Option<&AclIdMapper>) -> Vec<AclEntry> {
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
        let mapped_id = resolve_ida_id(ida, id_map);
        let name = mapped_id.to_string();

        if ida.access & NAME_IS_USER != 0 {
            entries.push(AclEntry::allow_user(&name, perms, None));
        } else {
            entries.push(AclEntry::allow_group(&name, perms, None));
        }
    }

    entries
}

/// Resolves the local UID/GID for a wire ACL entry, mirroring upstream's
/// `match_uid`/`match_gid` + `recv_add_id` fallback chain.
///
/// 1. When the sender shipped a name, try local NSS (`getpwnam_r`/`getgrnam_r`)
///    and use the resolved id when found - matches `recv_add_id()`
///    `user_to_uid(name, ...)`/`group_to_gid(name, ...)` at
///    `uidlist.c:273-280`.
/// 2. When no name was shipped, probe the wire id against local NSS
///    (`getpwuid_r`/`getgrgid_r`) so the upstream `DEBUG_GTE(OWN, 2)`
///    mapping line fires for both resolved and unresolved entries (the
///    upstream debug emission runs unconditionally inside `recv_add_id` at
///    `uidlist.c:287-291`).
/// 3. Always return the chosen id (resolved or unchanged wire id). Upstream
///    falls back to `id2 = id` at `uidlist.c:282` and the receiver still
///    calls `sys_acl_set_info()` with whatever id is in `ida->id`
///    (`acls.c:404`), so unmappable IDs flow through to the kernel.
///
/// When `id_map` is present it takes precedence: the id is remapped through the
/// received id-list plus `--usermap`/`--groupmap`, mirroring upstream
/// `match_acl_ids()` (`acls.c:1059-1081`). The id-list already folds in the
/// sender-supplied name resolution, so this is the cross-host path.
fn resolve_ida_id(ida: &IdAccess, id_map: Option<&AclIdMapper>) -> u32 {
    let is_user = ida.access & NAME_IS_USER != 0;
    let wire = ida.id;

    // upstream: acls.c:1069-1072 match_racl_ids - convert every named entry id
    // through the same uid/gid table as file owners (match_uid/match_gid).
    if let Some(mapper) = id_map {
        let mapped = if is_user {
            mapper.map_uid(wire)
        } else {
            mapper.map_gid(wire)
        };
        let name_str = ida
            .name
            .as_deref()
            .map(String::from_utf8_lossy)
            .unwrap_or_default();
        if is_user {
            trace_acl_uid_remap(wire, &name_str, mapped);
        } else {
            trace_acl_gid_remap(wire, &name_str, mapped);
        }
        return mapped;
    }

    if let Some(name_bytes) = ida.name.as_deref() {
        let name_str = String::from_utf8_lossy(name_bytes);
        let resolved = if is_user {
            lookup_user_by_name(name_bytes).ok().flatten()
        } else {
            lookup_group_by_name(name_bytes).ok().flatten()
        };
        let mapped = resolved.unwrap_or(wire);
        if is_user {
            trace_acl_uid_remap(wire, &name_str, mapped);
        } else {
            trace_acl_gid_remap(wire, &name_str, mapped);
        }
        return mapped;
    }

    // No wire name: upstream's recv_add_id sets id2 = id but still emits the
    // debug line. Probe local NSS so the emission can report a name when one
    // exists.
    let resolved_name = if is_user {
        lookup_user_name(wire).ok().flatten()
    } else {
        lookup_group_name(wire).ok().flatten()
    };
    let name_str = resolved_name
        .as_deref()
        .map(String::from_utf8_lossy)
        .unwrap_or_default();
    if is_user {
        trace_acl_uid_remap(wire, &name_str, wire);
    } else {
        trace_acl_gid_remap(wire, &name_str, wire);
    }
    wire
}
