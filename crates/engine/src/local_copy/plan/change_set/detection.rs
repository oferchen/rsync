use std::fs;
use std::time::SystemTime;

use ::metadata::MetadataOptions;

use super::{LocalCopyChangeSet, TimeChange};

impl LocalCopyChangeSet {
    /// Computes a change set for a file-like entry (regular files and symlinks).
    ///
    /// The position-2 `c` glyph is reserved for `--checksum` mode (upstream:
    /// `generator.c:1942` - `if (always_checksum > 0) iflags |=
    /// ITEM_REPORT_CHANGE`); this constructor leaves it cleared. Callers
    /// running under `--checksum` should use
    /// [`for_file_with_checksum`](Self::for_file_with_checksum) and pass
    /// `checksum_enabled = true`.
    #[allow(clippy::too_many_arguments)]
    pub fn for_file(
        metadata: &fs::Metadata,
        existing: Option<&fs::Metadata>,
        metadata_options: &MetadataOptions,
        destination_previously_existed: bool,
        wrote_data: bool,
        xattrs_enabled: bool,
        acls_enabled: bool,
    ) -> Self {
        Self::for_file_with_checksum(
            metadata,
            existing,
            metadata_options,
            destination_previously_existed,
            wrote_data,
            xattrs_enabled,
            acls_enabled,
            false,
        )
    }

    /// Computes a change set, gating the `c` (checksum) glyph on `checksum_enabled`.
    ///
    /// upstream: generator.c:1942 - `if (always_checksum > 0) iflags |=
    /// ITEM_REPORT_CHANGE`. The position-2 `c` glyph fires only under
    /// `--checksum`; without it, even when the receiver wrote new data, the
    /// itemize line keeps `.` in slot 2. Callers that have an explicit
    /// `--checksum` flag should use this constructor and pass `true` only
    /// when checksum-mode is active.
    #[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
    pub fn for_file_with_checksum(
        metadata: &fs::Metadata,
        existing: Option<&fs::Metadata>,
        metadata_options: &MetadataOptions,
        destination_previously_existed: bool,
        wrote_data: bool,
        xattrs_enabled: bool,
        acls_enabled: bool,
        checksum_enabled: bool,
    ) -> Self {
        let mut change_set = Self::new();

        if wrote_data && checksum_enabled {
            change_set = change_set.with_checksum_changed(true);
        }

        if !destination_previously_existed {
            change_set = change_set.with_size_changed(true);
        } else if let Some(existing_metadata) = existing
            && metadata.len() != existing_metadata.len()
        {
            change_set = change_set.with_size_changed(true);
        }

        change_set = change_set.with_time_change(determine_time_change(
            metadata_options,
            metadata,
            existing,
            destination_previously_existed,
            wrote_data,
        ));

        // upstream: generator.c:542-549 - `#ifndef CAN_CHMOD_SYMLINK` skips
        // the perm-compare entirely for symlinks. On Linux/macOS chmod
        // follows the link, so a symlink's own perms cannot be changed and
        // upstream rsync never reports ITEM_REPORT_PERMS for them.
        let is_symlink = metadata.file_type().is_symlink();
        if !is_symlink
            && metadata_options.permissions()
            && permissions_changed(metadata, existing, destination_previously_existed)
        {
            change_set = change_set.with_permissions_changed(true);
        }

        if !is_symlink && metadata_options.chmod().is_some() {
            change_set = change_set.with_permissions_changed(true);
        }

        if owner_changed(
            metadata_options,
            metadata,
            existing,
            destination_previously_existed,
        ) {
            change_set = change_set.with_owner_changed(true);
        }

        if group_changed(
            metadata_options,
            metadata,
            existing,
            destination_previously_existed,
        ) {
            change_set = change_set.with_group_changed(true);
        }

        if metadata_options.user_mapping().is_some() {
            change_set = change_set.with_owner_changed(true);
        }

        if metadata_options.group_mapping().is_some() {
            change_set = change_set.with_group_changed(true);
        }

        if xattrs_enabled {
            change_set = change_set.with_xattr_changed(true);
        }

        if acls_enabled {
            change_set = change_set.with_acl_changed(true);
        }

        change_set
    }
}

