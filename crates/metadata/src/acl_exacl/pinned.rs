//! Race-safe receiver ACL apply through the kernel's POSIX ACL xattr encoding.
//!
//! `acl_set_file` re-resolves the whole path on every call, so a parent
//! component flipped to a symlink between the commit and the ACL apply
//! redirects an attacker-chosen ACL outside the destination tree. Linux stores
//! POSIX ACLs as the `system.posix_acl_access` / `system.posix_acl_default`
//! xattrs, so serialising that format ourselves lets the receiver write it
//! through the confined `O_NOFOLLOW` leaf fd that already pins its xattr
//! writes ([`crate::dest_pin`]). The fd cannot be redirected after the pin.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.1/lib/acl.c:120-153` `acl_to_xattr()` - the on-disk layout
//! - `rsync-3.5.1/lib/acl.c:350-389` `acl_set_common()` /
//!   `acl_del_default_common()` - the fd-driven set and default-ACL delete
//! - `rsync-3.5.1/acls.c:1203,1302` `set_rsync_acl()` - the held-fd arm
//!   taken instead of the path-based `sys_acl_set_file()`

use std::ffi::OsString;
use std::fs::{self, File};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use protocol::acl::{AclCache, NAME_IS_USER, RsyncAcl};

use crate::AclIdMapper;
use crate::MetadataError;
use crate::dest_pin::{DestPin, pin_destination};

use super::error::is_unsupported_error;
use super::read::get_rsync_acl;
use super::reconstruct::{reconstruct_acl, resolve_ida_id};
use super::special::{SPECIAL_MODE_BITS, restored_special_mode};

const ACL_XATTR_ACCESS: &str = "system.posix_acl_access";
const ACL_XATTR_DEFAULT: &str = "system.posix_acl_default";

// upstream: lib/acl.c:70-72 - a 4-byte LE version header, then 8-byte entries.
const ACL_EA_VERSION: u32 = 0x0002;
const ACL_EA_ENT_LEN: usize = 8;

// Kernel tag values (`include/uapi/linux/posix_acl.h`), whose numeric order is
// the canonical entry order the kernel validator expects.
const ACL_USER_OBJ: u16 = 0x01;
const ACL_USER: u16 = 0x02;
const ACL_GROUP_OBJ: u16 = 0x04;
const ACL_GROUP: u16 = 0x08;
const ACL_MASK: u16 = 0x10;
const ACL_OTHER: u16 = 0x20;
const ACL_UNDEFINED_ID: u32 = u32::MAX;

/// What the ACL xattrs are written through.
enum AclTarget<'a> {
    /// The confined `O_NOFOLLOW` leaf fd (upstream's `fd >= 0` arm).
    Fd(File),
    /// A single-component leaf the pin could not open. Written with the
    /// l-variants so a leaf raced into a symlink is never followed; there is
    /// no parent below the root to flip. Upstream's `xacl_set_at(dirfd, leaf)`.
    NoFollow(&'a Path),
    /// A device or socket leaf, addressed as confined parent dirfd + name
    /// through `/proc/self/fd/<dirfd>/<leaf>` with the l-variants. The magic
    /// link resolves to the pinned parent, so a raced parent symlink cannot
    /// redirect the write. Upstream's pre-6.13 `xacl_set_at()` compat.
    At { dir: File, leaf: OsString },
}

