//! Reset helpers used when the source has no extended ACL entries.
//!
//! These functions strip the destination's extended entries so that only the
//! base owner/group/other bits remain, matching upstream rsync behaviour when
//! the source ACL is empty.

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use std::fs;
use std::io;
use std::path::Path;

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use exacl::AclOption;
use exacl::setfacl;

use crate::MetadataError;

use super::error::is_unsupported_error;

/// Resets the access ACL to match the file's permission bits.
///
/// On Linux and FreeBSD, converts the file's Unix permission mode into a
/// minimal ACL using [`exacl::from_mode`]. On macOS, clears extended ACL
/// entries by setting an empty ACL list.
///
/// # Errors
///
/// Returns [`MetadataError`] if reading file metadata or setting the ACL
/// fails (unsupported filesystem errors are silently ignored).
///
/// # Upstream Reference
///
/// Matches upstream rsync's behavior when the source has no extended
/// ACL entries - the destination retains only base owner/group/other entries.
pub(super) fn reset_acl_from_mode(path: &Path) -> Result<(), MetadataError> {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        let metadata = match fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) => return Err(MetadataError::new("stat", path, e)),
        };

        let mode = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o777
        };

        let base_acl = exacl::from_mode(mode);

        if let Err(e) = setfacl(&[path], &base_acl, None) {
            if !is_unsupported_error(&e) {
                return Err(MetadataError::new(
                    "reset ACL",
                    path,
                    io::Error::other(e.to_string()),
                ));
            }
        }
    }

    // macOS lacks exacl::from_mode - clear extended entries with empty list.
    // Permission bits are managed separately from the extended ACL.
    #[cfg(target_os = "macos")]
    {
        if let Err(e) = setfacl(&[path], &[], None) {
            if !is_unsupported_error(&e) {
                return Err(MetadataError::new(
                    "reset ACL",
                    path,
                    io::Error::other(e.to_string()),
                ));
            }
        }
    }

    Ok(())
}

/// Clears the default ACL from a directory (Linux/FreeBSD only).
///
/// Setting an empty default ACL removes it (equivalent to `setfacl -k`).
/// Unsupported-filesystem errors are silently ignored.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub(super) fn clear_default_acl(path: &Path) -> Result<(), MetadataError> {
    if let Err(e) = setfacl(&[path], &[], Some(AclOption::DEFAULT_ACL)) {
        if !is_unsupported_error(&e) {
            return Err(MetadataError::new(
                "clear default ACL",
                path,
                io::Error::other(e.to_string()),
            ));
        }
    }
    Ok(())
}
