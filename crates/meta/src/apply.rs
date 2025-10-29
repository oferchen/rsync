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
    apply_file_metadata_with_options(destination, metadata, MetadataOptions::default())
}

/// Applies file metadata using explicit [`MetadataOptions`].
pub fn apply_file_metadata_with_options(
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

/// Applies metadata from `metadata` to the destination symbolic link without
/// following the link target.
pub fn apply_symlink_metadata(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    apply_symlink_metadata_with_options(destination, metadata, MetadataOptions::default())
}

/// Applies symbolic link metadata using explicit [`MetadataOptions`].
pub fn apply_symlink_metadata_with_options(
    destination: &Path,
    metadata: &fs::Metadata,
    options: MetadataOptions,
) -> Result<(), MetadataError> {
    set_owner_like(metadata, destination, false, &options)?;
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
            map_uid(metadata.uid() as RawUid, options.numeric_ids_enabled())
        } else {
            None
        };
        let group = if let Some(gid) = options.group_override() {
            Some(ownership::gid_from_raw(gid as RawGid))
        } else if options.group() {
            map_gid(metadata.gid() as RawGid, options.numeric_ids_enabled())
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
            let mut mode = if options.permissions() {
                metadata.permissions().mode()
            } else {
                fs::metadata(destination)
                    .map_err(|error| {
                        MetadataError::new("inspect destination permissions", destination, error)
                    })?
                    .permissions()
                    .mode()
            };

            mode = modifiers.apply(mode, metadata.file_type());
            let permissions = PermissionsExt::from_mode(mode);
            fs::set_permissions(destination, permissions)
                .map_err(|error| MetadataError::new("preserve permissions", destination, error))?;
            return Ok(());
        }
    }

    if options.permissions() {
        set_permissions_like(metadata, destination)?;
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
            MetadataOptions::new()
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
            MetadataOptions::new().preserve_permissions(false),
        )
        .expect("apply metadata");

        let mode = current_mode(&dest) & 0o777;
        assert_ne!(mode, 0o750);
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
            MetadataOptions::new().preserve_times(false),
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
}
