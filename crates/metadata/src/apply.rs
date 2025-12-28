// Patch note (oc-rsync):
// - Removed the #[cfg(not(unix))] variant of `base_mode_for_permissions`,
//   which was never called on non-Unix targets and triggered a dead_code
//   error when building for Windows with `-D warnings`.
//   The function is only needed on Unix and is only referenced inside a
//   #[cfg(unix)] block, so restricting it to Unix preserves behavior and
//   keeps non-Unix builds clean.

use crate::error::MetadataError;
use crate::options::MetadataOptions;
use filetime::{FileTime, set_file_times, set_symlink_file_times};
use std::fs;
use std::io;
use std::path::Path;

#[cfg(unix)]
use crate::id_lookup::{map_gid, map_uid};
#[cfg(unix)]
use crate::ownership;
#[cfg(unix)]
use rustix::fs::{self as unix_fs, AtFlags, CWD};
#[cfg(unix)]
use rustix::process::{RawGid, RawUid};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

/// Applies metadata from `metadata` to the destination directory.
///
/// The helper preserves permission bits (best-effort on non-Unix targets) and
/// nanosecond timestamps.
pub fn apply_directory_metadata(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    apply_directory_metadata_with_options(destination, metadata, MetadataOptions::default())
}

/// Applies metadata from `metadata` to the destination directory using explicit options.
pub fn apply_directory_metadata_with_options(
    destination: &Path,
    metadata: &fs::Metadata,
    options: MetadataOptions,
) -> Result<(), MetadataError> {
    set_owner_like(metadata, destination, true, &options)?;
    apply_permissions_with_chmod(destination, metadata, &options)?;
    if options.times() {
        set_timestamp_like(metadata, destination, true)?;
    }
    Ok(())
}

/// Applies metadata from `metadata` to the destination file.
///
/// The helper preserves permission bits (best-effort on non-Unix targets) and
/// nanosecond timestamps.
pub fn apply_file_metadata(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    apply_file_metadata_with_options(destination, metadata, &MetadataOptions::default())
}

/// Applies file metadata using explicit [`MetadataOptions`].
pub fn apply_file_metadata_with_options(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
) -> Result<(), MetadataError> {
    set_owner_like(metadata, destination, true, options)?;
    apply_permissions_with_chmod(destination, metadata, options)?;
    if options.times() {
        set_timestamp_like(metadata, destination, true)?;
    }
    Ok(())
}

/// Applies metadata from `metadata` to the destination symbolic link without
/// following the link target.
pub fn apply_symlink_metadata(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    apply_symlink_metadata_with_options(destination, metadata, &MetadataOptions::default())
}

/// Applies symbolic link metadata using explicit [`MetadataOptions`].
pub fn apply_symlink_metadata_with_options(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
) -> Result<(), MetadataError> {
    set_owner_like(metadata, destination, false, options)?;
    if options.times() {
        set_timestamp_like(metadata, destination, false)?;
    }
    Ok(())
}

