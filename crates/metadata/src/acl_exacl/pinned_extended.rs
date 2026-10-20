//! Race-safe receiver ACL apply through the pinned leaf fd (macOS).
//!
//! A deliberate oc improvement over upstream. Upstream's macOS build has no
//! race-safe ACL primitive (`SUPPORT_ACL_FD` is Linux-only,
//! `configure.ac:1512-1514`), so `set_rsync_acl()` sets the extended ACL by
//! path and re-walks every parent (`acls.c:1315-1322`). A parent flipped to a
//! symlink between the confined pin and that call redirects the ACL outside
//! the destination tree. Here the ACL goes through `acl_set_fd_np` on the
//! `O_NOFOLLOW` leaf fd the xattr writes already hold ([`crate::dest_pin`]),
//! so the parents are never walked again.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.1/acls.c:399-409` `pack_smb_acl()` - under `HAVE_OSX_ACLS`
//!   only the named user/group entries are packed
//! - `rsync-3.5.1/lib/sysacls.c:2833-2874` `sys_acl_set_info()` - the
//!   per-entry encoding [`fast_io::acl_fd`] mirrors

use std::fs::{File, Permissions};
use std::os::fd::AsFd;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use fast_io::acl_fd::{ExtendedAce, set_extended_acl_fd};
use protocol::acl::{AclCache, NAME_IS_USER, RsyncAcl};

use crate::AclIdMapper;
use crate::MetadataError;

use super::error::is_unsupported_error;
use super::perms::rsync_perms_to_exacl;
use super::reconstruct::resolve_ida_id;
use super::special::{SPECIAL_MODE_BITS, restored_special_mode};

/// Replaces the extended ACL of the file held as `file` with the cached access
/// ACL, then restores its setuid/setgid/sticky bits from `mode`.
///
/// `destination` names the file in errors only; it is never resolved.
pub(super) fn apply_access_acl_fd(
    file: &File,
    destination: &Path,
    cache: &AclCache,
    access_ndx: u32,
    mode: Option<u32>,
    id_map: Option<&AclIdMapper>,
) -> Result<(), MetadataError> {
    let Some(acl) = cache.get_access(access_ndx) else {
        return Ok(());
    };
    let aces = extended_aces(acl, id_map);
    // An empty list clears every entry, as `reset_acl_from_mode()` does by path.
    let op = if aces.is_empty() {
        "reset ACL"
    } else {
        "apply ACL from cache"
    };
    if let Err(e) = set_extended_acl_fd(file.as_fd(), &aces)
        && !is_unsupported_error(&e)
    {
        return Err(MetadataError::new(op, destination, e));
    }
    // upstream: rsync.c:806-820 - restore the special bits after the ACL.
    if let Some(mode) = mode.filter(|m| m & SPECIAL_MODE_BITS != 0) {
        let current = file
            .metadata()
            .map_err(|e| MetadataError::new("inspect permissions", destination, e))?
            .permissions()
            .mode();
        if let Some(restored) = restored_special_mode(current, mode) {
            file.set_permissions(Permissions::from_mode(restored))
                .map_err(|e| {
                    MetadataError::new(
                        "restore setuid/setgid/sticky bits after ACL apply",
                        destination,
                        e,
                    )
                })?;
        }
    }
    Ok(())
}