/// Determines the appropriate time-change variant based on metadata options
/// and the relationship between new and existing file metadata.
fn determine_time_change(
    metadata_options: &MetadataOptions,
    metadata: &fs::Metadata,
    existing: Option<&fs::Metadata>,
    destination_previously_existed: bool,
    wrote_data: bool,
) -> Option<TimeChange> {
    if metadata_options.times() {
        if !destination_previously_existed {
            return Some(TimeChange::Modified);
        }

        let new_mtime = metadata_modified_time(metadata);
        let old_mtime = existing.and_then(metadata_modified_time);

        match (new_mtime, old_mtime) {
            (Some(new_value), Some(old_value)) if new_value == old_value => None,
            _ => Some(TimeChange::Modified),
        }
    } else if wrote_data || !destination_previously_existed {
        Some(TimeChange::TransferTime)
    } else {
        None
    }
}

/// Returns `true` when the permission bits differ between the new and existing metadata.
fn permissions_changed(
    metadata: &fs::Metadata,
    existing: Option<&fs::Metadata>,
    destination_previously_existed: bool,
) -> bool {
    let new_mode = metadata_mode(metadata);
    if !destination_previously_existed {
        return new_mode.is_some();
    }

    match (new_mode, existing.and_then(metadata_mode)) {
        (Some(new_value), Some(old_value)) => new_value != old_value,
        (Some(_), None) => true,
        (None, Some(_)) => true,
        _ => false,
    }
}

/// Returns `true` when the owner (uid) differs or an override is in effect.
fn owner_changed(
    metadata_options: &MetadataOptions,
    metadata: &fs::Metadata,
    existing: Option<&fs::Metadata>,
    destination_previously_existed: bool,
) -> bool {
    if let Some(override_uid) = metadata_options.owner_override() {
        return existing.and_then(metadata_uid) != Some(override_uid);
    }

    if !metadata_options.owner() {
        return false;
    }

    let new_uid = metadata_uid(metadata);
    if !destination_previously_existed {
        return new_uid.is_some();
    }

    match (new_uid, existing.and_then(metadata_uid)) {
        (Some(new_value), Some(old_value)) => new_value != old_value,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Returns `true` when the group (gid) differs or an override is in effect.
fn group_changed(
    metadata_options: &MetadataOptions,
    metadata: &fs::Metadata,
    existing: Option<&fs::Metadata>,
    destination_previously_existed: bool,
) -> bool {
    if let Some(override_gid) = metadata_options.group_override() {
        return existing.and_then(metadata_gid) != Some(override_gid);
    }

    if !metadata_options.group() {
        return false;
    }

    let new_gid = metadata_gid(metadata);
    if !destination_previously_existed {
        return new_gid.is_some();
    }

    match (new_gid, existing.and_then(metadata_gid)) {
        (Some(new_value), Some(old_value)) => new_value != old_value,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Extracts the modification time from filesystem metadata.
fn metadata_modified_time(metadata: &fs::Metadata) -> Option<SystemTime> {
    metadata.modified().ok()
}

/// Extracts the Unix permission mode from filesystem metadata.
#[cfg(unix)]
fn metadata_mode(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.mode())
}

/// Returns `None` on non-Unix platforms where permission modes are unavailable.
#[cfg(not(unix))]
fn metadata_mode(_metadata: &fs::Metadata) -> Option<u32> {
    None
}

/// Extracts the owner uid from filesystem metadata.
#[cfg(unix)]
fn metadata_uid(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.uid())
}

/// Returns `None` on non-Unix platforms where uid is unavailable.
#[cfg(not(unix))]
fn metadata_uid(_metadata: &fs::Metadata) -> Option<u32> {
    None
}

/// Extracts the group gid from filesystem metadata.
#[cfg(unix)]
fn metadata_gid(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.gid())
}

/// Returns `None` on non-Unix platforms where gid is unavailable.
#[cfg(not(unix))]
fn metadata_gid(_metadata: &fs::Metadata) -> Option<u32> {
    None
}