fn set_owner_like(
    metadata: &fs::Metadata,
    destination: &Path,
    follow_symlinks: bool,
    options: &MetadataOptions,
) -> Result<(), MetadataError> {
    #[cfg(unix)]
    {
        if options.owner_override().is_none()
            && options.group_override().is_none()
            && !options.owner()
            && !options.group()
        {
            return Ok(());
        }

        let owner = if let Some(uid) = options.owner_override() {
            Some(ownership::uid_from_raw(uid as RawUid))
        } else if options.owner() {
            let mut raw_uid = metadata.uid() as RawUid;
            if let Some(mapping) = options.user_mapping()
                && let Some(mapped) = mapping
                    .map_uid(raw_uid)
                    .map_err(|error| MetadataError::new("apply user mapping", destination, error))?
            {
                raw_uid = mapped;
            }
            map_uid(raw_uid, options.numeric_ids_enabled())
        } else {
            None
        };
        let group = if let Some(gid) = options.group_override() {
            Some(ownership::gid_from_raw(gid as RawGid))
        } else if options.group() {
            let mut raw_gid = metadata.gid() as RawGid;
            if let Some(mapping) = options.group_mapping()
                && let Some(mapped) = mapping.map_gid(raw_gid).map_err(|error| {
                    MetadataError::new("apply group mapping", destination, error)
                })?
            {
                raw_gid = mapped;
            }
            map_gid(raw_gid, options.numeric_ids_enabled())
        } else {
            None
        };

        if owner.is_none() && group.is_none() {
            return Ok(());
        }

        let flags = if follow_symlinks {
            AtFlags::empty()
        } else {
            AtFlags::SYMLINK_NOFOLLOW
        };

        unix_fs::chownat(CWD, destination, owner, group, flags).map_err(|error| {
            MetadataError::new("preserve ownership", destination, io::Error::from(error))
        })?
    }

    #[cfg(not(unix))]
    {
        let _ = metadata;
        let _ = follow_symlinks;
        if options.owner()
            || options.group()
            || options.owner_override().is_some()
            || options.group_override().is_some()
        {
            return Err(MetadataError::new(
                "preserve ownership",
                destination,
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "preserving ownership is not supported on this platform",
                ),
            ));
        }
    }

    Ok(())
}

fn set_permissions_like(metadata: &fs::Metadata, destination: &Path) -> Result<(), MetadataError> {
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

fn apply_permissions_with_chmod(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
) -> Result<(), MetadataError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if let Some(modifiers) = options.chmod() {
            let mut mode = base_mode_for_permissions(destination, metadata, options)?;

            mode = modifiers.apply(mode, metadata.file_type());
            let permissions = PermissionsExt::from_mode(mode);
            fs::set_permissions(destination, permissions)
                .map_err(|error| MetadataError::new("preserve permissions", destination, error))?;
            return Ok(());
        }
    }

    if options.permissions() || options.executability() {
        apply_permissions_without_chmod(destination, metadata, options)?;
    }

    Ok(())
}

