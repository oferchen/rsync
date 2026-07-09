//! Directory entry planning and action classification.
//!
//! Determines how each directory entry should be handled (copy, skip, link)
//! based on file type, filter rules, and command-line options. Mirrors the
//! decision logic in upstream `flist.c:make_file()` and `flist.c:flist_sort_and_clean()`.

use std::borrow::Cow;
use std::ffi::OsStr;
#[cfg(test)]
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::local_copy::{
    CopyContext, DeleteTiming, LocalCopyArgumentError, LocalCopyError, delete_extraneous_entries,
    follow_symlink_metadata,
};

use super::super::{
    emit_cannot_convert_filename, name_is_convertible, non_empty_path, symlink_target_is_safe,
    transcode_filename_component,
};
use super::support::{DirectoryEntry, is_device, is_fifo};

/// Action to take for a directory entry during recursive copy.
// upstream: generator.c:recv_generator() - entry dispatch
#[derive(Clone, Copy)]
pub(crate) enum EntryAction {
    /// Entry excluded by filter rules.
    SkipExcluded,
    /// Entry is a non-regular file type that is not being preserved.
    SkipNonRegular,
    /// Entry is on a different filesystem and `--one-file-system` is active.
    SkipMountPoint,
    /// Recurse into the directory.
    CopyDirectory,
    /// Copy the regular file.
    CopyFile,
    /// Recreate the symbolic link.
    CopySymlink,
    /// Recreate the FIFO (named pipe).
    CopyFifo,
    /// Recreate the device node.
    CopyDevice,
    /// Copy a device node as a regular file (`--copy-devices`).
    CopyDeviceAsFile,
}

/// A directory entry paired with its planned action and computed relative path.
pub(crate) struct PlannedEntry<'a> {
    pub(crate) entry: &'a DirectoryEntry,
    pub(crate) relative: PathBuf,
    pub(crate) action: EntryAction,
    pub(crate) metadata_override: Option<fs::Metadata>,
}

impl<'a> PlannedEntry<'a> {
    /// Returns the effective metadata, preferring the override when present.
    pub(crate) fn metadata(&self) -> &fs::Metadata {
        self.metadata_override
            .as_ref()
            .unwrap_or(&self.entry.metadata)
    }
}

/// Result of planning all entries in a directory for the recursive copy.
pub(crate) struct DirectoryPlan<'a> {
    pub(crate) planned_entries: Vec<PlannedEntry<'a>>,
    /// Names of entries to keep when deleting extraneous files.
    ///
    /// When `--iconv` is not configured this borrows the
    /// `DirectoryEntry::file_name` fields zero-copy. With `--iconv` the
    /// destination filename is the iconv-converted form (LOCAL -> REMOTE),
    /// so the keep-list must hold the converted bytes for the deletion
    /// step to compare them against the actual on-disk destination
    /// entries. The `Cow` keeps the no-iconv hot path allocation-free
    /// while letting the iconv path own the transcoded `OsString`.
    pub(crate) keep_names: Vec<Cow<'a, OsStr>>,
    pub(crate) deletion_enabled: bool,
    pub(crate) delete_timing: Option<DeleteTiming>,
}

