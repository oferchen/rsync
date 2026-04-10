//! Permission preservation and chmod operations.
//!
//! Handles permission bits (full mode on Unix, read-only flag on Windows),
//! chmod modifier application, executability-only preservation, and both
//! path-based and fd-based permission syscalls.

use crate::error::MetadataError;
use crate::options::MetadataOptions;
use std::fs;
use std::path::Path;

#[cfg(unix)]
use rustix::fs as unix_fs;
#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::os::fd::BorrowedFd;

/// Sets permissions on `destination` to match `metadata` (full mode on Unix,
/// read-only flag on Windows).
///
/// On Unix, copies the full mode bits (including suid/sgid/sticky). On
/// Windows, only the read-only flag is mirrored.
// upstream: rsync.c:set_file_attrs() - chmod path for direct permission copy
pub(super) fn set_permissions_like(
    metadata: &fs::Metadata,
    destination: &Path,
) -> Result<(), MetadataError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = metadata.permissions().mode();
        let permissions = PermissionsExt::from_mode(mode);
        fs::set_permissions(destination, permissions)
            .map_err(|error| MetadataError::new("preserve permissions", destination, error))?
    }

    #[cfg(not(unix))]
    {
        let readonly = metadata.permissions().readonly();
        let mut destination_permissions = fs::metadata(destination)
            .map_err(|error| {
                MetadataError::new("inspect destination permissions", destination, error)
            })?
            .permissions();
        destination_permissions.set_readonly(readonly);
        fs::set_permissions(destination, destination_permissions)
            .map_err(|error| MetadataError::new("preserve permissions", destination, error))?
    }

    Ok(())
}

/// Returns `true` when `target_mode` already matches the permission bits on
/// `existing`, comparing only the lower 12 bits (suid/sgid/sticky + rwx).
// upstream: rsync.c:set_file_attrs() - skips chmod when mode already matches
#[cfg(unix)]
pub(super) fn permissions_match(target_mode: u32, existing: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    (existing.permissions().mode() & 0o7777) == (target_mode & 0o7777)
}

/// Applies permissions with optional chmod modifiers (path-based).
///
/// When chmod modifiers are configured, applies them on top of the base mode.
/// Otherwise delegates to [`apply_permissions_without_chmod`] for direct
/// permission copy or executability-only preservation.
// upstream: rsync.c:set_file_attrs() - chmod with optional modifier chain
pub(super) fn apply_permissions_with_chmod(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
    existing: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if let Some(modifiers) = options.chmod() {
            let mut mode = base_mode_for_permissions(destination, metadata, options, existing)?;
            mode = modifiers.apply(mode, metadata.file_type());

            if let Some(existing) = existing {
                if permissions_match(mode, existing) {
                    return Ok(());
                }
            }

            let permissions = PermissionsExt::from_mode(mode);
            fs::set_permissions(destination, permissions)
                .map_err(|error| MetadataError::new("preserve permissions", destination, error))?;
            return Ok(());
        }
    }

    if options.permissions() || options.executability() {
        apply_permissions_without_chmod(destination, metadata, options, existing)?;
    }

    Ok(())
}

/// fd-based variant of permission application.
///
/// Uses `fchmod` when an fd is available and we can determine the mode without
/// reading the current destination permissions. Falls back to path-based
/// operations for chmod modifiers that require a fresh stat, or when no fd
/// is provided.
#[cfg(unix)]
pub(super) fn apply_permissions_with_chmod_fd(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
    fd: Option<BorrowedFd<'_>>,
    existing: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    use std::os::unix::fs::PermissionsExt;

    if let Some(modifiers) = options.chmod() {
        let mut mode = base_mode_for_permissions(destination, metadata, options, existing)?;
        mode = modifiers.apply(mode, metadata.file_type());

        if let Some(existing) = existing {
            if permissions_match(mode, existing) {
                return Ok(());
            }
        }

        if let Some(fd) = fd {
            unix_fs::fchmod(
                fd,
                unix_fs::Mode::from_raw_mode(mode as rustix::fs::RawMode),
            )
            .map_err(|error| {
                MetadataError::new("preserve permissions", destination, io::Error::from(error))
            })?;
        } else {
            let permissions = PermissionsExt::from_mode(mode);
            fs::set_permissions(destination, permissions)
                .map_err(|error| MetadataError::new("preserve permissions", destination, error))?;
        }
        return Ok(());
    }

    if options.permissions() {
        let mode = metadata.permissions().mode();

        if let Some(existing) = existing {
            if permissions_match(mode, existing) {
                return Ok(());
            }
        }

        if let Some(fd) = fd {
            unix_fs::fchmod(
                fd,
                unix_fs::Mode::from_raw_mode(mode as rustix::fs::RawMode),
            )
            .map_err(|error| {
                MetadataError::new("preserve permissions", destination, io::Error::from(error))
            })?;
        } else {
            set_permissions_like(metadata, destination)?;
        }
        return Ok(());
    }

    if options.executability() && metadata.is_file() {
        apply_permissions_without_chmod(destination, metadata, options, existing)?;
    }

    Ok(())
}

