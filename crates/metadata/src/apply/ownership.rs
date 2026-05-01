//! Ownership resolution and chown operations.
//!
//! Handles UID/GID resolution with overrides, mappings, and numeric-id rules,
//! plus path-based and fd-based chown application. Includes fake-super support
//! for non-root ownership preservation via extended attributes.

use crate::error::MetadataError;
use crate::options::MetadataOptions;
use std::fs;
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
use std::io;
#[cfg(unix)]
use std::os::fd::BorrowedFd;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

/// Resolves the target UID and GID after applying overrides, mappings, and
/// numeric-id rules. Returns `(None, None)` when no ownership change is
/// requested.
///
/// Resolution priority: override > mapping > source metadata, matching
/// upstream's chown logic.
// upstream: rsync.c:set_file_attrs() - UID/GID resolution before chown
#[cfg(unix)]
pub(super) fn resolve_ownership(
    metadata: &fs::Metadata,
    options: &MetadataOptions,
    destination: &Path,
) -> Result<(Option<unix_fs::Uid>, Option<unix_fs::Gid>), MetadataError> {
    if options.owner_override().is_none()
        && options.group_override().is_none()
        && !options.owner()
        && !options.group()
    {
        return Ok((None, None));
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
            && let Some(mapped) = mapping
                .map_gid(raw_gid)
                .map_err(|error| MetadataError::new("apply group mapping", destination, error))?
        {
            raw_gid = mapped;
        }
        map_gid(raw_gid, options.numeric_ids_enabled())
    } else {
        None
    };

    Ok((owner, group))
}

/// Returns `true` when the resolved ownership already matches `existing`.
///
/// Compares only the fields that are being changed - `None` values are
/// treated as "no change requested" and always match.
// upstream: rsync.c:set_file_attrs() - skips chown when uid/gid already match
#[cfg(unix)]
pub(super) fn ownership_matches(
    owner: &Option<unix_fs::Uid>,
    group: &Option<unix_fs::Gid>,
    existing: &fs::Metadata,
) -> bool {
    let uid_ok = match owner {
        Some(uid) => uid.as_raw() == existing.uid(),
        None => true,
    };
    let gid_ok = match group {
        Some(gid) => gid.as_raw() == existing.gid(),
        None => true,
    };
    uid_ok && gid_ok
}