/// The named entries of `acl` as allow ACEs, the same set
/// `rsync_acl_to_entries()` hands `setfacl` on macOS, with each id resolved
/// once and passed to `mbr_*_to_uuid` directly as upstream does.
fn extended_aces(acl: &RsyncAcl, id_map: Option<&AclIdMapper>) -> Vec<ExtendedAce> {
    acl.names
        .iter()
        .map(|ida| ExtendedAce {
            group: ida.access & NAME_IS_USER == 0,
            id: resolve_ida_id(ida, id_map),
            allow: true,
            perms: rsync_perms_to_exacl(ida.permissions() as u8).bits(),
            flags: 0,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply_acls_from_cache;
    use crate::dest_pin::{DestPin, pin_destination};
    use protocol::acl::IdAccess;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, symlink};
    use std::path::PathBuf;

    /// `root/sub/target` plus `outside/target`, the file a redirected write
    /// would land on.
    struct Tree {
        _dir: tempfile::TempDir,
        root: PathBuf,
        outside: PathBuf,
    }

    impl Tree {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let root = dir.path().join("root");
            let outside = dir.path().join("outside");
            fs::create_dir_all(root.join("sub")).expect("mkdir sub");
            fs::create_dir_all(&outside).expect("mkdir outside");
            fs::write(root.join("sub/target"), b"in").expect("write target");
            fs::write(outside.join("target"), b"out").expect("write victim");
            Self {
                _dir: dir,
                root,
                outside,
            }
        }

        fn destination(&self) -> PathBuf {
            self.root.join("sub/target")
        }

        fn victim(&self) -> PathBuf {
            self.outside.join("target")
        }

        /// Moves `sub` aside and replaces it with a symlink out of the tree.
        fn swap_sub_for_outside(&self) -> PathBuf {
            let moved = self.root.join("held");
            fs::rename(self.root.join("sub"), &moved).expect("move sub aside");
            symlink(&self.outside, self.root.join("sub")).expect("symlink sub");
            moved.join("target")
        }
    }

    /// A read entry for the file owner, who always has a member UUID.
    fn owner_read_cache(path: &Path) -> (AclCache, u32) {
        let uid = fs::metadata(path).expect("stat").uid();
        let mut acl = RsyncAcl::from_mode(0o640);
        acl.names.push(IdAccess::user(uid, 0x04));
        let mut cache = AclCache::new();
        let ndx = cache.store_access(acl);
        (cache, ndx)
    }

    fn has_extended_acl(path: &Path) -> bool {
        !exacl::getfacl(path, None).expect("getfacl").is_empty()
    }

    fn apply_confined(tree: &Tree, cache: &AclCache, ndx: u32) -> Result<(), MetadataError> {
        apply_acls_from_cache(
            &tree.destination(),
            cache,
            ndx,
            None,
            true,
            None,
            None,
            Some(&tree.root),
        )
    }

    #[test]
    fn a_confined_acl_lands_on_the_destination() {
        let tree = Tree::new();
        let (cache, ndx) = owner_read_cache(&tree.destination());
        apply_confined(&tree, &cache, ndx).expect("apply");
        assert!(has_extended_acl(&tree.destination()));
    }

    /// The parent is already a symlink out of the tree when the ACL is applied.
    /// The confined pin refuses, so nothing outside changes; upstream's
    /// path-based set would follow `sub` and write `outside/target`.
    #[test]
    fn a_confined_acl_never_follows_a_parent_symlink_out_of_the_tree() {
        let tree = Tree::new();
        let (cache, ndx) = owner_read_cache(&tree.destination());
        tree.swap_sub_for_outside();
        apply_confined(&tree, &cache, ndx).expect("a refused pin skips the ACL");
        assert!(!has_extended_acl(&tree.victim()));
    }

    /// The parent is swapped between the pin and the apply, the window the
    /// path-based set leaves open. The ACL lands on the held inode.
    #[test]
    fn a_pinned_acl_lands_on_the_held_inode_after_a_parent_swap() {
        let tree = Tree::new();
        let (cache, ndx) = owner_read_cache(&tree.destination());
        let DestPin::Pinned(file) = pin_destination(&tree.destination(), Some(&tree.root)) else {
            panic!("a regular file beneath the root must pin");
        };
        let held = tree.swap_sub_for_outside();
        apply_access_acl_fd(&file, &tree.destination(), &cache, ndx, None, None)
            .expect("apply through the fd");
        assert!(has_extended_acl(&held));
        assert!(!has_extended_acl(&tree.victim()));
    }

    /// An empty ACL clears the entries a previous apply left behind.
    #[test]
    fn an_empty_acl_clears_the_held_inode() {
        let tree = Tree::new();
        let (cache, ndx) = owner_read_cache(&tree.destination());
        apply_confined(&tree, &cache, ndx).expect("apply");
        let mut empty = AclCache::new();
        let empty_ndx = empty.store_access(RsyncAcl::from_mode(0o640));
        apply_confined(&tree, &empty, empty_ndx).expect("clear");
        assert!(!has_extended_acl(&tree.destination()));
    }
}
