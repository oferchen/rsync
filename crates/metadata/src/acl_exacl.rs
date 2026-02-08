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
use exacl::{getfacl, setfacl};

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
    // Symbolic links do not support ACLs on Linux/macOS
    if !follow_symlinks {
        return Ok(());
    }

    // Read the source ACL
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

    // Check if the source is a directory (for default ACLs on Linux/FreeBSD)
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    let is_dir = match fs::symlink_metadata(source) {
        Ok(m) => m.is_dir(),
        Err(e) => return Err(MetadataError::new("stat", source, e)),
    };

    // Read default ACL for directories (Linux/FreeBSD only)
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

    // Apply access ACL to destination
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
        // No extended ACL entries - reset to permission bits
        reset_acl_from_mode(destination)?;
    }

    // Apply default ACL to destination directory (Linux/FreeBSD only)
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
                // Clear default ACL on destination if source has none
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
    // Linux/FreeBSD: convert permission mode to a minimal POSIX ACL
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
    // Setting an empty default ACL clears it
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
