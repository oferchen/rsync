//! Receiver-side application of cached ACLs to destination files.

use std::io;
use std::path::Path;

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use exacl::AclOption;
use exacl::setfacl;
use protocol::acl::AclCache;

use crate::AclIdMapper;
use crate::MetadataError;

use super::error::is_unsupported_error;
use super::reconstruct::{reconstruct_acl, rsync_acl_to_entries};
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use super::reset::clear_default_acl;
use super::reset::reset_acl_from_mode;
use super::special::restore_special_mode_bits;

/// Applies parsed ACLs from an [`AclCache`] to a destination file.
///
/// This is the receiver-side function for applying ACLs that arrived over
/// the wire protocol. The sender encodes ACLs during file list transmission
/// and the receiver stores them in an [`AclCache`]. This function looks up
/// the ACL by index and applies it to the destination path using `setfacl`.
///
/// For directories, both the access ACL and optional default ACL are applied.
/// Symbolic links are skipped since they do not support ACLs on any platform.
///
/// # Arguments
///
/// * `destination` - Path to apply ACLs to.
/// * `cache` - The ACL cache populated during file list reception.
/// * `access_ndx` - Index into the access ACL cache.
/// * `default_ndx` - Optional index into the default ACL cache (directories only).
/// * `follow_symlinks` - Whether to follow symlinks. If `false`, returns immediately.
/// * `mode` - Optional file mode. When present, its bits reconstruct the
///   stripped base ACL entries and restore the setuid/setgid/sticky bits that
///   `setfacl` clears when it re-derives permissions from the ACL.
/// * `id_map` - Optional cross-host id remapper for named ACL entries. When
///   present, each named user/group id is remapped through the received id-list
///   and `--usermap`/`--groupmap`, matching upstream `match_acl_ids()`.
///
/// # Errors
///
/// Returns [`MetadataError`] if applying the ACL fails. Errors from filesystems
/// that do not support ACLs are silently ignored.
///
/// # Upstream Reference
///
/// Mirrors `set_acl()` in `acls.c` lines 930-1001 which applies cached
/// ACLs to destination files during the receiver's metadata application phase.
#[allow(clippy::module_name_repetitions)]
pub fn apply_acls_from_cache(
    destination: &Path,
    cache: &AclCache,
    access_ndx: u32,
    default_ndx: Option<u32>,
    follow_symlinks: bool,
    mode: Option<u32>,
    id_map: Option<&AclIdMapper>,
) -> Result<(), MetadataError> {
    if !follow_symlinks {
        return Ok(());
    }

    if let Some(acl) = cache.get_access(access_ndx) {
        // upstream: acls.c:change_sacl_perms() - reconstruct base entries from mode
        let reconstructed = reconstruct_acl(acl, mode);
        let entries = rsync_acl_to_entries(&reconstructed, id_map);
        if !entries.is_empty() {
            if let Err(e) = setfacl(&[destination], &entries, None) {
                if !is_unsupported_error(&e) {
                    return Err(MetadataError::new(
                        "apply ACL from cache",
                        destination,
                        io::Error::other(e.to_string()),
                    ));
                }
            }
        } else {
            reset_acl_from_mode(destination)?;
        }

        // upstream: acls.c:924-932 + rsync.c:659-660 - applying the access ACL
        // re-derives the permission bits and clears setuid/setgid/sticky, which
        // are not representable in a POSIX ACL. Restore them from the
        // transferred mode so setgid/setuid binaries and sticky dirs survive an
        // ACL transfer.
        if let Some(mode) = mode {
            restore_special_mode_bits(destination, mode)?;
        }
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    if let Some(def_ndx) = default_ndx {
        if let Some(def_acl) = cache.get_default(def_ndx) {
            let entries = rsync_acl_to_entries(def_acl, id_map);
            if !entries.is_empty() {
                if let Err(e) = setfacl(&[destination], &entries, Some(AclOption::DEFAULT_ACL)) {
                    if !is_unsupported_error(&e) {
                        return Err(MetadataError::new(
                            "apply default ACL from cache",
                            destination,
                            io::Error::other(e.to_string()),
                        ));
                    }
                }
            } else {
                clear_default_acl(destination)?;
            }
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    let _ = default_ndx;

    Ok(())
}

/// Stores parsed ACLs from an [`AclCache`] into `--fake-super` xattrs instead
/// of applying them with a real `setfacl`.
///
/// An unprivileged account cannot reliably apply an arbitrary POSIX ACL -
/// particularly named user/group entries - via a real `setfacl`, so under
/// `--fake-super` the encoded ACL is stashed in `%aacl`/`%dacl` xattrs
/// instead, mirroring how ownership is stashed in `%stat`
/// ([`crate::fake_super::store_fake_super`]).
///
/// Symbolic links are skipped: they do not support ACLs (or, for the xattr
/// route, portable `lsetxattr`) on any platform.
///
/// # Arguments
///
/// * `destination` - Path to store the fake-super ACL xattrs on.
/// * `cache` - The ACL cache populated during file list reception.
/// * `access_ndx` - Index into the access ACL cache.
/// * `default_ndx` - Optional index into the default ACL cache (directories only).
/// * `follow_symlinks` - Whether to process this entry at all. If `false`,
///   returns immediately.
///
/// # Errors
///
/// Returns [`MetadataError`] if the underlying xattr operation fails.
///
/// # Upstream Reference
///
/// Mirrors the `am_root < 0` branch of `set_rsync_acl()` in `acls.c` lines
/// 933-971.
#[allow(clippy::module_name_repetitions)]
pub fn store_acls_via_fake_super(
    destination: &Path,
    cache: &AclCache,
    access_ndx: u32,
    default_ndx: Option<u32>,
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    use crate::fake_super::{remove_fake_super_default_acl, store_fake_super_acl};

    if !follow_symlinks {
        return Ok(());
    }

    if let Some(acl) = cache.get_access(access_ndx) {
        store_fake_super_acl(destination, true, acl)
            .map_err(|e| MetadataError::new("store fake-super access ACL", destination, e))?;
    }

    if let Some(def_ndx) = default_ndx {
        match cache.get_default(def_ndx) {
            // upstream: acls.c:934-935 - user_obj == NO_ENTRY means "no default
            // ACL", so the xattr is removed rather than storing an empty ACL.
            Some(def_acl) if !def_acl.has_user_obj() => {
                remove_fake_super_default_acl(destination).map_err(|e| {
                    MetadataError::new("remove fake-super default ACL", destination, e)
                })?;
            }
            Some(def_acl) => {
                store_fake_super_acl(destination, false, def_acl).map_err(|e| {
                    MetadataError::new("store fake-super default ACL", destination, e)
                })?;
            }
            None => {}
        }
    }

    Ok(())
}
