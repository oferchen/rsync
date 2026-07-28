//! Destination-side ACL comparison for the itemize `a` column
//! (`ITEM_REPORT_ACL`).
//!
//! Mirrors upstream's `itemize()` ACL leg (`generator.c:557-563`), which reads
//! the destination's current ACL (`get_acl`) and probes whether applying the
//! sender's cached ACL would change it (`set_acl(NULL, file, sxp, mode)`,
//! `acls.c:1013-1057`). The single destination read plus comparison lives here
//! so both the network receiver and any future local path share one mechanism,
//! matching the placement of [`crate::dest_xattrs_differ`].

use crate::AclIdMapper;
use protocol::acl::{NAME_IS_USER, NO_ENTRY, RsyncAcl};
use std::path::Path;

/// Clones `acl`, remapping every named-entry id through `id_map` so the
/// comparison sees the same local ids the apply path would write.
///
/// Mirrors upstream `match_acl_ids()` (`acls.c:1059-1081`), which converts each
/// named entry id through the uid/gid table before the cached ACL is used. Only
/// the id changes; the access bits (including [`NAME_IS_USER`]) and any carried
/// name are preserved. With no mapper the ACL is returned unchanged.
fn remap_named_ids(acl: &RsyncAcl, id_map: Option<&AclIdMapper>) -> RsyncAcl {
    let Some(mapper) = id_map else {
        return acl.clone();
    };
    let mut out = acl.clone();
    out.names = acl
        .names
        .iter()
        .map(|ida| {
            let mut mapped = ida.clone();
            mapped.id = if ida.access & NAME_IS_USER != 0 {
                mapper.map_uid(ida.id)
            } else {
                mapper.map_gid(ida.id)
            };
            mapped
        })
        .collect();
    out
}

/// Reports whether applying the sender's cached ACL(s) to `path` would change
/// the destination, i.e. whether the itemize `a` column must light.
///
/// Mirrors upstream `itemize()` -> `set_acl(NULL, file, sxp, mode)`
/// (`generator.c:561`, `acls.c:1024-1054`): the access ACL is compared with
/// [`RsyncAcl::equal_enough`] (the extended entries plus a mask-gated
/// `group_obj`, the rest left to mode preservation), and a directory's default
/// ACL - transmitted un-stripped - is compared for strict equality with
/// [`RsyncAcl::equal`].
///
/// `sender_access` / `sender_default` are the sender's cached (condensed) ACLs
/// resolved from the flist by index; either is `None` when the sender shipped no
/// ACL of that kind, matching upstream's `ndx >= 0` guard, so an absent sender
/// ACL never raises the column. The destination is read fresh via
/// [`crate::get_rsync_acl`] so the comparison sees the pre-transfer state; a
/// destination whose ACL cannot be read falls back to a mode-derived ACL exactly
/// as upstream's `get_acl` does.
///
/// The comparison order matches upstream: the destination read is the
/// fully-populated `racl1` (`self`) and the sender's cached ACL is the condensed
/// `racl2` (`other`).
#[must_use]
pub fn dest_acl_differs(
    path: &Path,
    sender_access: Option<&RsyncAcl>,
    sender_default: Option<&RsyncAcl>,
    mode: u32,
    is_dir: bool,
    id_map: Option<&AclIdMapper>,
) -> bool {
    // upstream: acls.c:1024-1037 - access ACL leg. `ndx >= 0` gates the whole
    // block, so no sender access ACL means no change to report.
    if let Some(sender) = sender_access {
        let dest = crate::get_rsync_acl(path, mode, false);
        let mut sender = remap_named_ids(sender, id_map);
        // upstream: acls.c:773-776 recv_rsync_acl() - an ACCESS ACL that carries
        // named entries but no transmitted mask is cached with
        // `mask_obj = (mode >> 3) & 7`, because a POSIX ACL with named entries
        // must have a mask. oc leaves the received access mask NO_ENTRY and
        // reconstructs it at apply time, so restore it here to feed
        // `equal_enough` the same condensed form upstream compares against;
        // otherwise the mask-presence check would light `a` on an identical ACL.
        if !sender.names.is_empty() && sender.mask_obj == NO_ENTRY {
            sender.mask_obj = ((mode >> 3) & 7) as u8;
        }
        if !dest.equal_enough(&sender, mode) {
            return true;
        }
    }

    // upstream: acls.c:1039-1054 - the default ACL leg runs for directories only
    // and uses strict `rsync_acl_equal`.
    if is_dir {
        if let Some(sender) = sender_default {
            let dest = crate::get_rsync_acl(path, mode, true);
            let sender = remap_named_ids(sender, id_map);
            if !dest.equal(&sender) {
                return true;
            }
        }
    }

    false
}

#[cfg(all(
    test,
    feature = "acl",
    any(target_os = "linux", target_os = "macos", target_os = "freebsd")
))]
mod tests {
    use super::*;
    use protocol::acl::{IdAccess, RsyncAcl};
    use std::collections::HashMap;

    #[cfg(unix)]
    fn mapper(uid: HashMap<u32, u32>, gid: HashMap<u32, u32>) -> AclIdMapper {
        AclIdMapper::new(uid, gid, None, None, false)
    }
    #[cfg(not(unix))]
    fn mapper(uid: HashMap<u32, u32>, gid: HashMap<u32, u32>) -> AclIdMapper {
        AclIdMapper::new(uid, gid, false)
    }

