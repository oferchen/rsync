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

    // Pre-check existence - on macOS, ENOENT from getfacl would be masked by
    // is_unsupported_error(), which treats it as "no ACL support".
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
            // upstream: acls.c:get_rsync_acl() - no_acl_syscall_error() falls back
            // to the mode bits, which an empty ACL plus reset_acl_from_mode() mirrors.
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
        if let Err(e) = setfacl(&[destination], &source_acl, None)
            && !is_unsupported_error(&e)
        {
            return Err(MetadataError::new(
                "apply ACL",
                destination,
                io::Error::other(e.to_string()),
            ));
        }
    } else {
        reset_acl_from_mode(destination)?;
    }

    // upstream: acls.c:change_sacl_perms() + rsync.c:806-820 - applying the access ACL
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
                if let Err(e) = setfacl(&[destination], &acl, Some(AclOption::DEFAULT_ACL))
                    && !is_unsupported_error(&e)
                {
                    return Err(MetadataError::new(
                        "apply default ACL",
                        destination,
                        io::Error::other(e.to_string()),
                    ));
                }
            }
            _ => {
                clear_default_acl(destination)?;
            }
        }
    }

    Ok(())
}

/// Synchronizes ACLs from `source` to `destination` via `--fake-super` xattrs
/// instead of applying them with a real `setfacl`.
///
/// Mirrors [`sync_acls`], but the destination write stashes the ACL in
/// `%aacl`/`%dacl` rather than calling `sys_acl_set_file`/`setfacl` -
/// matching how the network receive path stashes ACLs under fake-super
/// (`store_acls_via_fake_super`). When the sender half also runs under
/// `--fake-super` (`sender_fake_super`), the source ACL is its `%aacl`/`%dacl`
/// stash alone ([`crate::read_sender_acl`]): a source without one sends the
/// empty ACL, and the destination then gets no xattr. A local copy's
/// `-M--fake-super` reaches the receiver half only (upstream: pipe.c:143-148),
/// so the sender then reads the source's real ACL.
///
/// Symbolic links do not support ACLs; when `follow_symlinks` is `false`,
/// this function returns immediately without performing any work.
///
/// # Errors
///
/// Returns [`MetadataError`] when reading the source's ACL or writing the
/// destination's fake-super xattr fails.
///
/// # Upstream Reference
///
/// Mirrors the `am_root < 0` branches of `get_rsync_acl()` (`acls.c:599-635`)
/// and `set_rsync_acl()` (`acls.c:1176-1182` default delete, `acls.c:1228-1251`
/// store).
#[allow(clippy::module_name_repetitions)]
pub fn sync_acls_via_fake_super(
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
    sender_fake_super: bool,
) -> Result<(), MetadataError> {
    use std::os::unix::fs::PermissionsExt;

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

    let source_mode = fs::metadata(source)
        .map_err(|e| MetadataError::new("stat", source, e))?
        .permissions()
        .mode();

    let access_acl =
        crate::fake_super::read_sender_acl(source, source_mode, false, sender_fake_super)
            .map_err(|e| MetadataError::new("read fake-super access ACL", source, e))?;

    crate::fake_super::update_fake_super_access_acl(destination, &access_acl, source_mode)
        .map_err(|e| MetadataError::new("store fake-super access ACL", destination, e))?;

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        let is_dir = fs::symlink_metadata(source)
            .map_err(|e| MetadataError::new("stat", source, e))?
            .is_dir();

        if is_dir {
            let default_acl =
                crate::fake_super::read_sender_acl(source, source_mode, true, sender_fake_super)
                    .map_err(|e| MetadataError::new("read fake-super default ACL", source, e))?;

            crate::fake_super::update_fake_super_default_acl(
                destination,
                &default_acl,
                source_mode,
            )
            .map_err(|e| MetadataError::new("store fake-super default ACL", destination, e))?;
        }
    }

    Ok(())
}