/// Centralized decision policy for how to treat a directory entry.
///
/// This encapsulates the "strategy" for turning the entry type + context
/// into an [`EntryAction`] and whether the name should be preserved for
/// deletion tracking.
///
/// When `prune_empty_dirs` is active and a directory is excluded by a
/// non-directory-specific filter rule (e.g., `*` rather than `cache/`),
/// we still return [`EntryAction::CopyDirectory`] so the directory is
/// descended into. This allows file-level include rules to be evaluated
/// inside the directory; the pruning logic in [`copy_directory_recursive`]
/// removes the directory afterwards if no children survive filtering.
/// This matches upstream rsync behavior where the sender includes all
/// directories in the file list and the receiver prunes empty ones
/// post-hoc in `flist_sort_and_clean()`.
fn decide_entry_action(
    context: &CopyContext,
    relative_path: &Path,
    entry_type: fs::FileType,
    effective_type: fs::FileType,
    keep_name: &mut bool,
) -> Result<EntryAction, LocalCopyError> {
    if !context.allows(relative_path, effective_type.is_dir()) {
        // upstream: flist.c:flist_sort_and_clean() - when -m is active,
        // directories excluded by non-dir-specific rules are still traversed
        // so that file-level include rules can rescue their contents.
        if effective_type.is_dir()
            && context.prune_empty_dirs_enabled()
            && context.excluded_dir_by_non_dir_rule(relative_path)
        {
            return Ok(EntryAction::CopyDirectory);
        }

        // upstream: a file absent from the sender flist (sender-side hide, or any
        // exclude under --delete-excluded) is extraneous at the receiver and is
        // removed by --del/--delete-during. Drop it from the keep-set exactly when
        // the delete-side filter permits deletion. Protective both-sides excludes
        // keep allows_deletion() == false, so they stay in the keep-set and are
        // never deleted (e.g. a per-dir `- *.deep` still protects nodel.deep).
        if context.allows_deletion(relative_path, effective_type.is_dir()) {
            *keep_name = false;
        }
        return Ok(EntryAction::SkipExcluded);
    }

    if entry_type.is_dir() {
        return Ok(EntryAction::CopyDirectory);
    }

    if effective_type.is_file() {
        return Ok(EntryAction::CopyFile);
    }

    if effective_type.is_dir() {
        return Ok(EntryAction::CopyDirectory);
    }

    // Only treat as a symlink when the effective type is still a symlink
    // (i.e., --copy-links did NOT resolve it to the referent).  When
    // --copy-links is active and the target is a FIFO or device, we must
    // fall through to the FIFO / device branches below.
    // upstream: generator.c:1155 list_file_entry() lists every flist entry, so
    // `--list-only` records symlinks, FIFOs, and devices (dry-run only reports
    // them) even without `--links`/`--specials`/`--devices`; a real transfer
    // without those flags still skips the non-regular entry.
    let list_only = context.list_only_enabled();

    if entry_type.is_symlink() && effective_type.is_symlink() {
        if context.links_enabled() || list_only {
            return Ok(EntryAction::CopySymlink);
        }
        *keep_name = false;
        return Ok(EntryAction::SkipNonRegular);
    }

    if is_fifo(effective_type) {
        if context.specials_enabled() || list_only {
            return Ok(EntryAction::CopyFifo);
        }
        *keep_name = false;
        return Ok(EntryAction::SkipNonRegular);
    }

    if is_device(effective_type) {
        if context.copy_devices_as_files_enabled() {
            return Ok(EntryAction::CopyDeviceAsFile);
        }
        if context.devices_enabled() || list_only {
            return Ok(EntryAction::CopyDevice);
        }
        *keep_name = false;
        return Ok(EntryAction::SkipNonRegular);
    }

    Err(LocalCopyError::invalid_argument(
        LocalCopyArgumentError::UnsupportedFileType,
    ))
}