/// Applies ownership (chown) to a path, optionally following symlinks.
///
/// Uses `chownat` with `AT_SYMLINK_NOFOLLOW` when `follow_symlinks` is false.
/// Skips the syscall entirely when both resolved UID and GID are `None`,
/// or when the resolved values already match `existing`.
// upstream: rsync.c:set_file_attrs() - chownat with conditional AT_SYMLINK_NOFOLLOW
pub(super) fn set_owner_like(
    metadata: &fs::Metadata,
    destination: &Path,
    follow_symlinks: bool,
    options: &MetadataOptions,
    existing: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    #[cfg(unix)]
    {
        let (owner, group) = resolve_ownership(metadata, options, destination)?;

        if owner.is_none() && group.is_none() {
            return Ok(());
        }

        if let Some(existing) = existing {
            if ownership_matches(&owner, &group, existing) {
                return Ok(());
            }
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
        let _ = destination;
        let _ = metadata;
        let _ = follow_symlinks;
        let _ = options;
        let _ = existing;
    }

    Ok(())
}

/// fd-based variant of [`set_owner_like`] that uses `fchown` instead of `chownat`.
#[cfg(unix)]
pub(super) fn set_owner_like_with_fd(
    metadata: &fs::Metadata,
    destination: &Path,
    options: &MetadataOptions,
    fd: BorrowedFd<'_>,
    existing: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    let (owner, group) = resolve_ownership(metadata, options, destination)?;

    if owner.is_none() && group.is_none() {
        return Ok(());
    }

    if let Some(existing) = existing {
        if ownership_matches(&owner, &group, existing) {
            return Ok(());
        }
    }

    unix_fs::fchown(fd, owner, group).map_err(|error| {
        MetadataError::new("preserve ownership", destination, io::Error::from(error))
    })?;

    Ok(())
}

/// Applies ownership from a protocol `FileEntry` on Unix.
///
/// Resolves UID/GID from the entry using overrides, mappings, and numeric-id
/// rules. Delegates to fake-super xattr storage when `--fake-super` is active.
/// Skips the chown syscall when the resolved values already match `cached_meta`.
// upstream: rsync.c:set_file_attrs() - chown path for receiver-side file entries
#[cfg(unix)]
pub(super) fn apply_ownership_from_entry(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
    cached_meta: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    use rustix::fs::{AtFlags, CWD, chownat};
    use rustix::process::{RawGid, RawUid};

    if !options.owner()
        && !options.group()
        && options.owner_override().is_none()
        && options.group_override().is_none()
    {
        return Ok(());
    }

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

    // upstream: rsync.c:set_file_attrs() - fake-super stores ownership in xattr
    if options.fake_super_enabled() {
        return apply_ownership_via_fake_super(destination, entry, raw_uid, raw_gid);
    }

    let owner = if let Some(uid_override) = options.owner_override() {
        Some(ownership::uid_from_raw(uid_override as RawUid))
    } else if options.owner() {
        entry.uid().and_then(|uid| {
            let mut mapped_uid = uid as RawUid;
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

    if owner.is_some() || group.is_some() {
        // upstream: rsync.c:set_file_attrs() - skips chown when uid/gid already match
        let needs_chown = match cached_meta {
            Some(meta) => {
                let current_uid = meta.uid();
                let current_gid = meta.gid();
                let desired_uid = owner.map(|o| o.as_raw()).unwrap_or(current_uid);
                let desired_gid = group.map(|g| g.as_raw()).unwrap_or(current_gid);
                current_uid != desired_uid || current_gid != desired_gid
            }
            None => true,
        };

        if needs_chown {
            chownat(CWD, destination, owner, group, AtFlags::empty()).map_err(|error| {
                MetadataError::new("preserve ownership", destination, io::Error::from(error))
            })?;
        }
    }

    Ok(())
}

/// Stores ownership metadata via fake-super xattr instead of applying directly.
///
/// Used when `--fake-super` is enabled, allowing non-root users to
/// preserve ownership information in extended attributes. Encodes
/// mode, uid, gid, and rdev into a `user.rsync.%stat` xattr.
// upstream: rsync.c:set_file_attrs() with am_root==0 and fake_super active
#[cfg(unix)]
fn apply_ownership_via_fake_super(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    uid: Option<u32>,
    gid: Option<u32>,
) -> Result<(), MetadataError> {
    use crate::fake_super::{FakeSuperStat, load_fake_super, store_fake_super};

    // upstream: xattrs.c:set_stat_xattr() encodes the full mode (S_IFMT + perms)
    // so a later read can rebuild the file type, not just the permission bits.
    let mode = entry.mode();
    let uid = uid.unwrap_or(0);
    let gid = gid.unwrap_or(0);

    let rdev = if entry.file_type().is_device() {
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

    // upstream: xattrs.c:read_stat_xattr() consults the existing xattr so an
    // unchanged stat skips the rewrite. Mirrors `set_file_attrs()`'s "no-op
    // when current state already matches" fast path.
    if let Ok(Some(existing)) = load_fake_super(destination)
        && existing == stat
    {
        return Ok(());
    }

    store_fake_super(destination, &stat)
        .map_err(|error| MetadataError::new("store fake-super metadata", destination, error))
}

/// No-op stub for non-Unix platforms where ownership (`chown`) is not supported.
///
/// Returns `Ok(())` unconditionally since Windows and other non-Unix targets
/// do not support POSIX ownership semantics.
#[cfg(not(unix))]
pub(super) fn apply_ownership_from_entry(
    _destination: &Path,
    _entry: &protocol::flist::FileEntry,
    _options: &MetadataOptions,
    _cached_meta: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    Ok(())
}