impl AclTarget<'_> {
    fn set(&self, name: &str, value: &[u8]) -> io::Result<()> {
        let flags = rustix::fs::XattrFlags::empty();
        match self {
            Self::Fd(file) => rustix::fs::fsetxattr(file, name, value, flags),
            Self::NoFollow(path) => rustix::fs::lsetxattr(*path, name, value, flags),
            Self::At { dir, leaf } => {
                rustix::fs::lsetxattr(proc_fd_leaf_path(dir, leaf), name, value, flags)
            }
        }
        .map_err(io::Error::from)
    }

    fn remove(&self, name: &str) -> io::Result<()> {
        match self {
            Self::Fd(file) => rustix::fs::fremovexattr(file, name),
            Self::NoFollow(path) => rustix::fs::lremovexattr(*path, name),
            Self::At { dir, leaf } => rustix::fs::lremovexattr(proc_fd_leaf_path(dir, leaf), name),
        }
        .map_err(io::Error::from)
    }

    /// The on-disk mode, read without following a leaf symlink.
    fn mode(&self) -> io::Result<u32> {
        match self {
            Self::Fd(file) => Ok(file.metadata()?.permissions().mode()),
            Self::NoFollow(path) => Ok(fs::symlink_metadata(path)?.permissions().mode()),
            Self::At { dir, leaf } => {
                rustix::fs::statat(dir, leaf.as_os_str(), rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
                    .map(|stat| stat.st_mode)
                    .map_err(io::Error::from)
            }
        }
    }

    fn set_mode(&self, mode: u32) -> io::Result<()> {
        match self {
            Self::Fd(file) => file.set_permissions(fs::Permissions::from_mode(mode)),
            Self::NoFollow(path) => fs::set_permissions(path, fs::Permissions::from_mode(mode)),
            // upstream: rsync.c:811 - do_chmod_atfd(dfd, leaf, new_mode).
            Self::At { dir, leaf } => rustix::fs::chmodat(
                dir,
                leaf.as_os_str(),
                rustix::fs::Mode::from_raw_mode(mode),
                rustix::fs::AtFlags::empty(),
            )
            .map_err(io::Error::from),
        }
    }
}

/// `/proc/self/fd/<dirfd>/<leaf>`: the leaf addressed through its pinned
/// parent. upstream: lib/acl.c:188-204 proc_fd_leaf_path().
fn proc_fd_leaf_path(dir: &File, leaf: &OsString) -> PathBuf {
    Path::new("/proc/self/fd")
        .join(dir.as_raw_fd().to_string())
        .join(leaf)
}

/// Whether the `/proc/self/fd` magic links are usable, probed once.
/// upstream: lib/acl.c:401-421 proc_self_fd_usable().
fn proc_self_fd_usable() -> bool {
    static USABLE: OnceLock<bool> = OnceLock::new();
    *USABLE.get_or_init(|| Path::new("/proc/self/fd").is_dir())
}

/// Applies cached ACLs to `destination` without re-resolving its parents.
///
/// `destination` is pinned beneath `confine_root` exactly as the xattr writes
/// are. A pin refused on a slashed path skips the ACL, as upstream's
/// `xattr_refuse` does, rather than falling back to the path.
pub(super) fn apply_acls_pinned(
    destination: &Path,
    confine_root: &Path,
    cache: &AclCache,
    access_ndx: u32,
    default_ndx: Option<u32>,
    mode: Option<u32>,
    id_map: Option<&AclIdMapper>,
) -> Result<(), MetadataError> {
    let target = match pin_destination(destination, Some(confine_root)) {
        DestPin::Pinned(file) => AclTarget::Fd(file),
        DestPin::Parent { dir, leaf } if proc_self_fd_usable() => AclTarget::At { dir, leaf },
        // upstream: acls.c:1315-1321 - with no held fd and no usable dirfd
        // arm (a /proc-less namespace), fall back to the path-based set.
        DestPin::Parent { .. } => AclTarget::NoFollow(destination),
        // upstream: rsync.c:597,800 - `xattr_refuse` skips set_acl_fdat().
        DestPin::Refused => return Ok(()),
        DestPin::Path => AclTarget::NoFollow(destination),
    };
    if matches!(target, AclTarget::NoFollow(_))
        && fs::symlink_metadata(destination).is_ok_and(|m| m.file_type().is_symlink())
    {
        // upstream: rsync.c:800 - a symlink leaf never receives an ACL.
        return Ok(());
    }
    let fail = |op: &'static str| move |e: io::Error| MetadataError::new(op, destination, e);

    if let Some(acl) = cache.get_access(access_ndx) {
        // upstream: acls.c:change_sacl_perms() - reconstruct base entries from mode
        let entries = acl_entries(&reconstruct_acl(acl, mode), id_map);
        let entries = if entries.is_empty() {
            // Same as reset_acl_from_mode(): no entries means the base ACL.
            base_entries(target.mode().map_err(fail("stat"))?)
        } else {
            entries
        };
        write_ignoring_unsupported(target.set(ACL_XATTR_ACCESS, &encode(entries)))
            .map_err(fail("apply ACL from cache"))?;

        // upstream: rsync.c:806-820 - setting the access ACL re-derives the
        // permission bits and drops setuid/setgid/sticky; restore them.
        if let Some(mode) = mode.filter(|m| m & SPECIAL_MODE_BITS != 0) {
            let current = target.mode().map_err(fail("inspect permissions"))?;
            if let Some(restored) = restored_special_mode(current, mode) {
                target
                    .set_mode(restored)
                    .map_err(fail("restore setuid/setgid/sticky bits after ACL apply"))?;
            }
        }
    }

    if let Some(def_ndx) = default_ndx
        && let Some(def_acl) = cache.get_default(def_ndx)
    {
        let entries = acl_entries(def_acl, id_map);
        let result = if entries.is_empty() {
            // upstream: lib/acl.c:381-389 - deleting an absent default ACL succeeds.
            match target.remove(ACL_XATTR_DEFAULT) {
                Err(e) if e.raw_os_error() == Some(libc::ENODATA) => Ok(()),
                other => other,
            }
        } else {
            target.set(ACL_XATTR_DEFAULT, &encode(entries))
        };
        write_ignoring_unsupported(result).map_err(fail("apply default ACL from cache"))?;
    }

    Ok(())
}

