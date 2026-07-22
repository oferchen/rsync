//! Cross-platform `sync_acls` integration that prefers the SDDL
//! round-trip on Windows-to-Windows transfers and falls back to the
//! lossy named-ACE encoder when the volume cannot serve a security
//! descriptor.

use std::io;
use std::path::Path;

use super::common::io_error_is_unsupported;
use super::dacl::{apply_rsync_acl_to_path, dacl_to_rsync_acl, read_dacl};
use super::sddl::{read_dacl_sddl, write_dacl_sddl};
use crate::MetadataError;

/// Synchronises the DACL from `source` to `destination`.
///
/// Reads the source's DACL, encodes it as a [`protocol::acl::RsyncAcl`],
/// and re-applies it to the destination. Symlinks are not followed when
/// `follow_symlinks` is `false`, matching the POSIX path's contract.
///
/// # Errors
///
/// Returns [`MetadataError`] on Win32 failures. Filesystems reporting no
/// ACL support are silently treated as success.
///
/// # Upstream Reference
///
/// Combines `acls.c:get_rsync_acl()` and `set_acl()`.
pub fn sync_acls(
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    if !follow_symlinks {
        return Ok(());
    }

    if !source.exists() {
        return Err(MetadataError::new(
            "read ACL",
            source,
            io::Error::new(io::ErrorKind::NotFound, "source does not exist"),
        ));
    }

    // Preferred Windows-to-Windows path: round-trip the full SDDL via
    // the WAS-2/3 helpers so owner, group, and the complete DACL transfer
    // verbatim. Falls back to the lossy named-ACE encoder when the volume
    // refuses to serve a descriptor (FAT32, network mounts).
    match read_dacl_sddl(source) {
        Ok(sddl) if !sddl.is_empty() => {
            return write_dacl_sddl(destination, &sddl)
                .map_err(|error| MetadataError::new("write SDDL", destination, error));
        }
        Ok(_) => {}
        Err(error) => {
            if !io_error_is_unsupported(&error) {
                return Err(MetadataError::new("read SDDL", source, error));
            }
        }
    }

    let (sd, pdacl) = read_dacl(source)?;
    if pdacl.is_null() {
        drop(sd);
        return Ok(());
    }

    let acl = dacl_to_rsync_acl(pdacl);
    drop(sd);
    if acl.names.is_empty() {
        return Ok(());
    }

    apply_rsync_acl_to_path(destination, &acl)
}

/// Fake-super variant of [`sync_acls`] for Windows.
///
/// `--fake-super`'s `%aacl`/`%dacl` stashing is a POSIX-ACL-only mechanism
/// (see [`super::dacl::store_acls_via_fake_super`]); Windows ACLs are already
/// persisted via their own SDDL xattr, so this falls straight through to the
/// normal sync path unchanged.
///
/// # Errors
///
/// Returns [`MetadataError`] on unrecoverable Win32 failures.
pub fn sync_acls_via_fake_super(
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    sync_acls(source, destination, follow_symlinks)
}