/// Determines the base mode before chmod modifiers are applied.
///
/// When `--perms` is active, returns the source mode directly. Otherwise
/// reads the destination's current mode and optionally merges executability
/// bits from the source.
#[cfg(unix)]
fn base_mode_for_permissions(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
    existing: Option<&fs::Metadata>,
) -> Result<u32, MetadataError> {
    use std::os::unix::fs::PermissionsExt;

    if options.permissions() {
        return Ok(metadata.permissions().mode());
    }

    let mut destination_permissions = if let Some(existing) = existing {
        existing.permissions().mode()
    } else {
        fs::metadata(destination)
            .map_err(|error| {
                MetadataError::new("inspect destination permissions", destination, error)
            })?
            .permissions()
            .mode()
    };

    if options.executability() && metadata.is_file() {
        let source_exec = metadata.permissions().mode() & 0o111;
        if source_exec == 0 {
            destination_permissions &= !0o111;
        } else {
            destination_permissions |= 0o111;
        }
    }

    Ok(destination_permissions)
}

/// Applies permissions without chmod modifiers (direct copy or executability only).
fn apply_permissions_without_chmod(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
    existing: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    let _ = &existing; // used only on unix
    if options.permissions() {
        #[cfg(unix)]
        if let Some(existing) = existing {
            use std::os::unix::fs::PermissionsExt;
            if permissions_match(metadata.permissions().mode(), existing) {
                return Ok(());
            }
        }
        set_permissions_like(metadata, destination)?;
        return Ok(());
    }

    #[cfg(unix)]
    {
        if options.executability() && metadata.is_file() {
            use std::os::unix::fs::PermissionsExt;

            let mut destination_permissions = if let Some(existing) = existing {
                existing.permissions().mode()
            } else {
                fs::metadata(destination)
                    .map_err(|error| {
                        MetadataError::new("inspect destination permissions", destination, error)
                    })?
                    .permissions()
                    .mode()
            };

            let source_exec = metadata.permissions().mode() & 0o111;
            if source_exec == 0 {
                destination_permissions &= !0o111;
            } else {
                destination_permissions |= 0o111;
            }

            if let Some(existing) = existing {
                if permissions_match(destination_permissions, existing) {
                    return Ok(());
                }
            }

            let permissions = PermissionsExt::from_mode(destination_permissions);
            fs::set_permissions(destination, permissions)
                .map_err(|error| MetadataError::new("preserve permissions", destination, error))?;
        }
    }

    Ok(())
}

/// Applies permissions from a protocol `FileEntry`.
///
/// Handles the receiver-side chmod path: applies the entry's permission bits
/// directly, then layers any `--chmod` modifiers on top. Skips the syscall
/// when the resulting mode already matches `cached_meta`.
// upstream: rsync.c:set_file_attrs() - receiver-side permission application
pub(super) fn apply_permissions_from_entry(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
    cached_meta: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if !options.permissions() && !options.executability() && options.chmod().is_none() {
            return Ok(());
        }

        if options.permissions() {
            let mode = entry.permissions();
            // upstream: rsync.c:set_file_attrs() - skips chmod when mode already matches
            let needs_chmod = match cached_meta {
                Some(meta) => (meta.permissions().mode() & 0o7777) != (mode & 0o7777),
                None => true,
            };

            if needs_chmod {
                let permissions = PermissionsExt::from_mode(mode);
                fs::set_permissions(destination, permissions).map_err(|error| {
                    MetadataError::new("preserve permissions", destination, error)
                })?;
            }
        }

        if let Some(chmod) = options.chmod() {
            // upstream: rsync.c:set_file_attrs() - read current mode before applying chmod modifiers
            let fresh_meta;
            let current_meta = if options.permissions() {
                fresh_meta = fs::metadata(destination)
                    .map_err(|error| MetadataError::new("read permissions", destination, error))?;
                &fresh_meta
            } else if let Some(meta) = cached_meta {
                meta
            } else {
                fresh_meta = fs::metadata(destination)
                    .map_err(|error| MetadataError::new("read permissions", destination, error))?;
                &fresh_meta
            };
            let current_mode = current_meta.permissions().mode();

            let new_mode = chmod.apply(current_mode, current_meta.file_type());
            if new_mode != current_mode {
                let new_permissions = PermissionsExt::from_mode(new_mode);
                fs::set_permissions(destination, new_permissions)
                    .map_err(|error| MetadataError::new("apply chmod", destination, error))?;
            }
        }
    }

    #[cfg(not(unix))]
    {
        if options.permissions() {
            let readonly = entry.permissions() & 0o200 == 0;
            let dest_perms_meta = if let Some(meta) = cached_meta {
                meta.permissions()
            } else {
                fs::metadata(destination)
                    .map_err(|error| {
                        MetadataError::new("read destination permissions", destination, error)
                    })?
                    .permissions()
            };
            let mut dest_perms = dest_perms_meta;
            if dest_perms.readonly() != readonly {
                dest_perms.set_readonly(readonly);
                fs::set_permissions(destination, dest_perms).map_err(|error| {
                    MetadataError::new("preserve permissions", destination, error)
                })?;
            }
        }
    }

    Ok(())
}