    /// A named-user id is rewritten through the uid table; a group id is left
    /// alone when it is not in the gid table.
    #[test]
    fn remap_rewrites_named_user_id_via_mapper() {
        let mut acl = RsyncAcl::from_mode(0o644);
        acl.names.push(IdAccess::user(1000, 0o6));
        acl.names.push(IdAccess::group(2000, 0o4));
        let mut uid = HashMap::new();
        uid.insert(1000, 5000);
        let remapped = remap_named_ids(&acl, Some(&mapper(uid, HashMap::new())));
        let ids: Vec<u32> = remapped.names.iter().map(|ida| ida.id).collect();
        assert_eq!(ids, vec![5000, 2000]);
    }

    /// Without a mapper the ACL is returned verbatim.
    #[test]
    fn remap_without_mapper_is_identity() {
        let mut acl = RsyncAcl::from_mode(0o644);
        acl.names.push(IdAccess::user(1000, 0o6));
        let remapped = remap_named_ids(&acl, None);
        assert_eq!(remapped.names.iter().next().unwrap().id, 1000);
    }
}

/// Integration tests that exercise the real destination ACL read against a
/// filesystem via `setfacl`/`getfacl`. Gated to Linux/FreeBSD, whose POSIX ACL
/// model matches upstream's `get_acl`; macOS uses a different (NFSv4) model.
/// Each test degrades gracefully when the temp filesystem lacks ACL support,
/// so they are safe to compile everywhere but only assert on a capable host -
/// the aarch64/x86_64 Linux hosts are the merge gate.
#[cfg(all(test, feature = "acl", any(target_os = "linux", target_os = "freebsd")))]
mod integration_tests {
    use super::*;
    use protocol::acl::IdAccess;
    use tempfile::tempdir;

    /// Reads a path's real access ACL and condenses it exactly as the sender
    /// would before shipping it (`strip_perms_for_send`), yielding the
    /// receiver-cached form used as `equal_enough`'s condensed `other`.
    fn sender_access_acl(path: &std::path::Path, mode: u32) -> RsyncAcl {
        let mut acl = crate::get_rsync_acl(path, mode, false);
        acl.strip_perms_for_send(mode);
        acl
    }

    /// A file whose destination ACL is byte-identical to the sender's must not
    /// light the `a` column. This is the identical-ACL false-positive guard.
    #[test]
    fn identical_named_user_acl_does_not_differ() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("f");
        std::fs::write(&file, b"x").expect("write");
        let mode = 0o644;

        let entries = vec![exacl::AclEntry::allow_user(
            "1000",
            exacl::Perm::READ | exacl::Perm::WRITE,
            None,
        )];
        if exacl::setfacl(&[&file], &entries, None).is_err() {
            return; // filesystem without ACL support
        }

        let sender = sender_access_acl(&file, mode);
        assert!(
            !dest_acl_differs(&file, Some(&sender), None, mode, false, None),
            "identical access ACL must not light the `a` column",
        );
    }

    /// A trivial file (no extended ACL) must not light `a` either: the sender's
    /// condensed ACL strips to the mode and the full destination read recovers
    /// the same via `equal_enough`.
    #[test]
    fn trivial_acl_does_not_differ() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("f");
        std::fs::write(&file, b"x").expect("write");
        let mode = 0o644;
        let sender = sender_access_acl(&file, mode);
        assert!(!dest_acl_differs(
            &file,
            Some(&sender),
            None,
            mode,
            false,
            None
        ));
    }

    /// When the sender carries a named entry the destination lacks, the access
    /// ACL differs and the `a` column lights.
    #[test]
    fn extra_named_user_on_sender_differs() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("f");
        std::fs::write(&file, b"x").expect("write");
        let mode = 0o644;

        // Destination has a named user 1000; the sender's cached ACL adds a
        // second named user 1001, so equal_enough sees unequal name counts.
        let entries = vec![exacl::AclEntry::allow_user("1000", exacl::Perm::READ, None)];
        if exacl::setfacl(&[&file], &entries, None).is_err() {
            return;
        }

        let mut sender = sender_access_acl(&file, mode);
        sender.names.push(IdAccess::user(1001, 0x04));
        assert!(
            dest_acl_differs(&file, Some(&sender), None, mode, false, None),
            "an extra sender named entry must light the `a` column",
        );
    }

    /// The default-ACL leg is directory-only and uses strict equality. A
    /// directory carrying no default ACL reads back as an empty ACL, so a sender
    /// default that also reads empty must not light `a`, while a sender default
    /// with an extra entry must. Exercises both arms of the default leg without
    /// depending on setting a (necessarily complete) POSIX default ACL on disk.
    #[test]
    fn default_acl_on_dir() {
        let dir = tempdir().expect("tempdir");
        let sub = dir.path().join("d");
        std::fs::create_dir(&sub).expect("mkdir");
        let mode = 0o755;

        // No default ACL on disk -> dest default reads back empty.
        let dest_default = crate::get_rsync_acl(&sub, mode, true);
        assert!(
            !dest_acl_differs(&sub, None, Some(&dest_default), mode, true, None),
            "an identical (empty) default ACL must not light `a`",
        );

        let mut changed = dest_default.clone();
        changed.names.push(IdAccess::group(2000, 0x04));
        assert!(
            dest_acl_differs(&sub, None, Some(&changed), mode, true, None),
            "a differing default ACL must light `a`",
        );

        // The default leg must never fire for a non-directory even when a
        // default ACL is supplied (upstream gates on S_ISDIR).
        let file = dir.path().join("f");
        std::fs::write(&file, b"x").expect("write");
        assert!(
            !dest_acl_differs(&file, None, Some(&changed), 0o644, false, None),
            "default ACL leg must not run for a non-directory",
        );
    }
}
