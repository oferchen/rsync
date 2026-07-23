use std::fs;
use std::time::SystemTime;

use ::metadata::{MetadataOptions, ModifyWindow};

use crate::local_copy::executor::system_time_within_window;

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
        modify_window: ModifyWindow,
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
            modify_window,
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
        modify_window: ModifyWindow,
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
            modify_window,
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

    /// Computes a change set for an existing destination directory.
    ///
    /// Mirrors upstream `itemize()` (`generator.c:518-579`) for the directory
    /// branch reached via `generator.c:1480-1483` when `statret == 0`: the
    /// receiver flags `ITEM_REPORT_TIME` when `mtime_differs` (gated by
    /// `!omit_dir_times`), `ITEM_REPORT_PERMS` when the masked mode bits
    /// differ, and `ITEM_REPORT_OWNER`/`ITEM_REPORT_GROUP` when ownership
    /// differs. ACL/xattr drift is signalled when those features are active.
    /// Symlink-only flags and size are skipped because directories never
    /// participate in those positions.
    pub fn for_existing_directory(
        source: &fs::Metadata,
        existing: &fs::Metadata,
        metadata_options: &MetadataOptions,
        omit_dir_times: bool,
        xattrs_enabled: bool,
        acls_enabled: bool,
        modify_window: ModifyWindow,
    ) -> Self {
        let mut change_set = Self::new();

        let times_preserved = metadata_options.times() && !omit_dir_times;
        if times_preserved {
            let new_mtime = metadata_modified_time(source);
            let old_mtime = metadata_modified_time(existing);
            // upstream: generator.c:533 - itemize() sets ITEM_REPORT_TIME via
            // `!same_time(file->modtime, 0, &sxp->st)`. same_time() (util1.c:1478)
            // compares whole seconds under `--modify-window`, so a sub-second
            // mtime drift on a directory must NOT light the `t` glyph. This is
            // type-agnostic in upstream: directories use the same same_time()
            // path as files.
            match (new_mtime, old_mtime) {
                (Some(new_value), Some(old_value))
                    if system_time_within_window(new_value, old_value, modify_window) => {}
                _ => {
                    change_set = change_set.with_time_change(Some(TimeChange::Modified));
                }
            }
        }

        if metadata_options.permissions() && permissions_changed(source, Some(existing), true) {
            change_set = change_set.with_permissions_changed(true);
        }
        if metadata_options.chmod().is_some() {
            change_set = change_set.with_permissions_changed(true);
        }

        if owner_changed(metadata_options, source, Some(existing), true) {
            change_set = change_set.with_owner_changed(true);
        }
        if group_changed(metadata_options, source, Some(existing), true) {
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

    /// Computes a change set for a recreated symlink whose existing
    /// destination already pointed somewhere different.
    ///
    /// Mirrors upstream `generator.c:1608-1609`, where the recreate path calls
    /// `itemize(... ITEM_LOCAL_CHANGE|ITEM_REPORT_CHANGE ...)`.
    /// `ITEM_REPORT_CHANGE` lights up the `c` glyph in position 2 to signal
    /// that the link target itself changed. `itemize()` then adds
    /// `ITEM_REPORT_TIME` when the symlink's mtime differs from the existing
    /// link's mtime (gated by `!omit_link_times`).
    ///
    /// The mtime comparison uses `same_time()` semantics via
    /// `system_time_within_window`, not exact equality: upstream `itemize()` at
    /// `generator.c:533-534` calls `mtime_differs()` ->
    /// `same_time(stp->st_mtime, ..., file->modtime, ...)` (util1.c:1478), which
    /// with the default `modify_window == 0` compares WHOLE SECONDS only
    /// (`f1_sec == f2_sec`) and ignores the fractional part. Two links whose
    /// mtimes fall in the same wall-clock second but differ in nanoseconds
    /// therefore do NOT light the `t` glyph upstream; an exact `SystemTime`
    /// comparison would spuriously report `ITEM_REPORT_TIME` (rendering
    /// `cLc.t......` instead of `cLc........`).
    pub fn for_recreated_symlink(
        source: &fs::Metadata,
        existing: &fs::Metadata,
        metadata_options: &MetadataOptions,
        omit_link_times: bool,
        modify_window: ModifyWindow,
    ) -> Self {
        let mut change_set = Self::new().with_checksum_changed(true);

        let times_preserved = metadata_options.times() && !omit_link_times;
        if times_preserved {
            let new_mtime = metadata_modified_time(source);
            let old_mtime = metadata_modified_time(existing);
            match (new_mtime, old_mtime) {
                (Some(new_value), Some(old_value))
                    if system_time_within_window(new_value, old_value, modify_window) => {}
                _ => {
                    change_set = change_set.with_time_change(Some(TimeChange::Modified));
                }
            }
        }

        // upstream: generator.c:542-549 - `#ifndef CAN_CHMOD_SYMLINK` skips
        // perm/owner/group bits for symlinks on platforms where chmod follows
        // the link. Linux and macOS both behave that way for symlinks, so the
        // itemize line never reports symlink perm changes in those slots.
        change_set
    }

    /// Computes a change set for a device or special file whose existing
    /// destination is being recreated because it differs from the source.
    ///
    /// Mirrors upstream `generator.c:1677-1682`: after `atomic_create()`
    /// recreates the node, `itemize()` runs with
    /// `ITEM_LOCAL_CHANGE|ITEM_REPORT_CHANGE` and `statret == 0`.
    /// `ITEM_REPORT_CHANGE` lights the position-2 `c` glyph; its source is the
    /// `st_rdev` mismatch (for devices) or the `_S_IFMT` mismatch (for
    /// specials) that made `quick_check_ok()` return false at
    /// `generator.c:657-671`, so the caller passes that comparison in via
    /// `content_differs`. `itemize()` (`generator.c:508-549`) then derives the
    /// remaining glyphs exactly as for a regular file: `ITEM_REPORT_TIME`
    /// (rendered `t` when preserving mtimes and they differ, `T` when mtimes
    /// are not preserved because `ITEM_LOCAL_CHANGE` marks a fresh recreate),
    /// `ITEM_REPORT_PERMS`, and owner/group. Size is never reported because
    /// `S_ISREG` is false for device and special nodes (`generator.c:521`).
    #[allow(clippy::too_many_arguments)]
    pub fn for_recreated_device(
        source: &fs::Metadata,
        existing: &fs::Metadata,
        metadata_options: &MetadataOptions,
        modify_window: ModifyWindow,
        content_differs: bool,
        xattrs_enabled: bool,
        acls_enabled: bool,
    ) -> Self {
        let mut change_set = Self::new();

        // upstream: generator.c:1680-1681 - the recreate path passes
        // ITEM_REPORT_CHANGE, but it is only reached because quick_check_ok()
        // found a differing device number / special type. Light the `c` glyph
        // from that same comparison.
        if content_differs {
            change_set = change_set.with_checksum_changed(true);
        }

        // upstream: generator.c:526-530 - with preserve_mtimes (`keep_time`),
        // ITEM_REPORT_TIME fires only when the mtime differs (rendered `t`).
        // Without preserve_mtimes, the ITEM_LOCAL_CHANGE branch sets
        // ITEM_REPORT_TIME unconditionally for a freshly recreated node
        // (rendered `T` via log.c:716-717). When the node is not being
        // recreated (`content_differs == false`, i.e. the upstream identical
        // branch that passes iflags 0) the time is reported only under
        // preserve_mtimes when it differs.
        if metadata_options.times() {
            let new_mtime = metadata_modified_time(source);
            let old_mtime = metadata_modified_time(existing);
            match (new_mtime, old_mtime) {
                (Some(new_value), Some(old_value))
                    if system_time_within_window(new_value, old_value, modify_window) => {}
                _ => change_set = change_set.with_time_change(Some(TimeChange::Modified)),
            }
        } else if content_differs {
            change_set = change_set.with_time_change(Some(TimeChange::TransferTime));
        }

        // upstream: generator.c:540-545 - perms compared via BITS_EQUAL under
        // preserve_perms; an explicit --chmod always reports.
        if metadata_options.permissions() && permissions_changed(source, Some(existing), true) {
            change_set = change_set.with_permissions_changed(true);
        }
        if metadata_options.chmod().is_some() {
            change_set = change_set.with_permissions_changed(true);
        }

        // upstream: generator.c:546-549 - owner/group flags.
        if owner_changed(metadata_options, source, Some(existing), true) {
            change_set = change_set.with_owner_changed(true);
        }
        if group_changed(metadata_options, source, Some(existing), true) {
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
    modify_window: ModifyWindow,
) -> Option<TimeChange> {
    if metadata_options.times() {
        if !destination_previously_existed {
            return Some(TimeChange::Modified);
        }

        let new_mtime = metadata_modified_time(metadata);
        let old_mtime = existing.and_then(metadata_modified_time);

        // upstream: generator.c:533 - the ITEM_REPORT_TIME bit is set via
        // `!same_time(file->modtime, 0, &sxp->st)`. same_time() (util1.c:1478)
        // treats two mtimes as equal when their whole-second delta is within
        // `--modify-window`, so a sub-window drift must NOT light the `t` glyph.
        match (new_mtime, old_mtime) {
            (Some(new_value), Some(old_value))
                if system_time_within_window(new_value, old_value, modify_window) =>
            {
                None
            }
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
