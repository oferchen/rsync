//! Source-to-destination ACL replication (filesystem read, filesystem write).

use std::fs;
use std::io;
use std::path::Path;

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use exacl::AclOption;
use exacl::{getfacl, setfacl};

use crate::MetadataError;

use super::error::is_unsupported_error;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use super::reset::clear_default_acl;
use super::reset::reset_acl_from_mode;
use super::special::restore_special_mode_bits;

/// Synchronizes ACLs from `source` to `destination`.
///
/// Copies the access ACL and, when present on directories, the default ACL.
/// When the source lacks extended ACL entries, the destination's ACL is reset
/// to match its permission bits.
///
/// Symbolic links do not support ACLs; when `follow_symlinks` is `false`,
/// this function returns immediately without performing any work.
///
/// # Errors
///
/// Returns [`MetadataError`] when reading the source ACLs or applying them
/// to the destination fails. Filesystems that report ACLs as unsupported
/// are treated as lacking ACLs and do not trigger an error.
///
/// # Upstream Reference
///
/// - `acls.c`: High-level ACL synchronization logic
/// - `lib/sysacls.c`: Platform-specific ACL wrappers
#[allow(clippy::module_name_repetitions)]
pub fn sync_acls(
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    if !follow_symlinks {
        return Ok(());
    }

    // Pre-check existence - ENOENT from getfacl would be masked by
    // is_unsupported_error() which treats NotFound as "no ACL support".
    if !source.exists() {
        return Err(MetadataError::new(
            "read ACL",
            source,
            io::Error::new(io::ErrorKind::NotFound, "source does not exist"),
        ));
    }

    let source_acl = match getfacl(source, None) {
        Ok(acl) => acl,
        Err(e) => {
            // upstream: acls.c - unsupported fs treated as empty ACL
            if is_unsupported_error(&e) {
                Vec::new()
            } else {
                return Err(MetadataError::new(
                    "read ACL",
                    source,
                    io::Error::other(e.to_string()),
                ));
            }
        }
    };

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    let is_dir = match fs::symlink_metadata(source) {
        Ok(m) => m.is_dir(),
        Err(e) => return Err(MetadataError::new("stat", source, e)),
    };

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    let default_acl = if is_dir {
        match getfacl(source, Some(AclOption::DEFAULT_ACL)) {
            Ok(acl) => Some(acl),
            Err(e) if is_unsupported_error(&e) => None,
            Err(e) => {
                return Err(MetadataError::new(
                    "read default ACL",
                    source,
                    io::Error::other(e.to_string()),
                ));
            }
        }
    } else {
        None
    };

    if !source_acl.is_empty() {
        if let Err(e) = setfacl(&[destination], &source_acl, None) {
            if !is_unsupported_error(&e) {
                return Err(MetadataError::new(
                    "apply ACL",
                    destination,
                    io::Error::other(e.to_string()),
                ));
            }
        }
    } else {
        reset_acl_from_mode(destination)?;
    }

    // upstream: acls.c:924-932 + rsync.c:659-660 - applying the access ACL
    // clears setuid/setgid/sticky, which are not representable in a POSIX ACL.
    // Restore them from the source mode so setgid/setuid binaries and sticky
    // dirs survive a local copy.
    {
        use std::os::unix::fs::PermissionsExt;
        let source_mode = fs::metadata(source)
            .map_err(|e| MetadataError::new("stat", source, e))?
            .permissions()
            .mode();
        restore_special_mode_bits(destination, source_mode)?;
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    if is_dir {
        match default_acl {
            Some(acl) if !acl.is_empty() => {
                if let Err(e) = setfacl(&[destination], &acl, Some(AclOption::DEFAULT_ACL)) {
                    if !is_unsupported_error(&e) {
                        return Err(MetadataError::new(
                            "apply default ACL",
                            destination,
                            io::Error::other(e.to_string()),
                        ));
                    }
                }
            }
            _ => {
                clear_default_acl(destination)?;
            }
        }
    }

    Ok(())
}