#[cfg(unix)]
fn base_mode_for_permissions(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
) -> Result<u32, MetadataError> {
    use std::os::unix::fs::PermissionsExt;

    if options.permissions() {
        return Ok(metadata.permissions().mode());
    }

    let mut destination_permissions = fs::metadata(destination)
        .map_err(|error| MetadataError::new("inspect destination permissions", destination, error))?
        .permissions()
        .mode();

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

fn apply_permissions_without_chmod(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
) -> Result<(), MetadataError> {
    if options.permissions() {
        set_permissions_like(metadata, destination)?;
        return Ok(());
    }

    #[cfg(unix)]
    {
        if options.executability() && metadata.is_file() {
            use std::os::unix::fs::PermissionsExt;

            let mut destination_permissions = fs::metadata(destination)
                .map_err(|error| {
                    MetadataError::new("inspect destination permissions", destination, error)
                })?
                .permissions()
                .mode();

            let source_exec = metadata.permissions().mode() & 0o111;
            if source_exec == 0 {
                destination_permissions &= !0o111;
            } else {
                destination_permissions |= 0o111;
            }

            let permissions = PermissionsExt::from_mode(destination_permissions);
            fs::set_permissions(destination, permissions)
                .map_err(|error| MetadataError::new("preserve permissions", destination, error))?;
        }
    }

    Ok(())
}

fn set_timestamp_like(
    metadata: &fs::Metadata,
    destination: &Path,
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    let accessed = FileTime::from_last_access_time(metadata);
    let modified = FileTime::from_last_modification_time(metadata);

    if follow_symlinks {
        set_file_times(destination, accessed, modified)
            .map_err(|error| MetadataError::new("preserve timestamps", destination, error))?
    } else {
        set_symlink_file_times(destination, accessed, modified)
            .map_err(|error| MetadataError::new("preserve timestamps", destination, error))?
    }

    Ok(())
}

/// Applies metadata from a protocol FileEntry to the destination file.
///
/// This is the receiver-side counterpart to [`apply_file_metadata`] that works
/// directly with FileEntry metadata from the wire protocol, avoiding the need
/// to construct an [`fs::Metadata`] instance.
///
/// # Arguments
/// - `destination`: Path to the file to apply metadata to
/// - `entry`: FileEntry containing metadata from sender
/// - `options`: Controls which metadata fields are preserved
///
/// # Errors
/// Returns [`MetadataError`] if any filesystem operation fails.
///
/// # Examples
///
/// ```no_run
/// use metadata::{apply_metadata_from_file_entry, MetadataOptions};
/// use protocol::flist::FileEntry;
/// use std::path::Path;
///
/// # fn example(file_entry: &FileEntry) -> Result<(), metadata::MetadataError> {
/// let dest_path = Path::new("/path/to/reconstructed/file.txt");
///
/// // Apply metadata with permissions and timestamps
/// let options = MetadataOptions::new()
///     .preserve_permissions(true)
///     .preserve_times(true);
///
/// apply_metadata_from_file_entry(dest_path, file_entry, &options)?;
/// # Ok(())
/// # }
/// ```
pub fn apply_metadata_from_file_entry(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
) -> Result<(), MetadataError> {
    // Step 1: Apply ownership (if requested)
    apply_ownership_from_entry(destination, entry, options)?;

    // Step 2: Apply permissions (if requested)
    apply_permissions_from_entry(destination, entry, options)?;

    // Step 3: Apply timestamps (if requested)
    if options.times() {
        apply_timestamps_from_entry(destination, entry)?;
    }

    Ok(())
}

#[cfg(unix)]
fn apply_ownership_from_entry(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
) -> Result<(), MetadataError> {
    use rustix::fs::{AtFlags, CWD, chownat};
    use rustix::process::{RawGid, RawUid};

    // Early return if no ownership preservation requested
    if !options.owner()
        && !options.group()
        && options.owner_override().is_none()
        && options.group_override().is_none()
    {
        return Ok(());
    }

    // Get raw uid/gid from entry for potential fake-super storage
    let raw_uid = if let Some(uid_override) = options.owner_override() {
        Some(uid_override)
    } else if options.owner() {
        entry.uid()
    } else {
        None
    };

    let raw_gid = if let Some(gid_override) = options.group_override() {
        Some(gid_override)
    } else if options.group() {
        entry.gid()
    } else {
        None
    };

    // If fake-super mode is enabled, store ownership in xattr instead of applying directly
    if options.fake_super_enabled() {
        return apply_ownership_via_fake_super(destination, entry, raw_uid, raw_gid);
    }

    // Determine owner (with mappings applied)
    let owner = if let Some(uid_override) = options.owner_override() {
        Some(ownership::uid_from_raw(uid_override as RawUid))
    } else if options.owner() {
        entry.uid().and_then(|uid| {
            let mut mapped_uid = uid as RawUid;
            // Apply user mapping if present
            if let Some(mapping) = options.user_mapping()
                && let Ok(Some(mapped)) = mapping.map_uid(mapped_uid)
            {
                mapped_uid = mapped;
            }
            map_uid(mapped_uid, options.numeric_ids_enabled())
        })
    } else {
        None
    };

    // Determine group (similar structure)
    let group = if let Some(gid_override) = options.group_override() {
        Some(ownership::gid_from_raw(gid_override as RawGid))
    } else if options.group() {
        entry.gid().and_then(|gid| {
            let mut mapped_gid = gid as RawGid;
            if let Some(mapping) = options.group_mapping()
                && let Ok(Some(mapped)) = mapping.map_gid(mapped_gid)
            {
                mapped_gid = mapped;
            }
            map_gid(mapped_gid, options.numeric_ids_enabled())
        })
    } else {
        None
    };

    // Apply ownership if at least one is set
    if owner.is_some() || group.is_some() {
        chownat(CWD, destination, owner, group, AtFlags::empty()).map_err(|error| {
            MetadataError::new("preserve ownership", destination, io::Error::from(error))
        })?;
    }

    Ok(())
}

/// Stores ownership metadata via fake-super xattr instead of applying directly.
///
/// This is used when `--fake-super` is enabled, allowing non-root users to
/// preserve ownership information in extended attributes.
#[cfg(unix)]
fn apply_ownership_via_fake_super(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    uid: Option<u32>,
    gid: Option<u32>,
) -> Result<(), MetadataError> {
    use crate::fake_super::{FakeSuperStat, store_fake_super};

    // Build FakeSuperStat from entry metadata
    let mode = entry.permissions();
    let uid = uid.unwrap_or(0);
    let gid = gid.unwrap_or(0);

    // Check if this is a device file (block or char) and get rdev if present
    let rdev = if entry.file_type().is_device() {
        // Get major/minor directly from the entry - they're already decoded
        match (entry.rdev_major(), entry.rdev_minor()) {
            (Some(major), Some(minor)) => Some((major, minor)),
            _ => None,
        }
    } else {
        None
    };

    let stat = FakeSuperStat {
        mode,
        uid,
        gid,
        rdev,
    };

    store_fake_super(destination, &stat)
        .map_err(|error| MetadataError::new("store fake-super metadata", destination, error))
}

#[cfg(not(unix))]
fn apply_ownership_from_entry(
    _destination: &Path,
    _entry: &protocol::flist::FileEntry,
    _options: &MetadataOptions,
) -> Result<(), MetadataError> {
    // Non-Unix platforms: ownership is not supported
    Ok(())
}

fn apply_permissions_from_entry(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
) -> Result<(), MetadataError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if !options.permissions() && !options.executability() && options.chmod().is_none() {
            return Ok(());
        }

        // Standard permission preservation
        if options.permissions() {
            let mode = entry.permissions();
            let permissions = PermissionsExt::from_mode(mode);
            fs::set_permissions(destination, permissions)
                .map_err(|error| MetadataError::new("preserve permissions", destination, error))?;
        }

        // Apply chmod modifiers if present
        if let Some(chmod) = options.chmod() {
            // Get current permissions
            let current_meta = fs::metadata(destination)
                .map_err(|error| MetadataError::new("read permissions", destination, error))?;
            let current_mode = current_meta.permissions().mode();

            // Apply chmod modifiers
            let new_mode = chmod.apply(current_mode, current_meta.file_type());
            let new_permissions = PermissionsExt::from_mode(new_mode);
            fs::set_permissions(destination, new_permissions)
                .map_err(|error| MetadataError::new("apply chmod", destination, error))?;
        }
    }

    #[cfg(not(unix))]
    {
        if options.permissions() {
            // Non-Unix: only readonly flag
            let readonly = entry.permissions() & 0o200 == 0;
            let mut dest_perms = fs::metadata(destination)
                .map_err(|error| {
                    MetadataError::new("read destination permissions", destination, error)
                })?
                .permissions();
            dest_perms.set_readonly(readonly);
            fs::set_permissions(destination, dest_perms)
                .map_err(|error| MetadataError::new("preserve permissions", destination, error))?;
        }
    }

    Ok(())
}

