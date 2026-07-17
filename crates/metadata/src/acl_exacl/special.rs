//! Restoration of setuid/setgid/sticky bits after an access ACL is applied.
//!
//! Applying a POSIX access ACL via `acl_set_file`/`setfacl` re-derives the
//! file's permission bits from the ACL's USER_OBJ/GROUP_OBJ/OTHER entries and
//! clears the high mode bits (setuid/setgid/sticky) that cannot be represented
//! in a POSIX ACL. Upstream restores them afterwards through the final
//! `do_chmod_at()` in `set_file_attrs()`; this module provides the equivalent
//! fixup for oc-rsync, which applies ACLs after the permission chmod.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::MetadataError;

/// Special mode bits not representable in a POSIX ACL: setuid, setgid, sticky.
const SPECIAL_MODE_BITS: u32 = 0o7000;

/// Re-applies the setuid/setgid/sticky bits from `mode` after an access ACL has
/// been set, restoring the high bits that `setfacl`/`acl_set_file` clears when
/// it re-derives the permission bits from the ACL's base entries.
///
/// The ACL-derived permission bits (low 0o777) are preserved verbatim; only the
/// special bits are OR'd back on, so the ACL mask written by `setfacl` is not
/// disturbed. A `chmod` is issued only when the special bits actually differ
/// from what is already on disk.
///
/// # Upstream Reference
///
/// - `acls.c:924-932` `change_sacl_perms()` - keeps the special bits out of the
///   ACL-derived mode (`(old_mode & ~ACCESSPERMS) | (mode & ACCESSPERMS)`) and,
///   under `SMB_ACL_LOSES_SPECIAL_MODE_BITS`, forces a later chmod to restore
///   any lost setid bits.
/// - `rsync.c:659-660` `set_file_attrs()` - the final
///   `do_chmod_at(fname, new_mode)` after `set_acl()` re-applies any special
///   bit the ACL application dropped.
pub(super) fn restore_special_mode_bits(path: &Path, mode: u32) -> Result<(), MetadataError> {
    let special = mode & SPECIAL_MODE_BITS;
    if special == 0 {
        // setfacl only ever clears special bits, never sets them, so when the
        // desired mode carries none there is nothing to restore.
        return Ok(());
    }

    let current = fs::metadata(path)
        .map_err(|e| MetadataError::new("inspect permissions", path, e))?
        .permissions()
        .mode();

    // upstream: rsync.c:659 - the chmod is skipped when the on-disk mode already
    // matches, mirroring `BITS_EQUAL(sxp->st.st_mode, new_mode, CHMOD_BITS)`.
    if current & SPECIAL_MODE_BITS == special {
        return Ok(());
    }

    let restored = (current & 0o777) | special;
    fs::set_permissions(path, fs::Permissions::from_mode(restored)).map_err(|e| {
        MetadataError::new("restore setuid/setgid/sticky bits after ACL apply", path, e)
    })
}