/// Plans actions for all entries in a directory.
///
/// Iterates over pre-sorted directory entries, applies filter rules,
/// resolves symlinks when `--copy-links` or `--copy-dirlinks` is active,
/// and determines the appropriate action for each entry.
// upstream: flist.c:send_directory() - builds file list from directory
pub(crate) fn plan_directory_entries<'a>(
    context: &mut CopyContext,
    entries: &'a [DirectoryEntry],
    relative: Option<&Path>,
    root_device: Option<u64>,
) -> Result<DirectoryPlan<'a>, LocalCopyError> {
    let deletion_enabled = context.options().delete_extraneous();
    let delete_timing = context.delete_timing();
    let mut keep_names = if deletion_enabled {
        Vec::with_capacity(entries.len())
    } else {
        Vec::new()
    };
    let mut planned_entries = Vec::with_capacity(entries.len());

    // Reusable buffer for building relative paths. When a base relative
    // path is provided, we push each entry name and pop it after use,
    // avoiding a per-entry PathBuf allocation from Path::join.
    let mut relative_buf = relative.map(Path::to_path_buf);

    for entry in entries {
        context.enforce_timeout()?;
        context.register_progress();

        let file_name = &entry.file_name;
        let entry_metadata = &entry.metadata;
        let entry_type = entry_metadata.file_type();

        // upstream: flist.c:make_file() - skip entries with bogus zero st_mode
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if entry_metadata.mode() == 0 {
                continue;
            }
        }

        let mut metadata_override = None;
        let mut effective_type = entry_type;

        if entry_type.is_symlink()
            && (context.copy_links_enabled() || context.copy_dirlinks_enabled())
        {
            match follow_symlink_metadata(entry.path.as_path()) {
                Ok(target_metadata) => {
                    let target_type = target_metadata.file_type();
                    if context.copy_links_enabled()
                        || (context.copy_dirlinks_enabled() && target_type.is_dir())
                    {
                        effective_type = target_type;
                        metadata_override = Some(target_metadata);
                    }
                }
                Err(error) => {
                    if context.copy_links_enabled() {
                        return Err(error);
                    }
                }
            }
        }

        let relative_path = if let Some(buf) = &mut relative_buf {
            buf.push(Path::new(&file_name));
            buf.clone()
        } else {
            PathBuf::from(Path::new(&file_name))
        };

        // upstream: flist.c:1614-1638 send_file1() - a name that cannot be
        // strictly transcoded under --iconv is dropped from the flist with a
        // diagnostic and io_error |= IOERR_GENERAL. Detecting it during planning
        // sets io_error before this directory's delete pass runs, so deletions
        // are suppressed exactly as upstream's build-then-generate order does;
        // the entry never enters the plan (no copy, no keep-name, no ndx).
        if !name_is_convertible(file_name.as_os_str(), context.options().iconv()) {
            emit_cannot_convert_filename(relative_path.as_os_str());
            context.record_iconv_conversion_error();
            if let Some(buf) = &mut relative_buf {
                buf.pop();
            }
            continue;
        }
        context.record_file_list_entry(non_empty_path(relative_path.as_path()));

        let mut keep_name = true;
        let mut action = decide_entry_action(
            context,
            relative_path.as_path(),
            entry_type,
            effective_type,
            &mut keep_name,
        )?;

        // When --copy-unsafe-links is active, the planner must dereference
        // unsafe symlinks before the executor sees them.  When only
        // --safe-links is active (without --copy-unsafe-links), we leave
        // the CopySymlink action in place so that copy_symlink() can
        // handle it and record the SkippedUnsafeSymlink event properly.
        if matches!(action, EntryAction::CopySymlink) && context.copy_unsafe_links_enabled() {
            match fs::read_link(entry.path.as_path()) {
                Ok(target) => {
                    let safety_rel = context.strip_safety_prefix(relative_path.as_path());
                    if !symlink_target_is_safe(&target, safety_rel) {
                        match follow_symlink_metadata(entry.path.as_path()) {
                            Ok(target_metadata) => {
                                let target_type = target_metadata.file_type();
                                if target_type.is_dir() {
                                    action = EntryAction::CopyDirectory;
                                    metadata_override = Some(target_metadata);
                                } else if target_type.is_file() {
                                    action = EntryAction::CopyFile;
                                    metadata_override = Some(target_metadata);
                                } else if is_fifo(target_type) {
                                    if context.specials_enabled() {
                                        action = EntryAction::CopyFifo;
                                        metadata_override = Some(target_metadata);
                                    } else {
                                        keep_name = false;
                                        action = EntryAction::SkipNonRegular;
                                        metadata_override = None;
                                    }
                                } else if is_device(target_type) {
                                    if context.copy_devices_as_files_enabled() {
                                        action = EntryAction::CopyDeviceAsFile;
                                        metadata_override = Some(target_metadata);
                                    } else if context.devices_enabled() {
                                        action = EntryAction::CopyDevice;
                                        metadata_override = Some(target_metadata);
                                    } else {
                                        keep_name = false;
                                        action = EntryAction::SkipNonRegular;
                                        metadata_override = None;
                                    }
                                } else {
                                    keep_name = false;
                                    action = EntryAction::SkipNonRegular;
                                    metadata_override = None;
                                }
                            }
                            Err(_) => {
                                // upstream: flist.c:1277-1282 - dangling symlink
                                // whose target was to be dereferenced by
                                // --copy-unsafe-links; log and skip.
                                eprintln!("symlink has no referent: {}", entry.path.display());
                                context.record_io_error();
                                keep_name = false;
                                action = EntryAction::SkipNonRegular;
                            }
                        }
                    }
                }
                Err(_) => {
                    // Cannot read symlink target - skip with I/O error.
                    eprintln!("symlink has no referent: {}", entry.path.display());
                    context.record_io_error();
                    keep_name = false;
                    action = EntryAction::SkipNonRegular;
                }
            }
        }

        if matches!(action, EntryAction::CopyDirectory)
            && context.one_file_system_enabled()
            && let Some(root) = root_device
            && let Some(entry_device) = crate::local_copy::overrides::device_identifier(
                entry.path.as_path(),
                metadata_override.as_ref().unwrap_or(entry_metadata),
            )
            && entry_device != root
        {
            action = EntryAction::SkipMountPoint;
        }

        if deletion_enabled && keep_name {
            let preserve_name = match delete_timing {
                Some(DeleteTiming::Before) => matches!(
                    action,
                    EntryAction::CopyDirectory
                        | EntryAction::SkipExcluded
                        | EntryAction::SkipMountPoint
                ),
                _ => true,
            };

            if preserve_name {
                // upstream: flist.c:1579-1603 + flist.c:738-754 - the
                // receiver hits the filesystem with the iconv-converted
                // name; align the keep-list so deletion does not wipe
                // freshly-written entries when --iconv is configured.
                keep_names.push(transcode_filename_component(
                    file_name.as_os_str(),
                    context.options().iconv(),
                ));
            }
        }

        planned_entries.push(PlannedEntry {
            entry,
            relative: relative_path,
            action,
            metadata_override,
        });
        if let Some(buf) = &mut relative_buf {
            buf.pop();
        }
    }

    Ok(DirectoryPlan {
        planned_entries,
        keep_names,
        deletion_enabled,
        delete_timing,
    })
}

