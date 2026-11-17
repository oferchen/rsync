//! Compute the default permission bits a child of a directory should inherit.

use std::path::Path;

#[cfg(target_os = "freebsd")]
use exacl::{AclEntryKind, AclOption, getfacl};
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use protocol::acl::trace_default_perms_for_dir;

#[cfg(target_os = "freebsd")]
use super::perms::exacl_perms_to_rsync;

/// Computes the default permission bits a child of `dir` should inherit.
///
/// Mirrors upstream rsync's `default_perms_for_dir` (`acls.c:1453-1508`):
///
/// 1. Start from `ACCESSPERMS & ~umask` (the POSIX default).
/// 2. Read the directory's default POSIX ACL.
/// 3. When the ACL has a `user_obj` entry, fold its bits into the returned
///    permission mask via `rsync_acl_get_perms`.
/// 4. When `--debug=ACL` is at level 1, emit `got ACL-based default perms %o
///    for directory %s` (`acls.c:1502-1503`).
///
/// Unsupported filesystems and missing default ACLs both fall back to the
/// umask-derived default without emitting; only the successful ACL lookup
/// produces the upstream debug line.
///
/// A read error other than `EINVAL`, `ENOTSUP` or `ENOSYS` warns
/// `default_perms_for_dir: sys_acl_get_file(DIR, ACL_TYPE_DEFAULT): STRERROR,
/// falling back on umask` before the umask default applies, as upstream does
/// (`acls.c:1466-1488`). Upstream's dry-run `ENOENT` exemption needs no
/// counterpart: oc's callers never read a default ACL under `--dry-run`.
///
/// On platforms that do not expose POSIX default ACLs (e.g. macOS), this
/// function returns the umask-derived default and never emits, matching the
/// `#ifdef SUPPORT_ACLS` gating at `generator.c:1738-1741`.
///
/// # Upstream Reference
///
/// - `acls.c:1453-1508` `default_perms_for_dir`
/// - `generator.c:1738-1741` and `receiver.c:1200-1206` - the two call sites
///   that fold the returned bits into `dest_mode()` when `--perms` is off.
#[allow(clippy::module_name_repetitions)]
#[must_use]
pub fn default_perms_for_dir(dir: &Path, orig_umask: u32) -> u32 {
    // upstream: acls.c:1462 - perms = ACCESSPERMS & ~orig_umask
    let default_perms = 0o777u32 & !(orig_umask & 0o777);

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        // upstream: acls.c:1460-1464 reads the default ACL of a name relative to
        // the cwd change_dir() entered, so the lookup resolves from the held
        // destination rather than re-resolving `dir` from `/`.
        let read = fast_io::destination_at::with_anchored_path(dir, |spelling| match spelling {
            Some(spelling) => default_acl_at(spelling),
            None => default_acl_beneath_pin(dir),
        });
        let object = match read {
            Ok(object) => object,
            Err(error) => {
                warn_unreadable_default_acl(dir, &error);
                return default_perms;
            }
        };
        let Some(perms) = object.perms() else {
            return default_perms;
        };
        // upstream: acls.c:1502-1503 - DEBUG_GTE(ACL, 1) emission
        if logging::debug_gte(logging::DebugFlag::Acl, 1) {
            trace_default_perms_for_dir(perms, &upstream_dir_name(dir));
        }
        perms
    }

    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    {
        // macOS and other targets without POSIX default ACLs: no emission.
        // upstream: lib/sysacls.c:2763-2768 - the HAVE_OSX_ACLS
        // sys_acl_get_file() answers SMB_ACL_TYPE_DEFAULT with ENOTSUP, which
        // acls.c:1471-1474 turns into the umask default without a warning.
        let _ = dir;
        default_perms
    }
}

/// Warns that the default ACL of `dir` could not be read, unless the error
/// only means the filesystem has no POSIX default ACLs.
///
/// upstream: acls.c:1466-1488 - `EINVAL`, `ENOTSUP` and `ENOSYS` fall back
/// silently; any other errno prints the `FWARNING` line below.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub(super) fn warn_unreadable_default_acl(dir: &Path, error: &std::io::Error) {
    let reason = match error.raw_os_error() {
        Some(libc::EINVAL | libc::ENOTSUP | libc::ENOSYS) => return,
        Some(errno) => logging::strerror(errno),
        // exacl (the FreeBSD read) keeps only the ErrorKind of the OS error
        // (failx.rs `path_err()`), so the silent set is read from the kinds
        // those errnos map to - ENOTSUP and ENOSYS are `Unsupported`, EINVAL
        // is `InvalidInput` - and no strerror text can be recovered.
        None if matches!(
            error.kind(),
            std::io::ErrorKind::Unsupported | std::io::ErrorKind::InvalidInput
        ) =>
        {
            return;
        }
        None => error.kind().to_string(),
    };
    logging::warn_log!(
        "default_perms_for_dir: sys_acl_get_file({}, ACL_TYPE_DEFAULT): {reason}, falling back on umask",
        upstream_dir_name(dir)
    );
}

