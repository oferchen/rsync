#![cfg(all(
    feature = "acl",
    any(target_os = "linux", target_os = "macos", target_os = "freebsd")
))]

//! Cross-platform ACL synchronization using the `exacl` crate.
//!
//! This module provides ACL synchronization for Linux (POSIX ACLs), macOS
//! (extended ACLs), and FreeBSD (POSIX and NFSv4 ACLs) using the `exacl`
//! crate for a unified, safe abstraction.
//!
//! # Design
//!
//! The [`sync_acls`] function coordinates the full ACL replication workflow:
//!
//! - Read access and default ACLs from the source without following symbolic
//!   links unless explicitly requested.
//! - Apply the retrieved ACLs to the destination.
//! - When the source has no extended ACL entries, reset the destination to
//!   match its permission bits.
//!
//! # Platform Differences
//!
//! - **Linux**: Uses POSIX ACLs with access and default ACL types.
//! - **macOS**: Uses extended ACLs (NFSv4-style); no default ACLs on directories.
//! - **FreeBSD**: Supports both POSIX and NFSv4 ACLs depending on filesystem.
//!
//! The `exacl` crate handles these differences internally.
//!
//! # Upstream Reference
//!
//! The behavior mirrors upstream rsync's ACL handling in `acls.c` and
//! `lib/sysacls.c`, where:
//! - ACL read/write errors on unsupported filesystems are silently ignored.
//! - Symbolic links do not receive ACL updates (Linux doesn't support link ACLs).
//! - Default ACLs are handled for directories on platforms that support them.
//!
//! # Examples
//!
//! ```rust,ignore
//! use ::metadata::sync_acls;
//! use std::path::Path;
//!
//! # fn demo() -> Result<(), ::metadata::MetadataError> {
//! let source = Path::new("src");
//! let destination = Path::new("dst");
//! sync_acls(source, destination, true)?;
//! # Ok(())
//! # }
//! ```

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use std::fs;
use std::io;
use std::path::Path;

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use exacl::AclOption;
use exacl::{AclEntry, Perm, getfacl, setfacl};
use protocol::acl::{AclCache, NAME_IS_USER, RsyncAcl};