/// Applies pre-transfer deletions when `--delete-before` is active.
// upstream: generator.c:do_delete_pass() - pre-transfer deletion
pub(crate) fn apply_pre_transfer_deletions(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    plan: &DirectoryPlan<'_>,
) -> Result<(), LocalCopyError> {
    if plan.deletion_enabled && matches!(plan.delete_timing, Some(DeleteTiming::Before)) {
        delete_extraneous_entries(context, destination, relative, &plan.keep_names)?;
    }
    Ok(())
}

/// Plans directory entries with parallel metadata prefetching.
///
/// When the `parallel` feature is enabled and the context options require
/// expensive metadata operations (symlink following, device checks), this
/// function prefetches the metadata in parallel before running the sequential
/// planning logic.
///
/// # Performance
///
/// For directories with many symlinks or when `--one-file-system` is enabled,
/// this can provide significant speedup by parallelizing filesystem syscalls.
#[allow(dead_code)] // Prepared for integration with parallel directory traversal
pub(crate) fn plan_directory_entries_parallel<'a>(
    context: &mut CopyContext,
    entries: &'a [DirectoryEntry],
    relative: Option<&Path>,
    root_device: Option<u64>,
) -> Result<DirectoryPlan<'a>, LocalCopyError> {
    use super::parallel_planner::{PrefetchConfig, prefetch_entry_metadata};

    let config = PrefetchConfig {
        follow_symlinks: context.copy_links_enabled() || context.copy_dirlinks_enabled(),
        read_symlink_targets: context.copy_unsafe_links_enabled(),
        check_devices: context.one_file_system_enabled() && root_device.is_some(),
    };

    let prefetched = prefetch_entry_metadata(entries, config);

    plan_directory_entries_with_prefetch(context, entries, relative, root_device, &prefetched)
}