/// Replicates the ACLs of `source` onto `destination` through the confined
/// pin, for the local-copy receiver.
///
/// The source ACL is read into the wire form the network receiver applies, so
/// both receivers share one race-safe write path.
pub(crate) fn sync_acls_pinned(
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
    confine_root: &Path,
) -> Result<(), MetadataError> {
    if !follow_symlinks {
        return Ok(());
    }
    let source_meta =
        fs::metadata(source).map_err(|e| MetadataError::new("read ACL", source, e))?;
    let mode = source_meta.permissions().mode();
    let mut cache = AclCache::new();
    let access = cache.store_access(get_rsync_acl(source, mode, false));
    let default = source_meta
        .is_dir()
        .then(|| cache.store_default(get_rsync_acl(source, mode, true)));
    apply_acls_pinned(
        destination,
        confine_root,
        &cache,
        access,
        default,
        Some(mode),
        None,
    )
}

fn write_ignoring_unsupported(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(e) if is_unsupported_error(&e) => Ok(()),
        other => other,
    }
}

/// One kernel ACL entry: `(tag, perm, id)`.
type AclEnt = (u16, u16, u32);

/// Mirrors `rsync_acl_to_entries()` in the kernel's tag/id form.
fn acl_entries(acl: &RsyncAcl, id_map: Option<&AclIdMapper>) -> Vec<AclEnt> {
    let mut entries = Vec::with_capacity(4 + acl.names.len());
    for (present, tag, bits) in [
        (acl.has_user_obj(), ACL_USER_OBJ, acl.user_obj),
        (acl.has_group_obj(), ACL_GROUP_OBJ, acl.group_obj),
        (acl.has_mask_obj(), ACL_MASK, acl.mask_obj),
        (acl.has_other_obj(), ACL_OTHER, acl.other_obj),
    ] {
        if present {
            entries.push((tag, u16::from(bits & 7), ACL_UNDEFINED_ID));
        }
    }
    for ida in acl.names.iter() {
        let tag = if ida.access & NAME_IS_USER != 0 {
            ACL_USER
        } else {
            ACL_GROUP
        };
        let perm = (ida.permissions() & 7) as u16;
        entries.push((tag, perm, resolve_ida_id(ida, id_map)));
    }
    entries
}

/// The minimal access ACL equivalent to `mode` (`exacl::from_mode`).
fn base_entries(mode: u32) -> Vec<AclEnt> {
    vec![
        (ACL_USER_OBJ, ((mode >> 6) & 7) as u16, ACL_UNDEFINED_ID),
        (ACL_GROUP_OBJ, ((mode >> 3) & 7) as u16, ACL_UNDEFINED_ID),
        (ACL_OTHER, (mode & 7) as u16, ACL_UNDEFINED_ID),
    ]
}