/// The name upstream prints for `dir`: `dn`, the file-list dirname relative
/// to the destination it entered, or `.` for the destination itself
/// (receiver.c:1201, generator.c:1777). Outside a pinned destination there is
/// no entered directory to be relative to, and `dir` is printed as given.
///
/// Only the spelling comes from the pin; nothing is resolved by name.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn upstream_dir_name(dir: &Path) -> String {
    match fast_io::confinement::pinned_destination_relative(dir) {
        Some((_, relative)) => relative.display().to_string(),
        None => dir.display().to_string(),
    }
}

/// The base entries of a default ACL - the only ones
/// `rsync_acl_get_perms()` reads.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ObjectPerms {
    user_obj: Option<u8>,
    group_obj: Option<u8>,
    mask_obj: Option<u8>,
    other_obj: Option<u8>,
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
impl ObjectPerms {
    /// Collects the base entries from the kernel's `(tag, perm)` xattr
    /// entries; the first of each kind wins.
    #[cfg(target_os = "linux")]
    fn from_xattr(entries: &[(u16, u16)]) -> Self {
        use super::pinned::{ACL_GROUP_OBJ, ACL_MASK, ACL_OTHER, ACL_USER_OBJ};

        let mut object = Self::default();
        for &(tag, perm) in entries {
            let slot = match tag {
                ACL_USER_OBJ => &mut object.user_obj,
                ACL_GROUP_OBJ => &mut object.group_obj,
                ACL_MASK => &mut object.mask_obj,
                ACL_OTHER => &mut object.other_obj,
                _ => continue,
            };
            slot.get_or_insert((perm & 0o7) as u8);
        }
        object
    }

    /// Collects the base entries from a `getfacl` listing; the first of each
    /// kind wins.
    #[cfg(target_os = "freebsd")]
    fn from_entries(entries: &[exacl::AclEntry]) -> Self {
        let mut object = Self::default();
        for entry in entries {
            let slot = match entry.kind {
                AclEntryKind::User if entry.name.is_empty() => &mut object.user_obj,
                AclEntryKind::Group if entry.name.is_empty() => &mut object.group_obj,
                AclEntryKind::Mask => &mut object.mask_obj,
                AclEntryKind::Other => &mut object.other_obj,
                _ => continue,
            };
            slot.get_or_insert(exacl_perms_to_rsync(entry.perms));
        }
        object
    }

    /// The permission bits the default ACL implies, or `None` when it has no
    /// `user_obj` entry.
    ///
    /// upstream: acls.c:1500 - the `racl.user_obj != NO_ENTRY` guard - and
    /// acls.c:133-138 `rsync_acl_get_perms()`:
    ///   `(user_obj << 6) + ((mask_obj != NO_ENTRY ? mask_obj : group_obj) << 3) + other_obj`.
    /// The mask supersedes `group_obj` for the middle bits because it bounds
    /// named users, named groups and `group_obj` in POSIX ACL semantics.
    pub(super) fn perms(&self) -> Option<u32> {
        let user_obj = self.user_obj?;
        let group_bits = self.mask_obj.or(self.group_obj).unwrap_or(0);
        Some(
            (u32::from(user_obj) << 6)
                | (u32::from(group_bits) << 3)
                | u32::from(self.other_obj.unwrap_or(0)),
        )
    }
}

/// Reads the default ACL of the directory `path` names.
///
/// Linux reads the `system.posix_acl_default` xattr, which is what libacl's
/// `acl_get_file(path, ACL_TYPE_DEFAULT)` behind upstream's
/// `sys_acl_get_file()` reads; an absent xattr is an empty ACL.
#[cfg(target_os = "linux")]
fn default_acl_at(path: &Path) -> std::io::Result<ObjectPerms> {
    Ok(ObjectPerms::from_xattr(
        &super::pinned::default_acl_entries_at(path)?,
    ))
}

/// Reads the default ACL of the directory `path` names.
#[cfg(target_os = "freebsd")]
fn default_acl_at(path: &Path) -> std::io::Result<ObjectPerms> {
    getfacl(path, Some(AclOption::DEFAULT_ACL)).map(|entries| ObjectPerms::from_entries(&entries))
}

/// Reads the default ACL of `dir`, a directory beneath the pinned destination
/// that has no path spelling through the pin (no `/proc/self/fd`).
///
/// Linux reads the xattr through a descriptor opened beneath the pin, as
/// upstream's race-safe `xacl_get_fd()` does (lib/acl.c:317-342), so the read
/// still resolves from the entered directory.
#[cfg(target_os = "linux")]
pub(super) fn default_acl_beneath_pin(dir: &Path) -> std::io::Result<ObjectPerms> {
    let dir = std::fs::File::from(fast_io::destination_at::open_dir(dir)?);
    Ok(ObjectPerms::from_xattr(
        &super::pinned::default_acl_entries(&dir)?,
    ))
}

/// Reads the default ACL of `dir` on FreeBSD, which has no `/proc/self/fd`
/// spelling of the pin.
///
/// FreeBSD's `acl_get_file()` has no `*at` form, and its descriptor form
/// (`acl_get_fd_np()`) is not reachable through `exacl` or any safe wrapper,
/// so the default ACL is read by `dir` itself, as oc applies ACLs by path on
/// FreeBSD. Upstream reads the same directory by a name relative to its cwd
/// (acls.c:1464).
#[cfg(target_os = "freebsd")]
fn default_acl_beneath_pin(dir: &Path) -> std::io::Result<ObjectPerms> {
    default_acl_at(dir)
}