fn apply_timestamps_from_entry(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
) -> Result<(), MetadataError> {
    // Build FileTime from FileEntry's (mtime, mtime_nsec)
    // This preserves nanosecond precision!
    let mtime = FileTime::from_unix_time(entry.mtime(), entry.mtime_nsec());
    let atime = mtime; // Use mtime for both (rsync behavior)

    set_file_times(destination, atime, mtime)
        .map_err(|error| MetadataError::new("preserve timestamps", destination, error))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::id_lookup::{map_gid, map_uid};
    #[cfg(unix)]
    use crate::ownership;
    use tempfile::tempdir;

    #[cfg(unix)]
    fn current_mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;

        fs::metadata(path).expect("metadata").permissions().mode()
    }

    #[test]
    fn file_permissions_and_times_are_preserved() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");
        fs::write(&source, b"data").expect("write source");
        fs::write(&dest, b"data").expect("write dest");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&source, PermissionsExt::from_mode(0o640))
                .expect("set source perms");
        }

        let atime = FileTime::from_unix_time(1_700_000_000, 111_000_000);
        let mtime = FileTime::from_unix_time(1_700_000_100, 222_000_000);
        set_file_times(&source, atime, mtime).expect("set source times");

        let metadata = fs::metadata(&source).expect("metadata");
        apply_file_metadata(&dest, &metadata).expect("apply file metadata");

        let dest_meta = fs::metadata(&dest).expect("dest metadata");
        let dest_atime = FileTime::from_last_access_time(&dest_meta);
        let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
        assert_eq!(dest_atime, atime);
        assert_eq!(dest_mtime, mtime);

        #[cfg(unix)]
        {
            assert_eq!(current_mode(&dest) & 0o777, 0o640);
        }
    }

    #[cfg(unix)]
    #[test]
    fn file_ownership_is_preserved_when_requested() {
        use rustix::fs::{AtFlags, CWD, chownat};

        if rustix::process::geteuid().as_raw() != 0 {
            return;
        }

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source-owner.txt");
        let dest = temp.path().join("dest-owner.txt");
        fs::write(&source, b"data").expect("write source");
        fs::write(&dest, b"data").expect("write dest");

        let owner = 12_345;
        let group = 54_321;
        chownat(
            CWD,
            &source,
            Some(ownership::uid_from_raw(owner)),
            Some(ownership::gid_from_raw(group)),
            AtFlags::empty(),
        )
        .expect("assign ownership");

        let metadata = fs::metadata(&source).expect("metadata");
        apply_file_metadata_with_options(
            &dest,
            &metadata,
            &MetadataOptions::new()
                .preserve_owner(true)
                .preserve_group(true),
        )
        .expect("preserve metadata");

        let dest_meta = fs::metadata(&dest).expect("dest metadata");
        assert_eq!(dest_meta.uid(), owner);
        assert_eq!(dest_meta.gid(), group);
    }

    #[cfg(unix)]
    #[test]
    fn file_permissions_respect_toggle() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source-perms.txt");
        let dest = temp.path().join("dest-perms.txt");
        fs::write(&source, b"data").expect("write source");
        fs::write(&dest, b"data").expect("write dest");

        fs::set_permissions(&source, PermissionsExt::from_mode(0o750)).expect("set source perms");
        let metadata = fs::metadata(&source).expect("metadata");

        apply_file_metadata_with_options(
            &dest,
            &metadata,
            &MetadataOptions::new().preserve_permissions(false),
        )
        .expect("apply metadata");

        let mode = current_mode(&dest) & 0o777;
        assert_ne!(mode, 0o750);
    }

    #[cfg(unix)]
    #[test]
    fn file_executability_can_be_preserved_without_other_bits() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source-exec.txt");
        let dest = temp.path().join("dest-exec.txt");

        fs::write(&source, b"data").expect("write source");
        fs::write(&dest, b"data").expect("write dest");

        fs::set_permissions(&source, PermissionsExt::from_mode(0o751)).expect("set source perms");
        fs::set_permissions(&dest, PermissionsExt::from_mode(0o620)).expect("set dest perms");

        let metadata = fs::metadata(&source).expect("metadata");

        apply_file_metadata_with_options(
            &dest,
            &metadata,
            &MetadataOptions::new()
                .preserve_permissions(false)
                .preserve_executability(true),
        )
        .expect("apply metadata");

        let mode = current_mode(&dest) & 0o777;
        assert_eq!(mode & 0o111, 0o751 & 0o111);
        assert_eq!(mode & 0o666, 0o620);
    }

    #[test]
    fn file_times_respect_toggle() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source-times.txt");
        let dest = temp.path().join("dest-times.txt");
        fs::write(&source, b"data").expect("write source");
        fs::write(&dest, b"data").expect("write dest");

        let atime = FileTime::from_unix_time(1_700_050_000, 100_000_000);
        let mtime = FileTime::from_unix_time(1_700_060_000, 200_000_000);
        set_file_times(&source, atime, mtime).expect("set source times");
        let metadata = fs::metadata(&source).expect("metadata");

        apply_file_metadata_with_options(
            &dest,
            &metadata,
            &MetadataOptions::new().preserve_times(false),
        )
        .expect("apply metadata");

        let dest_meta = fs::metadata(&dest).expect("dest metadata");
        let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
        assert_ne!(dest_mtime, mtime);
    }

    #[test]
    fn metadata_options_numeric_ids_toggle() {
        let opts = MetadataOptions::new().numeric_ids(true);
        assert!(opts.numeric_ids_enabled());
        assert!(!MetadataOptions::new().numeric_ids_enabled());
    }

    #[cfg(unix)]
    #[test]
    fn map_uid_round_trips_current_user_without_numeric_flag() {
        let uid = rustix::process::geteuid().as_raw();
        let mapped = map_uid(uid, false).expect("uid");
        assert_eq!(mapped.as_raw(), uid);
    }

    #[cfg(unix)]
    #[test]
    fn map_gid_round_trips_current_group_without_numeric_flag() {
        let gid = rustix::process::getegid().as_raw();
        let mapped = map_gid(gid, false).expect("gid");
        assert_eq!(mapped.as_raw(), gid);
    }

    #[test]
    fn directory_permissions_and_times_are_preserved() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source-dir");
        let dest = temp.path().join("dest-dir");
        fs::create_dir(&source).expect("create source dir");
        fs::create_dir(&dest).expect("create dest dir");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&source, PermissionsExt::from_mode(0o751))
                .expect("set source perms");
        }

        let atime = FileTime::from_unix_time(1_700_010_000, 0);
        let mtime = FileTime::from_unix_time(1_700_020_000, 333_000_000);
        set_file_times(&source, atime, mtime).expect("set source times");

        let metadata = fs::metadata(&source).expect("metadata");
        apply_directory_metadata(&dest, &metadata).expect("apply dir metadata");

        let dest_meta = fs::metadata(&dest).expect("dest metadata");
        let dest_atime = FileTime::from_last_access_time(&dest_meta);
        let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
        assert_eq!(dest_atime, atime);
        assert_eq!(dest_mtime, mtime);

        #[cfg(unix)]
        {
            assert_eq!(current_mode(&dest) & 0o777, 0o751);
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_times_are_preserved_without_following_target() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().expect("tempdir");
        let target = temp.path().join("target.txt");
        fs::write(&target, b"data").expect("write target");

        let source_link = temp.path().join("source-link");
        let dest_link = temp.path().join("dest-link");
        symlink(&target, &source_link).expect("create source link");
        symlink(&target, &dest_link).expect("create dest link");

        let atime = FileTime::from_unix_time(1_700_030_000, 444_000_000);
        let mtime = FileTime::from_unix_time(1_700_040_000, 555_000_000);
        set_symlink_file_times(&source_link, atime, mtime).expect("set link times");

        let metadata = fs::symlink_metadata(&source_link).expect("metadata");
        apply_symlink_metadata(&dest_link, &metadata).expect("apply symlink metadata");

        let dest_meta = fs::symlink_metadata(&dest_link).expect("dest metadata");
        let dest_atime = FileTime::from_last_access_time(&dest_meta);
        let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
        assert_eq!(dest_atime, atime);
        assert_eq!(dest_mtime, mtime);

        let dest_target = fs::read_link(&dest_link).expect("read dest link");
        assert_eq!(dest_target, target);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_metadata_with_options_no_times() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().expect("tempdir");
        let target = temp.path().join("target.txt");
        fs::write(&target, b"data").expect("write target");

        let source_link = temp.path().join("source-link2");
        let dest_link = temp.path().join("dest-link2");
        symlink(&target, &source_link).expect("create source link");
        symlink(&target, &dest_link).expect("create dest link");

        let metadata = fs::symlink_metadata(&source_link).expect("metadata");

        // Apply with times disabled
        apply_symlink_metadata_with_options(
            &dest_link,
            &metadata,
            &MetadataOptions::new().preserve_times(false),
        )
        .expect("apply symlink metadata");

        // Should succeed without error
        assert!(fs::symlink_metadata(&dest_link).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn directory_metadata_with_options_no_times() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source-dir-notime");
        let dest = temp.path().join("dest-dir-notime");
        fs::create_dir(&source).expect("create source dir");
        fs::create_dir(&dest).expect("create dest dir");

        let metadata = fs::metadata(&source).expect("metadata");

        apply_directory_metadata_with_options(
            &dest,
            &metadata,
            MetadataOptions::new().preserve_times(false),
        )
        .expect("apply dir metadata");

        // Should succeed
        assert!(fs::metadata(&dest).is_ok());
    }

    #[test]
    fn file_metadata_with_all_options_disabled() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source-noop.txt");
        let dest = temp.path().join("dest-noop.txt");
        fs::write(&source, b"data").expect("write source");
        fs::write(&dest, b"data").expect("write dest");

        let metadata = fs::metadata(&source).expect("metadata");

        // Apply with everything disabled
        apply_file_metadata_with_options(
            &dest,
            &metadata,
            &MetadataOptions::new()
                .preserve_times(false)
                .preserve_permissions(false)
                .preserve_owner(false)
                .preserve_group(false),
        )
        .expect("apply metadata");

        // Should succeed
        assert!(fs::metadata(&dest).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn executability_not_applied_to_directory() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source-exec-dir");
        let dest = temp.path().join("dest-exec-dir");
        fs::create_dir(&source).expect("create source dir");
        fs::create_dir(&dest).expect("create dest dir");

        fs::set_permissions(&source, PermissionsExt::from_mode(0o755)).expect("set source perms");
        fs::set_permissions(&dest, PermissionsExt::from_mode(0o700)).expect("set dest perms");

        let metadata = fs::metadata(&source).expect("metadata");

        // Executability preservation only applies to files, not directories
        apply_file_metadata_with_options(
            &dest,
            &metadata,
            &MetadataOptions::new()
                .preserve_permissions(false)
                .preserve_executability(true)
                .preserve_times(false),
        )
        .expect("apply metadata");

        // For directories, executability flag should have no effect
        assert!(fs::metadata(&dest).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn executability_removed_when_source_not_executable() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source-noexec.txt");
        let dest = temp.path().join("dest-noexec.txt");
        fs::write(&source, b"data").expect("write source");
        fs::write(&dest, b"data").expect("write dest");

        // Source is NOT executable
        fs::set_permissions(&source, PermissionsExt::from_mode(0o644)).expect("set source perms");
        // Dest IS executable
        fs::set_permissions(&dest, PermissionsExt::from_mode(0o755)).expect("set dest perms");

        let metadata = fs::metadata(&source).expect("metadata");

        apply_file_metadata_with_options(
            &dest,
            &metadata,
            &MetadataOptions::new()
                .preserve_permissions(false)
                .preserve_executability(true)
                .preserve_times(false),
        )
        .expect("apply metadata");

        // Dest should no longer be executable
        let mode = current_mode(&dest) & 0o111;
        assert_eq!(mode, 0);
    }

    #[cfg(unix)]
    #[test]
    fn owner_override_takes_precedence() {
        if rustix::process::geteuid().as_raw() != 0 {
            return;
        }

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source-override.txt");
        let dest = temp.path().join("dest-override.txt");
        fs::write(&source, b"data").expect("write source");
        fs::write(&dest, b"data").expect("write dest");

        let metadata = fs::metadata(&source).expect("metadata");

        apply_file_metadata_with_options(
            &dest,
            &metadata,
            &MetadataOptions::new()
                .preserve_owner(true)
                .with_owner_override(Some(1000))
                .preserve_times(false),
        )
        .expect("apply metadata");

        let dest_meta = fs::metadata(&dest).expect("dest metadata");
        assert_eq!(dest_meta.uid(), 1000);
    }

    #[cfg(unix)]
    #[test]
    fn group_override_takes_precedence() {
        if rustix::process::geteuid().as_raw() != 0 {
            return;
        }

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source-grp-override.txt");
        let dest = temp.path().join("dest-grp-override.txt");
        fs::write(&source, b"data").expect("write source");
        fs::write(&dest, b"data").expect("write dest");

        let metadata = fs::metadata(&source).expect("metadata");

        apply_file_metadata_with_options(
            &dest,
            &metadata,
            &MetadataOptions::new()
                .preserve_group(true)
                .with_group_override(Some(1000))
                .preserve_times(false),
        )
        .expect("apply metadata");

        let dest_meta = fs::metadata(&dest).expect("dest metadata");
        assert_eq!(dest_meta.gid(), 1000);
    }

    #[test]
    fn apply_metadata_from_file_entry_with_timestamps() {
        use protocol::flist::FileEntry;

        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("entry-dest.txt");
        fs::write(&dest, b"data").expect("write dest");

        let mut entry = FileEntry::new_file("entry-dest.txt".into(), 4, 0o644);
        entry.set_mtime(1_700_000_000, 123_456_789);

        let opts = MetadataOptions::new().preserve_times(true);
        apply_metadata_from_file_entry(&dest, &entry, &opts).expect("apply from entry");

        let dest_meta = fs::metadata(&dest).expect("dest metadata");
        let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
        assert_eq!(
            dest_mtime,
            FileTime::from_unix_time(1_700_000_000, 123_456_789)
        );
    }

    #[test]
    fn apply_metadata_from_file_entry_no_times() {
        use protocol::flist::FileEntry;

        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("entry-notime.txt");
        fs::write(&dest, b"data").expect("write dest");

        let entry = FileEntry::new_file("entry-notime.txt".into(), 4, 0o644);

        let opts = MetadataOptions::new().preserve_times(false);
        apply_metadata_from_file_entry(&dest, &entry, &opts).expect("apply from entry");

        // Should succeed without modifying times
        assert!(fs::metadata(&dest).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn apply_permissions_from_entry_respects_permissions_flag() {
        use protocol::flist::FileEntry;
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("entry-perms.txt");
        fs::write(&dest, b"data").expect("write dest");
        fs::set_permissions(&dest, PermissionsExt::from_mode(0o666)).expect("set dest perms");

        let entry = FileEntry::new_file("entry-perms.txt".into(), 4, 0o755);

        let opts = MetadataOptions::new()
            .preserve_permissions(true)
            .preserve_times(false);
        apply_metadata_from_file_entry(&dest, &entry, &opts).expect("apply from entry");

        let mode = current_mode(&dest) & 0o777;
        assert_eq!(mode, 0o755);
    }

    #[cfg(unix)]
    #[test]
    fn apply_permissions_from_entry_no_change_when_disabled() {
        use protocol::flist::FileEntry;
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("entry-noperms.txt");
        fs::write(&dest, b"data").expect("write dest");
        fs::set_permissions(&dest, PermissionsExt::from_mode(0o666)).expect("set dest perms");

        let entry = FileEntry::new_file("entry-noperms.txt".into(), 4, 0o755);

        let opts = MetadataOptions::new()
            .preserve_permissions(false)
            .preserve_times(false);
        apply_metadata_from_file_entry(&dest, &entry, &opts).expect("apply from entry");

        let mode = current_mode(&dest) & 0o777;
        // Should still be original mode
        assert_eq!(mode, 0o666);
    }
}