/// Serialises entries into the kernel xattr layout, in canonical order.
///
/// upstream: lib/acl.c:120-153 acl_to_xattr()
fn encode(mut entries: Vec<AclEnt>) -> Vec<u8> {
    // upstream: lib/acl.c:109-116 ent_compare() - tag ascending, then id.
    entries.sort_unstable_by_key(|&(tag, _, id)| (tag, id));
    let mut buf = Vec::with_capacity(4 + entries.len() * ACL_EA_ENT_LEN);
    buf.extend_from_slice(&ACL_EA_VERSION.to_le_bytes());
    for (tag, perm, id) in entries {
        buf.extend_from_slice(&tag.to_le_bytes());
        buf.extend_from_slice(&perm.to_le_bytes());
        buf.extend_from_slice(&id.to_le_bytes());
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply_acls_from_cache;
    use protocol::acl::IdAccess;
    use std::os::unix::fs::symlink;

    /// A named user no test host maps, so its entry can only come from us.
    const MARKER_UID: u32 = 54_321;

    /// An access ACL carrying a named-user entry, so the kernel stores it as a
    /// `system.posix_acl_access` xattr instead of folding it into the mode.
    fn marker_cache() -> (AclCache, u32) {
        let mut acl = RsyncAcl::from_mode(0o640);
        acl.names.push(IdAccess::user(MARKER_UID, 0x04));
        let mut cache = AclCache::new();
        let ndx = cache.store_access(acl);
        (cache, ndx)
    }

    fn has_access_acl(path: &Path) -> bool {
        rustix::fs::lgetxattr(path, ACL_XATTR_ACCESS, &mut [0u8; 256][..]).is_ok()
    }

    /// The temp filesystem must store POSIX ACLs, or every assertion below is
    /// vacuous. Probed with the path-based apply, which needs no pin.
    fn acls_supported(dir: &Path) -> bool {
        let probe = dir.join("probe");
        fs::write(&probe, b"p").expect("write probe");
        let (cache, ndx) = marker_cache();
        let applied =
            apply_acls_from_cache(&probe, &cache, ndx, None, true, Some(0o640), None, None).is_ok()
                && has_access_acl(&probe);
        fs::remove_file(&probe).expect("remove probe");
        if !applied {
            eprintln!("skipping: the temp filesystem does not store POSIX ACLs");
        }
        applied
    }

    /// root/ (the confine root) and outside/ with a victim file, canonicalised
    /// so the root is a real directory and only the planted links can escape.
    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let tmp = test_support::create_tempdir();
        let base = fs::canonicalize(tmp.path()).expect("canonicalize");
        let root = base.join("root");
        let outside = base.join("outside");
        fs::create_dir(&root).expect("mkdir root");
        fs::create_dir(&outside).expect("mkdir outside");
        fs::write(outside.join("victim"), b"outside").expect("write victim");
        (tmp, root, outside)
    }

    /// CVE-2026-53799: a parent component swapped for a symlink out of the tree
    /// between the commit and the ACL apply must not carry the ACL outside.
    /// `acl_set_file` re-walks parents, so the path-based apply writes the
    /// victim; the confined pin refuses the escaping parent and skips.
    /// upstream: rsync.c:582-597,800 + acls.c:1302.
    #[test]
    fn a_swapped_parent_does_not_carry_the_acl_outside_the_confine_root() {
        let (_tmp, root, outside) = fixture();
        if !acls_supported(&root) {
            return;
        }
        symlink(&outside, root.join("sub")).expect("plant escaping parent");
        let (cache, ndx) = marker_cache();

        let _ = apply_acls_from_cache(
            &root.join("sub/victim"),
            &cache,
            ndx,
            None,
            true,
            Some(0o640),
            None,
            Some(&root),
        );

        assert!(
            !has_access_acl(&outside.join("victim")),
            "the ACL was applied to a file OUTSIDE the confine root through a swapped parent"
        );
    }

    /// The leaf itself swapped for a symlink to an outside file: the pin opens
    /// it `O_NOFOLLOW`, so the ACL never reaches the target.
    #[test]
    fn a_swapped_leaf_does_not_carry_the_acl_outside_the_confine_root() {
        let (_tmp, root, outside) = fixture();
        if !acls_supported(&root) {
            return;
        }
        fs::create_dir(root.join("dir")).expect("mkdir dir");
        symlink(outside.join("victim"), root.join("dir/leaf")).expect("plant leaf link");
        let (cache, ndx) = marker_cache();

        let _ = apply_acls_from_cache(
            &root.join("dir/leaf"),
            &cache,
            ndx,
            None,
            true,
            Some(0o640),
            None,
            Some(&root),
        );

        assert!(
            !has_access_acl(&outside.join("victim")),
            "the ACL followed a swapped leaf symlink out of the confine root"
        );
    }

    /// A top-level leaf has no parent to pin, so the pin falls back to the
    /// path - but through the l-variants, which never follow the leaf.
    /// upstream: lib/acl.c:232-255 do_setxattr() dirfd+leaf arm.
    #[test]
    fn a_swapped_top_level_leaf_is_not_followed() {
        let (_tmp, root, outside) = fixture();
        if !acls_supported(&root) {
            return;
        }
        symlink(outside.join("victim"), root.join("leaf")).expect("plant leaf link");
        let (cache, ndx) = marker_cache();

        let _ = apply_acls_from_cache(
            &root.join("leaf"),
            &cache,
            ndx,
            None,
            true,
            Some(0o640),
            None,
            Some(&root),
        );

        assert!(
            !has_access_acl(&outside.join("victim")),
            "the ACL followed a top-level leaf symlink out of the confine root"
        );
    }

    /// Non-vacuity: the pinned arm must still apply the ACL to a legitimate
    /// in-tree file, named entry, mask and setgid bit included. Without this
    /// the refusals above would pass for an apply that writes nothing.
    #[test]
    fn the_pinned_apply_writes_the_acl_in_tree() {
        let (_tmp, root, _outside) = fixture();
        if !acls_supported(&root) {
            return;
        }
        fs::create_dir(root.join("dir")).expect("mkdir dir");
        let file = root.join("dir/file");
        fs::write(&file, b"in tree").expect("write file");
        let (cache, ndx) = marker_cache();
        let mode = 0o2640;

        apply_acls_from_cache(
            &file,
            &cache,
            ndx,
            None,
            true,
            Some(mode),
            None,
            Some(&root),
        )
        .expect("pinned apply");

        let acl = exacl::getfacl(&file, None).expect("getfacl");
        assert!(
            acl.iter()
                .any(|e| e.kind == exacl::AclEntryKind::User && e.name == MARKER_UID.to_string()),
            "named user entry missing after the pinned apply: {acl:?}"
        );
        let on_disk = fs::metadata(&file).expect("stat").permissions().mode();
        assert_eq!(on_disk & 0o7777, mode, "mode or setgid bit not restored");
    }

    /// A FIFO is pinned like upstream's `S_ISFIFO` arm, with `O_NONBLOCK`: the
    /// apply must return without a writer and still land the ACL. A blocking
    /// open would hang here rather than fail.
    /// upstream: rsync.c:563-571.
    #[test]
    fn a_fifo_entry_receives_the_acl_without_blocking() {
        let (_tmp, root, _outside) = fixture();
        if !acls_supported(&root) {
            return;
        }
        fs::create_dir(root.join("dir")).expect("mkdir dir");
        let fifo = root.join("dir/pipe");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o640),
            0,
        )
        .expect("mkfifo");
        let (cache, ndx) = marker_cache();

        apply_acls_from_cache(
            &fifo,
            &cache,
            ndx,
            None,
            true,
            Some(0o640),
            None,
            Some(&root),
        )
        .expect("pinned apply on a FIFO");

        assert!(has_access_acl(&fifo), "the FIFO did not receive the ACL");
    }

    /// A socket is never opened (upstream pins only regular files,
    /// directories and FIFOs), so its ACL goes through the confined parent
    /// dirfd and `/proc/self/fd`, never the re-resolved path.
    /// upstream: acls.c:1300-1304 xacl_set_at(), lib/acl.c:232-255.
    #[test]
    fn a_socket_entry_receives_the_acl_through_its_parent_dirfd() {
        let (_tmp, root, _outside) = fixture();
        if !acls_supported(&root) {
            return;
        }
        fs::create_dir(root.join("dir")).expect("mkdir dir");
        let sock = root.join("dir/sock");
        let _listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind socket");
        assert!(
            matches!(pin_destination(&sock, Some(&root)), DestPin::Parent { .. }),
            "a socket leaf must take the parent-dirfd arm"
        );
        let (cache, ndx) = marker_cache();

        apply_acls_from_cache(
            &sock,
            &cache,
            ndx,
            None,
            true,
            Some(0o640),
            None,
            Some(&root),
        )
        .expect("pinned apply on a socket");

        assert!(has_access_acl(&sock), "the socket did not receive the ACL");
    }

    /// The local-copy receiver replicates a source ACL through the same pin:
    /// a destination parent raced into a symlink out of the tree must not
    /// carry the source's ACL onto the outside file. The unconfined run is
    /// the control that proves the fixture redirects a path-based sync.
    /// upstream: rsync.c:555-599,800 - a local copy's receiver half.
    #[test]
    fn a_local_copy_acl_sync_is_pinned_beneath_the_destination_root() {
        let (_tmp, root, outside) = fixture();
        if !acls_supported(&root) {
            return;
        }
        let source = root.join("source");
        fs::write(&source, b"source").expect("write source");
        let (cache, ndx) = marker_cache();
        apply_acls_from_cache(&source, &cache, ndx, None, true, Some(0o640), None, None)
            .expect("plant the source ACL");
        symlink(&outside, root.join("sub")).expect("plant escaping parent");
        let victim = outside.join("victim");

        crate::sync_acls_confined(&source, &root.join("sub/victim"), true, Some(&root))
            .expect("a refused pin skips the ACL");
        assert!(
            !has_access_acl(&victim),
            "the local-copy ACL sync wrote outside the destination root"
        );

        crate::sync_acls_confined(&source, &root.join("sub/victim"), true, None)
            .expect("unconfined control sync");
        assert!(
            has_access_acl(&victim),
            "control: the path-based sync must follow the swapped parent"
        );
    }

    /// The default ACL of a directory is set, then removed by an empty one,
    /// both through the pinned directory fd.
    #[test]
    fn the_pinned_apply_sets_and_clears_a_default_acl() {
        let (_tmp, root, _outside) = fixture();
        if !acls_supported(&root) {
            return;
        }
        let dir = root.join("dir");
        fs::create_dir(&dir).expect("mkdir dir");
        let mut cache = AclCache::new();
        let access = cache.store_access(RsyncAcl::from_mode(0o750));
        let mut def = RsyncAcl::from_mode(0o750);
        def.names.push(IdAccess::user(MARKER_UID, 0x05));
        def.mask_obj = 0x05;
        let def_ndx = cache.store_default(def);
        let has_default =
            |p: &Path| rustix::fs::lgetxattr(p, ACL_XATTR_DEFAULT, &mut [0u8; 256][..]).is_ok();

        apply_acls_from_cache(
            &dir,
            &cache,
            access,
            Some(def_ndx),
            true,
            Some(0o750),
            None,
            Some(&root),
        )
        .expect("set default ACL");
        assert!(has_default(&dir), "default ACL not written through the pin");

        let cleared = cache.store_default(RsyncAcl::new());
        apply_acls_from_cache(
            &dir,
            &cache,
            access,
            Some(cleared),
            true,
            Some(0o750),
            None,
            Some(&root),
        )
        .expect("clear default ACL");
        assert!(
            !has_default(&dir),
            "empty default ACL did not remove the xattr"
        );
    }

    /// Golden bytes for `u::rw-,u:1000:r--,g::r--,m::r--,o::---`, given out of
    /// order: the encoder must sort into the kernel's canonical order, or
    /// `setxattr` rejects the ACL with `EINVAL`.
    #[test]
    fn encode_sorts_into_the_kernel_layout() {
        let encoded = encode(vec![
            (ACL_OTHER, 0, ACL_UNDEFINED_ID),
            (ACL_USER, 4, 1000),
            (ACL_MASK, 4, ACL_UNDEFINED_ID),
            (ACL_GROUP_OBJ, 4, ACL_UNDEFINED_ID),
            (ACL_USER_OBJ, 6, ACL_UNDEFINED_ID),
        ]);
        let expected: Vec<u8> = [
            &[2, 0, 0, 0][..],
            &[0x01, 0, 6, 0, 0xff, 0xff, 0xff, 0xff],
            &[0x02, 0, 4, 0, 0xe8, 0x03, 0, 0],
            &[0x04, 0, 4, 0, 0xff, 0xff, 0xff, 0xff],
            &[0x10, 0, 4, 0, 0xff, 0xff, 0xff, 0xff],
            &[0x20, 0, 0, 0, 0xff, 0xff, 0xff, 0xff],
        ]
        .concat();
        assert_eq!(encoded, expected);
    }

    #[test]
    fn base_entries_follow_the_mode_bits() {
        assert_eq!(
            base_entries(0o4750),
            vec![
                (ACL_USER_OBJ, 7, ACL_UNDEFINED_ID),
                (ACL_GROUP_OBJ, 5, ACL_UNDEFINED_ID),
                (ACL_OTHER, 0, ACL_UNDEFINED_ID),
            ]
        );
    }
}