use crate::MetadataError;

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

    // Verify source exists before reading ACLs — an ENOENT from getfacl
    // would otherwise be masked by is_unsupported_error() which treats
    // NotFound as "filesystem lacks ACL support".
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
            // Treat unsupported filesystems as "no ACL"
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
/// ACL entries — the destination retains only base owner/group/other entries.
fn reset_acl_from_mode(path: &Path) -> Result<(), MetadataError> {
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

    // macOS: clear extended ACL entries by setting an empty list.
    // macOS does not have exacl::from_mode; the permission bits are
    // managed separately from the extended ACL.
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

/// Clears the default ACL from a directory.
///
/// On Linux and FreeBSD, directories can have default ACLs that are
/// automatically applied to newly created files within the directory.
/// This function removes the default ACL by setting it to an empty list.
///
/// # Platform Support
///
/// Only compiled on Linux and FreeBSD, where default ACLs are supported.
/// macOS does not use default ACLs in its extended ACL model.
///
/// # Errors
///
/// Returns [`MetadataError`] if clearing the ACL fails, except for
/// unsupported filesystem errors which are silently ignored.
///
/// # Note
///
/// Setting an empty default ACL is the standard way to clear it on
/// POSIX ACL systems. This is equivalent to `setfacl -k` on the command line.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn clear_default_acl(path: &Path) -> Result<(), MetadataError> {
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

/// Checks if an I/O error indicates an unsupported filesystem.
///
/// Determines whether an error should be treated as "filesystem doesn't
/// support ACLs" rather than a true error. This allows graceful degradation
/// when copying files across different filesystem types.
///
/// # Detection Strategy
///
/// The function uses multiple detection methods in order:
/// 1. **Error kind matching**: Checks for `Unsupported`, `InvalidInput`, `NotFound`
/// 2. **OS error codes**: Checks for `ENOTSUP`, `ENOENT`, `EINVAL`, `ENODATA`, `EPERM`
/// 3. **Error message parsing**: Looks for common error message patterns
///
/// # Common Scenarios
///
/// This function returns `true` for:
/// - FAT/VFAT filesystems (don't support ACLs)
/// - Network mounts without ACL support
/// - Permission errors when reading ACLs
/// - Missing xattr support in the kernel
///
/// # Upstream Reference
///
/// Matches upstream rsync's `no_acl_syscall_error()` behavior where errors
/// from filesystems that don't support ACLs are silently ignored.
fn is_unsupported_error(e: &io::Error) -> bool {
    // Check by error kind first
    matches!(
        e.kind(),
        io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput | io::ErrorKind::NotFound
    ) || {
        // Fall back to raw OS error codes
        match e.raw_os_error() {
            Some(libc::ENOTSUP) | Some(libc::ENOENT) | Some(libc::EINVAL) | Some(libc::ENODATA)
            | Some(libc::EPERM) => true,
            _ => {
                // Check error message as last resort
                let msg = e.to_string().to_lowercase();
                msg.contains("not supported")
                    || msg.contains("no such file")
                    || msg.contains("invalid argument")
                    || msg.contains("no data")
            }
        }
    }
}

/// Converts rsync permission bits (3-bit rwx) to [`exacl::Perm`] flags.
fn rsync_perms_to_exacl(bits: u8) -> Perm {
    let mut perms = Perm::empty();
    if bits & 0x04 != 0 {
        perms |= Perm::READ;
    }
    if bits & 0x02 != 0 {
        perms |= Perm::WRITE;
    }
    if bits & 0x01 != 0 {
        perms |= Perm::EXECUTE;
    }
    perms
}

/// Converts a [`RsyncAcl`] from the wire protocol into a list of [`AclEntry`]
/// values suitable for [`exacl::setfacl`].
///
/// On Linux/FreeBSD, the resulting list contains POSIX ACL entries (user_obj,
/// group_obj, mask, other, plus named user/group entries). On macOS, only
/// named user/group entries are emitted as extended ACL entries since the
/// base permissions are managed separately through file mode bits.
///
/// # Upstream Reference
///
/// Mirrors `set_rsync_acl()` in `acls.c` lines 835-928 which reconstructs
/// a system ACL from the wire protocol `rsync_acl` struct.
fn rsync_acl_to_entries(acl: &RsyncAcl) -> Vec<AclEntry> {
    let mut entries = Vec::new();

    // Base entries (user_obj, group_obj, other_obj, mask_obj) are only
    // used on Linux/FreeBSD where POSIX ACLs include these. On macOS,
    // the base mode bits are managed separately.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        if acl.has_user_obj() {
            entries.push(AclEntry::allow_user(
                "",
                rsync_perms_to_exacl(acl.user_obj),
                None,
            ));
        }
        if acl.has_group_obj() {
            entries.push(AclEntry::allow_group(
                "",
                rsync_perms_to_exacl(acl.group_obj),
                None,
            ));
        }
        if acl.has_mask_obj() {
            entries.push(AclEntry::allow_mask(
                rsync_perms_to_exacl(acl.mask_obj),
                None,
            ));
        }
        if acl.has_other_obj() {
            entries.push(AclEntry::allow_other(
                rsync_perms_to_exacl(acl.other_obj),
                None,
            ));
        }
    }

    for ida in acl.names.iter() {
        let perms = rsync_perms_to_exacl(ida.permissions() as u8);
        let name = ida.id.to_string();

        if ida.access & NAME_IS_USER != 0 {
            entries.push(AclEntry::allow_user(&name, perms, None));
        } else {
            entries.push(AclEntry::allow_group(&name, perms, None));
        }
    }

    entries
}

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
) -> Result<(), MetadataError> {
    if !follow_symlinks {
        return Ok(());
    }

    if let Some(acl) = cache.get_access(access_ndx) {
        let entries = rsync_acl_to_entries(acl);
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
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    if let Some(def_ndx) = default_ndx {
        if let Some(def_acl) = cache.get_default(def_ndx) {
            let entries = rsync_acl_to_entries(def_acl);
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

#[cfg(test)]
mod tests {
    use super::*;
    use exacl::AclEntryKind;
    use std::fs::File;
    use tempfile::tempdir;

    #[test]
    fn sync_acls_skips_when_not_following_symlinks() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("src");
        let destination = dir.path().join("dst");
        File::create(&source).expect("create src");
        File::create(&destination).expect("create dst");

        // Should return Ok without doing anything
        let result = sync_acls(&source, &destination, false);
        assert!(result.is_ok());
    }

    #[test]
    fn sync_acls_copies_between_regular_files() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("src");
        let destination = dir.path().join("dst");
        File::create(&source).expect("create src");
        File::create(&destination).expect("create dst");

        // Should succeed for files on same filesystem
        let result = sync_acls(&source, &destination, true);
        assert!(result.is_ok());
    }

    #[test]
    fn sync_acls_works_with_directories() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("src_dir");
        let destination = dir.path().join("dst_dir");
        std::fs::create_dir(&source).expect("create src_dir");
        std::fs::create_dir(&destination).expect("create dst_dir");

        let result = sync_acls(&source, &destination, true);
        assert!(result.is_ok());
    }

    #[test]
    fn reset_acl_from_mode_works() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("test");
        File::create(&file).expect("create file");

        let result = reset_acl_from_mode(&file);
        assert!(result.is_ok());
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn clear_default_acl_works_on_directory() {
        let dir = tempdir().expect("tempdir");
        let subdir = dir.path().join("subdir");
        std::fs::create_dir(&subdir).expect("create subdir");

        let result = clear_default_acl(&subdir);
        assert!(result.is_ok());
    }

    #[test]
    fn is_unsupported_error_detects_common_messages() {
        // Test various error message patterns
        let patterns = [
            "operation not supported",
            "No such file or directory",
            "Invalid argument",
            "No data available",
        ];

        for pattern in patterns {
            let err = std::io::Error::other(pattern);
            assert!(
                is_unsupported_error(&err),
                "should detect '{pattern}' as unsupported"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn is_unsupported_error_detects_os_error_codes() {
        // Test raw OS error codes
        let codes = [libc::ENOTSUP, libc::ENOENT, libc::EINVAL, libc::EPERM];

        for code in codes {
            let err = std::io::Error::from_raw_os_error(code);
            assert!(
                is_unsupported_error(&err),
                "should detect OS error code {code} as unsupported"
            );
        }
    }

    #[test]
    fn rsync_perms_to_exacl_all_bits() {
        assert_eq!(rsync_perms_to_exacl(0x00), Perm::empty());
        assert_eq!(rsync_perms_to_exacl(0x01), Perm::EXECUTE);
        assert_eq!(rsync_perms_to_exacl(0x02), Perm::WRITE);
        assert_eq!(rsync_perms_to_exacl(0x04), Perm::READ);
        assert_eq!(
            rsync_perms_to_exacl(0x07),
            Perm::READ | Perm::WRITE | Perm::EXECUTE
        );
        assert_eq!(rsync_perms_to_exacl(0x05), Perm::READ | Perm::EXECUTE);
    }

    #[test]
    fn rsync_acl_to_entries_empty_acl() {
        let acl = RsyncAcl::new();
        let entries = rsync_acl_to_entries(&acl);
        assert!(entries.is_empty());
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn rsync_acl_to_entries_base_entries() {
        let acl = RsyncAcl::from_mode(0o754);
        let entries = rsync_acl_to_entries(&acl);

        // user_obj(rwx) + group_obj(r-x) + other_obj(r--)
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].kind, AclEntryKind::User);
        assert_eq!(entries[0].name, "");
        assert_eq!(entries[0].perms, Perm::READ | Perm::WRITE | Perm::EXECUTE);
        assert_eq!(entries[1].kind, AclEntryKind::Group);
        assert_eq!(entries[1].name, "");
        assert_eq!(entries[1].perms, Perm::READ | Perm::EXECUTE);
        assert_eq!(entries[2].kind, AclEntryKind::Other);
        assert_eq!(entries[2].name, "");
        assert_eq!(entries[2].perms, Perm::READ);
    }

    #[test]
    fn rsync_acl_to_entries_named_user_and_group() {
        use protocol::acl::IdAccess;

        let mut acl = RsyncAcl::from_mode(0o755);
        acl.names.push(IdAccess::user(1000, 0x07));
        acl.names.push(IdAccess::group(100, 0x05));

        let entries = rsync_acl_to_entries(&acl);

        // Find named entries (skip base entries on Linux/FreeBSD)
        let named: Vec<_> = entries.iter().filter(|e| !e.name.is_empty()).collect();
        assert_eq!(named.len(), 2);
        assert_eq!(named[0].kind, AclEntryKind::User);
        assert_eq!(named[0].name, "1000");
        assert_eq!(named[0].perms, Perm::READ | Perm::WRITE | Perm::EXECUTE);
        assert_eq!(named[1].kind, AclEntryKind::Group);
        assert_eq!(named[1].name, "100");
        assert_eq!(named[1].perms, Perm::READ | Perm::EXECUTE);
    }

    #[test]
    fn apply_acls_from_cache_skips_symlinks() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("test");
        File::create(&file).expect("create file");

        let cache = AclCache::new();
        let result = apply_acls_from_cache(&file, &cache, 0, None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn apply_acls_from_cache_applies_basic_acl() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("test");
        File::create(&file).expect("create file");

        let mut cache = AclCache::new();
        let acl = RsyncAcl::from_mode(0o644);
        let ndx = cache.store_access(acl);

        let result = apply_acls_from_cache(&file, &cache, ndx, None, true);
        assert!(result.is_ok());
    }

    #[test]
    fn apply_acls_from_cache_empty_acl_resets() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("test");
        File::create(&file).expect("create file");

        let mut cache = AclCache::new();
        let acl = RsyncAcl::new();
        let ndx = cache.store_access(acl);

        let result = apply_acls_from_cache(&file, &cache, ndx, None, true);
        assert!(result.is_ok());
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn apply_acls_from_cache_directory_with_default() {
        let dir = tempdir().expect("tempdir");
        let subdir = dir.path().join("subdir");
        std::fs::create_dir(&subdir).expect("create subdir");

        let mut cache = AclCache::new();
        let access = RsyncAcl::from_mode(0o755);
        let default = RsyncAcl::from_mode(0o755);
        let access_ndx = cache.store_access(access);
        let default_ndx = cache.store_default(default);

        let result = apply_acls_from_cache(&subdir, &cache, access_ndx, Some(default_ndx), true);
        assert!(result.is_ok());
    }

    #[test]
    fn apply_acls_from_cache_missing_index_is_noop() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("test");
        File::create(&file).expect("create file");

        let cache = AclCache::new();
        // Index 99 does not exist - should be a no-op, not an error
        let result = apply_acls_from_cache(&file, &cache, 99, None, true);
        assert!(result.is_ok());
    }
}
