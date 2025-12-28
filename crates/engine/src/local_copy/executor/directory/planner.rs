use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::local_copy::{
    CopyContext, DeleteTiming, LocalCopyArgumentError, LocalCopyError, delete_extraneous_entries,
    follow_symlink_metadata,
};

use super::super::{non_empty_path, symlink_target_is_safe};
use super::support::{DirectoryEntry, is_device, is_fifo};

#[derive(Clone, Copy)]
pub(crate) enum EntryAction {
    SkipExcluded,
    SkipNonRegular,
    SkipMountPoint,
    CopyDirectory,
    CopyFile,
    CopySymlink,
    CopyFifo,
    CopyDevice,
    CopyDeviceAsFile,
}

pub(crate) struct PlannedEntry<'a> {
    pub(crate) entry: &'a DirectoryEntry,
    pub(crate) relative: PathBuf,
    pub(crate) action: EntryAction,
    pub(crate) metadata_override: Option<fs::Metadata>,
}

impl<'a> PlannedEntry<'a> {
    pub(crate) fn metadata(&self) -> &fs::Metadata {
        self.metadata_override
            .as_ref()
            .unwrap_or(&self.entry.metadata)
    }
}

pub(crate) struct DirectoryPlan<'a> {
    pub(crate) planned_entries: Vec<PlannedEntry<'a>>,
    pub(crate) keep_names: Vec<OsString>,
    pub(crate) deletion_enabled: bool,
    pub(crate) delete_timing: Option<DeleteTiming>,
}

/// Centralized decision policy for how to treat a directory entry.
///
/// This encapsulates the "strategy" for turning the entry type + context
/// into an [`EntryAction`] and whether the name should be preserved for
/// deletion tracking.
fn decide_entry_action(
    context: &CopyContext,
    relative_path: &Path,
    entry_type: &fs::FileType,
    effective_type: &fs::FileType,
    keep_name: &mut bool,
) -> Result<EntryAction, LocalCopyError> {
    if !context.allows(relative_path, effective_type.is_dir()) {
        if context.options().delete_excluded_enabled() {
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

    if entry_type.is_symlink() {
        if context.links_enabled() {
            return Ok(EntryAction::CopySymlink);
        }
        *keep_name = false;
        return Ok(EntryAction::SkipNonRegular);
    }

    if is_fifo(effective_type) {
        if context.specials_enabled() {
            return Ok(EntryAction::CopyFifo);
        }
        *keep_name = false;
        return Ok(EntryAction::SkipNonRegular);
    }

    if is_device(effective_type) {
        if context.copy_devices_as_files_enabled() {
            return Ok(EntryAction::CopyDeviceAsFile);
        }
        if context.devices_enabled() {
            return Ok(EntryAction::CopyDevice);
        }
        *keep_name = false;
        return Ok(EntryAction::SkipNonRegular);
    }

    Err(LocalCopyError::invalid_argument(
        LocalCopyArgumentError::UnsupportedFileType,
    ))
}

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

    for entry in entries.iter() {
        context.enforce_timeout()?;
        context.register_progress();

        let file_name = entry.file_name.clone();
        let entry_metadata = &entry.metadata;
        let entry_type = entry_metadata.file_type();
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

        let relative_path = match relative {
            Some(base) => base.join(Path::new(&file_name)),
            None => PathBuf::from(Path::new(&file_name)),
        };
        context.record_file_list_entry(non_empty_path(relative_path.as_path()));

        let mut keep_name = true;
        let mut action = decide_entry_action(
            context,
            relative_path.as_path(),
            &entry_type,
            &effective_type,
            &mut keep_name,
        )?;

        if matches!(action, EntryAction::CopySymlink)
            && context.safe_links_enabled()
            && context.copy_unsafe_links_enabled()
        {
            match fs::read_link(entry.path.as_path()) {
                Ok(target) => {
                    if !symlink_target_is_safe(&target, relative_path.as_path()) {
                        match follow_symlink_metadata(entry.path.as_path()) {
                            Ok(target_metadata) => {
                                let target_type = target_metadata.file_type();
                                if target_type.is_dir() {
                                    action = EntryAction::CopyDirectory;
                                    metadata_override = Some(target_metadata);
                                } else if target_type.is_file() {
                                    action = EntryAction::CopyFile;
                                    metadata_override = Some(target_metadata);
                                } else if is_fifo(&target_type) {
                                    if context.specials_enabled() {
                                        action = EntryAction::CopyFifo;
                                        metadata_override = Some(target_metadata);
                                    } else {
                                        keep_name = false;
                                        action = EntryAction::SkipNonRegular;
                                        metadata_override = None;
                                    }
                                } else if is_device(&target_type) {
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
                            Err(error) => {
                                return Err(error);
                            }
                        }
                    }
                }
                Err(error) => {
                    return Err(LocalCopyError::io(
                        "read symbolic link",
                        entry.path.to_path_buf(),
                        error,
                    ));
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
                keep_names.push(file_name.clone());
            }
        }

        planned_entries.push(PlannedEntry {
            entry,
            relative: relative_path,
            action,
            metadata_override,
        });
    }

    Ok(DirectoryPlan {
        planned_entries,
        keep_names,
        deletion_enabled,
        delete_timing,
    })
}

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

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== EntryAction tests ====================

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
        // Original still usable
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

    // ==================== DirectoryPlan field tests ====================

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
        let plan = DirectoryPlan {
            planned_entries: Vec::new(),
            keep_names: vec![OsString::from("keep_me")],
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
        let plan = DirectoryPlan {
            planned_entries: Vec::new(),
            keep_names: vec![
                OsString::from("file1.txt"),
                OsString::from("file2.txt"),
                OsString::from("dir"),
            ],
            deletion_enabled: true,
            delete_timing: Some(DeleteTiming::During),
        };
        assert_eq!(plan.keep_names.len(), 3);
    }
}
