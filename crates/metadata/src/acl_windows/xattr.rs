//! Reserved xattr slot carrying the Windows SDDL security descriptor for
//! Windows-to-Windows DACL fidelity.

use std::io;
use std::path::Path;

use protocol::xattr::{XattrEntry, XattrList};

use super::common::io_error_is_unsupported;
use super::sddl::{read_dacl_sddl, write_dacl_sddl};
use crate::MetadataError;

/// Reserved xattr key carrying the full SDDL security descriptor for
/// Windows-to-Windows DACL fidelity.
///
/// Mirrors Samba's `user.win32.security_descriptor` slot so external
/// tooling that already understands the convention can interoperate with
/// oc-rsync transfers without protocol changes. See
/// `docs/design/windows-ntfs-acl-support.md` section 4.2.
pub const WINDOWS_SDDL_XATTR_NAME: &[u8] = b"user.win32.security_descriptor";

/// Builds an [`XattrEntry`] carrying the full SDDL descriptor for `path`.
///
/// Returns `Ok(None)` when the path cannot be read or carries no DACL
/// (matching the conservative posture of the underlying `read_dacl`).
/// Higher layers append the returned entry to the xattr list emitted by
/// `read_xattrs_for_wire` so a Windows receiver can restore the
/// descriptor verbatim via [`apply_sddl_from_xattrs`].
///
/// # Errors
///
/// Returns [`io::Error`] propagated from [`read_dacl_sddl`] only when the
/// failure is not "filesystem does not support security descriptors";
/// those benign failures collapse to `Ok(None)`.
pub fn sddl_xattr_entry(path: &Path) -> io::Result<Option<XattrEntry>> {
    match read_dacl_sddl(path) {
        Ok(sddl) if !sddl.is_empty() => Ok(Some(XattrEntry::new(
            WINDOWS_SDDL_XATTR_NAME.to_vec(),
            sddl.into_bytes(),
        ))),
        Ok(_) => Ok(None),
        Err(error) => {
            if io_error_is_unsupported(&error) {
                Ok(None)
            } else {
                Err(error)
            }
        }
    }
}

/// Looks for the reserved SDDL xattr inside `xattr_list`.
///
/// Returns the parsed SDDL string when present so callers can apply it
/// with [`write_dacl_sddl`] or lower it to a POSIX mode with
/// [`super::posix_map::dacl_to_posix_mode`].
#[must_use]
pub fn find_sddl_in_xattrs(xattr_list: &XattrList) -> Option<&str> {
    let entry = xattr_list.find_by_name(WINDOWS_SDDL_XATTR_NAME)?;
    if entry.is_abbreviated() {
        return None;
    }
    std::str::from_utf8(entry.datum()).ok()
}

/// Applies the SDDL security descriptor carried inside `xattr_list` to
/// `path` via [`write_dacl_sddl`].
///
/// Returns `Ok(true)` when the reserved SDDL xattr was found and applied,
/// `Ok(false)` when no SDDL payload was present (so the caller falls back
/// to the cross-platform named-ACE path).
///
/// # Errors
///
/// Returns [`MetadataError`] when the descriptor cannot be applied.
/// Failures equivalent to "filesystem does not support security
/// descriptors" swallow silently so transfers do not abort on FAT32 or
/// network mounts.
pub fn apply_sddl_from_xattrs(path: &Path, xattr_list: &XattrList) -> Result<bool, MetadataError> {
    let Some(sddl) = find_sddl_in_xattrs(xattr_list) else {
        return Ok(false);
    };
    match write_dacl_sddl(path, sddl) {
        Ok(()) => Ok(true),
        Err(error) => {
            if io_error_is_unsupported(&error) {
                Ok(true)
            } else {
                Err(MetadataError::new("apply SDDL xattr", path, error))
            }
        }
    }
}