/// Plans directory entries using prefetched metadata.
///
/// This is the sequential planning phase that uses pre-gathered metadata
/// to avoid blocking on filesystem syscalls.
fn plan_directory_entries_with_prefetch<'a>(
    context: &mut CopyContext,
    entries: &'a [DirectoryEntry],
    relative: Option<&Path>,
    root_device: Option<u64>,
    prefetched: &[super::parallel_planner::PrefetchedEntryData],
) -> Result<DirectoryPlan<'a>, LocalCopyError> {
    let deletion_enabled = context.options().delete_extraneous();
    let delete_timing = context.delete_timing();
    let mut keep_names = if deletion_enabled {
        Vec::with_capacity(entries.len())
    } else {
        Vec::new()
    };
    let mut planned_entries = Vec::with_capacity(entries.len());

    // Reusable buffer for building relative paths (same optimization as
    // plan_directory_entries).
    let mut relative_buf = relative.map(Path::to_path_buf);

    for (entry, prefetch) in entries.iter().zip(prefetched.iter()) {
        context.enforce_timeout()?;
        context.register_progress();

        let file_name = &entry.file_name;
        let entry_metadata = &entry.metadata;
        let entry_type = entry_metadata.file_type();

        // upstream: flist.c:make_file() - skip entries with bogus zero st_mode
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if entry_metadata.mode() == 0 {
                continue;
            }
        }

        let mut metadata_override = None;
        let mut effective_type = entry_type;

        if entry_type.is_symlink()
            && (context.copy_links_enabled() || context.copy_dirlinks_enabled())
        {
            if let Some(ref result) = prefetch.symlink_target_metadata {
                match result {
                    Ok(target_metadata) => {
                        let target_type = target_metadata.file_type();
                        if context.copy_links_enabled()
                            || (context.copy_dirlinks_enabled() && target_type.is_dir())
                        {
                            effective_type = target_type;
                            metadata_override = Some(target_metadata.clone());
                        }
                    }
                    Err(_) if context.copy_links_enabled() => {
                        // Re-fetch to get the actual error for reporting
                        return Err(follow_symlink_metadata(entry.path.as_path()).unwrap_err());
                    }
                    Err(_) => {}
                }
            }
        }

        let relative_path = if let Some(buf) = &mut relative_buf {
            buf.push(Path::new(&file_name));
            buf.clone()
        } else {
            PathBuf::from(Path::new(&file_name))
        };

        // upstream: flist.c:1614-1638 send_file1() - a name that cannot be
        // strictly transcoded under --iconv is dropped from the flist with a
        // diagnostic and io_error |= IOERR_GENERAL. Detecting it during planning
        // sets io_error before this directory's delete pass runs, so deletions
        // are suppressed exactly as upstream's build-then-generate order does;
        // the entry never enters the plan (no copy, no keep-name, no ndx).
        if !name_is_convertible(file_name.as_os_str(), context.options().iconv()) {
            emit_cannot_convert_filename(relative_path.as_os_str());
            context.record_iconv_conversion_error();
            if let Some(buf) = &mut relative_buf {
                buf.pop();
            }
            continue;
        }
        context.record_file_list_entry(non_empty_path(relative_path.as_path()));

        let mut keep_name = true;
        let mut action = decide_entry_action(
            context,
            relative_path.as_path(),
            entry_type,
            effective_type,
            &mut keep_name,
        )?;

        // Handle --copy-unsafe-links with prefetched symlink target.
        // When only --safe-links is active (no --copy-unsafe-links), we
        // leave CopySymlink so that copy_symlink() records
        // SkippedUnsafeSymlink properly.
        if matches!(action, EntryAction::CopySymlink) && context.copy_unsafe_links_enabled() {
            if let Some(ref result) = prefetch.symlink_target {
                match result {
                    Ok(target) => {
                        let safety_rel = context.strip_safety_prefix(relative_path.as_path());
                        if !symlink_target_is_safe(target, safety_rel) {
                            // Use prefetched metadata or re-fetch; dangling
                            // symlinks yield None and are skipped below.
                            let fetched_meta;
                            let target_metadata =
                                if let Some(ref meta_result) = prefetch.symlink_target_metadata {
                                    meta_result.as_ref().ok()
                                } else {
                                    match follow_symlink_metadata(entry.path.as_path()) {
                                        Ok(m) => {
                                            fetched_meta = m;
                                            Some(&fetched_meta)
                                        }
                                        Err(_) => None,
                                    }
                                };

                            if let Some(target_metadata) = target_metadata {
                                let target_type = target_metadata.file_type();
                                if target_type.is_dir() {
                                    action = EntryAction::CopyDirectory;
                                    metadata_override = Some(target_metadata.clone());
                                } else if target_type.is_file() {
                                    action = EntryAction::CopyFile;
                                    metadata_override = Some(target_metadata.clone());
                                } else if is_fifo(target_type) {
                                    if context.specials_enabled() {
                                        action = EntryAction::CopyFifo;
                                        metadata_override = Some(target_metadata.clone());
                                    } else {
                                        keep_name = false;
                                        action = EntryAction::SkipNonRegular;
                                        metadata_override = None;
                                    }
                                } else if is_device(target_type) {
                                    if context.copy_devices_as_files_enabled() {
                                        action = EntryAction::CopyDeviceAsFile;
                                        metadata_override = Some(target_metadata.clone());
                                    } else if context.devices_enabled() {
                                        action = EntryAction::CopyDevice;
                                        metadata_override = Some(target_metadata.clone());
                                    } else {
                                        keep_name = false;
                                        action = EntryAction::SkipNonRegular;
                                        metadata_override = None;
                                    }
                                } else {
                                    keep_name = false;
                                    action = EntryAction::SkipNonRegular;
                                    metadata_override = None;
                                }
                            } else {
                                // upstream: flist.c:1277-1282 - dangling symlink
                                // whose target was to be dereferenced by
                                // --copy-unsafe-links; log and skip.
                                eprintln!("symlink has no referent: {}", entry.path.display());
                                context.record_io_error();
                                keep_name = false;
                                action = EntryAction::SkipNonRegular;
                            }
                        }
                    }
                    Err(_) => {
                        // Cannot read symlink target - skip with I/O error.
                        eprintln!("symlink has no referent: {}", entry.path.display());
                        context.record_io_error();
                        keep_name = false;
                        action = EntryAction::SkipNonRegular;
                    }
                }
            }
        }

        #[cfg(unix)]
        if matches!(action, EntryAction::CopyDirectory)
            && context.one_file_system_enabled()
            && let Some(root) = root_device
            && let Some(entry_device) = prefetch.device_id
            && entry_device != root
        {
            action = EntryAction::SkipMountPoint;
        }

        #[cfg(not(unix))]
        if matches!(action, EntryAction::CopyDirectory)
            && context.one_file_system_enabled()
            && let Some(root) = root_device
            && let Some(entry_device) = crate::local_copy::overrides::device_identifier(
                entry.path.as_path(),
                metadata_override.as_ref().unwrap_or(entry_metadata),
            )
            && entry_device != root
        {
            action = EntryAction::SkipMountPoint;
        }

        if deletion_enabled && keep_name {
            let preserve_name = match delete_timing {
                Some(DeleteTiming::Before) => matches!(
                    action,
                    EntryAction::CopyDirectory
                        | EntryAction::SkipExcluded
                        | EntryAction::SkipMountPoint
                ),
                _ => true,
            };

            if preserve_name {
                // upstream: flist.c:1579-1603 + flist.c:738-754 - the
                // receiver hits the filesystem with the iconv-converted
                // name; align the keep-list so deletion does not wipe
                // freshly-written entries when --iconv is configured.
                keep_names.push(transcode_filename_component(
                    file_name.as_os_str(),
                    context.options().iconv(),
                ));
            }
        }

        planned_entries.push(PlannedEntry {
            entry,
            relative: relative_path,
            action,
            metadata_override,
        });
        if let Some(buf) = &mut relative_buf {
            buf.pop();
        }
    }

    Ok(DirectoryPlan {
        planned_entries,
        keep_names,
        deletion_enabled,
        delete_timing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_action_clone() {
        let action = EntryAction::CopyFile;
        let cloned = action;
        assert!(matches!(cloned, EntryAction::CopyFile));
    }

    #[test]
    fn entry_action_copy() {
        let action = EntryAction::CopyDirectory;
        let copied = action;
        assert!(matches!(action, EntryAction::CopyDirectory));
        assert!(matches!(copied, EntryAction::CopyDirectory));
    }

    #[test]
    fn entry_action_skip_excluded() {
        let action = EntryAction::SkipExcluded;
        assert!(matches!(action, EntryAction::SkipExcluded));
    }

    #[test]
    fn entry_action_skip_non_regular() {
        let action = EntryAction::SkipNonRegular;
        assert!(matches!(action, EntryAction::SkipNonRegular));
    }

    #[test]
    fn entry_action_skip_mount_point() {
        let action = EntryAction::SkipMountPoint;
        assert!(matches!(action, EntryAction::SkipMountPoint));
    }

    #[test]
    fn entry_action_copy_symlink() {
        let action = EntryAction::CopySymlink;
        assert!(matches!(action, EntryAction::CopySymlink));
    }

    #[test]
    fn entry_action_copy_fifo() {
        let action = EntryAction::CopyFifo;
        assert!(matches!(action, EntryAction::CopyFifo));
    }

    #[test]
    fn entry_action_copy_device() {
        let action = EntryAction::CopyDevice;
        assert!(matches!(action, EntryAction::CopyDevice));
    }

    #[test]
    fn entry_action_copy_device_as_file() {
        let action = EntryAction::CopyDeviceAsFile;
        assert!(matches!(action, EntryAction::CopyDeviceAsFile));
    }

    #[test]
    fn directory_plan_default_values() {
        let plan = DirectoryPlan {
            planned_entries: Vec::new(),
            keep_names: Vec::new(),
            deletion_enabled: false,
            delete_timing: None,
        };
        assert!(plan.planned_entries.is_empty());
        assert!(plan.keep_names.is_empty());
        assert!(!plan.deletion_enabled);
        assert!(plan.delete_timing.is_none());
    }

    #[test]
    fn directory_plan_deletion_enabled() {
        let names = [OsString::from("keep_me")];
        let plan = DirectoryPlan {
            planned_entries: Vec::new(),
            keep_names: names.iter().map(|n| Cow::Borrowed(n.as_os_str())).collect(),
            deletion_enabled: true,
            delete_timing: Some(DeleteTiming::Before),
        };
        assert!(plan.deletion_enabled);
        assert!(matches!(plan.delete_timing, Some(DeleteTiming::Before)));
        assert_eq!(plan.keep_names.len(), 1);
    }

    #[test]
    fn directory_plan_delete_timing_after() {
        let plan = DirectoryPlan {
            planned_entries: Vec::new(),
            keep_names: Vec::new(),
            deletion_enabled: true,
            delete_timing: Some(DeleteTiming::After),
        };
        assert!(matches!(plan.delete_timing, Some(DeleteTiming::After)));
    }

    #[test]
    fn directory_plan_multiple_keep_names() {
        let names = [
            OsString::from("file1.txt"),
            OsString::from("file2.txt"),
            OsString::from("dir"),
        ];
        let plan = DirectoryPlan {
            planned_entries: Vec::new(),
            keep_names: names.iter().map(|n| Cow::Borrowed(n.as_os_str())).collect(),
            deletion_enabled: true,
            delete_timing: Some(DeleteTiming::During),
        };
        assert_eq!(plan.keep_names.len(), 3);
    }
}
