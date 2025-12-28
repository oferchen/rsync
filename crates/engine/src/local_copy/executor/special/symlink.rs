use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use crate::local_copy::remove_existing_destination;
#[cfg(all(unix, feature = "acl"))]
use crate::local_copy::sync_acls_if_requested;
#[cfg(all(unix, feature = "xattr"))]
use crate::local_copy::sync_xattrs_if_requested;
use crate::local_copy::{
    CopyContext, CreatedEntryKind, LocalCopyAction, LocalCopyArgumentError, LocalCopyError,
    LocalCopyMetadata, LocalCopyRecord, copy_directory_recursive, copy_file,
    follow_symlink_metadata, map_metadata_error, overrides::create_hard_link,
    remove_source_entry_if_requested,
};
use ::metadata::{MetadataOptions, apply_symlink_metadata_with_options};

use super::super::{is_device, is_fifo};
use super::{device::copy_device, fifo::copy_fifo};

pub(crate) fn symlink_target_is_safe(target: &Path, link_relative: &Path) -> bool {
    if target.as_os_str().is_empty() || target.has_root() {
        return false;
    }

    let mut seen_non_parent = false;
    let mut last_was_parent = false;
    let mut component_count = 0usize;

    for component in target.components() {
        match component {
            Component::ParentDir => {
                if seen_non_parent {
                    return false;
                }
                last_was_parent = true;
            }
            Component::CurDir => {
                seen_non_parent = true;
                last_was_parent = false;
            }
            Component::Normal(_) => {
                seen_non_parent = true;
                last_was_parent = false;
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
        component_count = component_count.saturating_add(1);
    }

    if component_count > 1 && last_was_parent {
        return false;
    }

    let mut depth: i64 = 0;
    for component in link_relative.components() {
        match component {
            Component::ParentDir => depth = 0,
            Component::CurDir => {}
            Component::Normal(_) => depth += 1,
            Component::RootDir | Component::Prefix(_) => depth = 0,
        }
    }

    for component in target.components() {
        match component {
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            Component::CurDir => {}
            Component::Normal(_) => depth += 1,
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }

    true
}

pub(crate) fn copy_symlink(
    context: &mut CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    metadata_options: &MetadataOptions,
    relative: Option<&Path>,
) -> Result<(), LocalCopyError> {
    context.enforce_timeout()?;
    let mode = context.mode();
    let file_type = metadata.file_type();

    #[cfg(all(unix, feature = "xattr"))]
    let preserve_xattrs = context.xattrs_enabled();
    #[cfg(all(unix, feature = "acl"))]
    let preserve_acls = context.acls_enabled();

    #[cfg(not(all(unix, feature = "xattr")))]
    let _ = context;
    #[cfg(not(all(unix, feature = "acl")))]
    let _ = mode;

    let record_path = relative
        .map(Path::to_path_buf)
        .or_else(|| destination.file_name().map(PathBuf::from));
    context.summary_mut().record_symlink_total();

    let target = fs::read_link(source)
        .map_err(|error| LocalCopyError::io("read symbolic link", source.to_path_buf(), error))?;

    let mut destination_metadata = match fs::symlink_metadata(destination) {
        Ok(existing) => Some(existing),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(LocalCopyError::io(
                "inspect existing destination",
                destination.to_path_buf(),
                error,
            ));
        }
    };

    let destination_previously_existed = destination_metadata.is_some();

    if let Some(existing) = destination_metadata.as_ref()
        && existing.file_type().is_dir()
    {
        if context.force_replacements_enabled() {
            context.force_remove_destination(destination, relative, existing)?;
            destination_metadata = None;
        } else {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::ReplaceDirectoryWithSymlink,
            ));
        }
    }

    // --existing handling
    if context.existing_only_enabled() && destination_metadata.is_none() {
        if let Some(relative_path) = record_path.as_ref() {
            let metadata_snapshot =
                LocalCopyMetadata::from_metadata(metadata, Some(target));
            context.record(LocalCopyRecord::new(
                relative_path.clone(),
                LocalCopyAction::SkippedMissingDestination,
                0,
                Some(metadata_snapshot.len()),
                Duration::default(),
                Some(metadata_snapshot),
            ));
        }
        return Ok(());
    }

    // build a "relative" path to check symlink safety
    let safety_relative = relative
        .map(Path::to_path_buf)
        .or_else(|| {
            destination
                .strip_prefix(context.destination_root())
                .ok()
                .and_then(|path| (!path.as_os_str().is_empty()).then(|| path.to_path_buf()))
        })
        .or_else(|| destination.file_name().map(PathBuf::from))
        .unwrap_or_else(|| destination.to_path_buf());

    let unsafe_target =
        context.safe_links_enabled() && !symlink_target_is_safe(&target, &safety_relative);

    // If the link is unsafe but we were told to copy what it points to, do that.
    if unsafe_target {
        if context.copy_unsafe_links_enabled() {
            let target_metadata = follow_symlink_metadata(source)?;
            let target_type = target_metadata.file_type();

            if target_type.is_dir() {
                let _kept = copy_directory_recursive(
                    context,
                    source,
                    destination,
                    &target_metadata,
                    relative,
                    None,
                )?;
                return Ok(());
            }

            if target_type.is_file() {
                copy_file(context, source, destination, &target_metadata, relative)?;
                return Ok(());
            }

            if is_fifo(target_type) {
                if !context.specials_enabled() {
                    context.record_skipped_non_regular(record_path.as_deref());
                    context.register_progress();
                    return Ok(());
                }
                copy_fifo(
                    context,
                    source,
                    destination,
                    &target_metadata,
                    metadata_options,
                    relative,
                )?;
                return Ok(());
            }

            if is_device(target_type) {
                if !context.devices_enabled() {
                    context.record_skipped_non_regular(record_path.as_deref());
                    context.register_progress();
                    return Ok(());
                }
                copy_device(
                    context,
                    source,
                    destination,
                    &target_metadata,
                    metadata_options,
                    relative,
                )?;
                return Ok(());
            }

            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::UnsupportedFileType,
            ));
        }

        // otherwise we just record that we skipped it
        context.record_skipped_unsafe_symlink(record_path.as_deref(), metadata, target);
        context.register_progress();
        return Ok(());
    }

    if let Some(parent) = destination.parent() {
        context.prepare_parent_directory(parent)?;
    }

    if !mode.is_dry_run()
        && let Some(existing) = destination_metadata.take()
    {
        context.backup_existing_entry(destination, relative, existing.file_type())?;
        remove_existing_destination(destination)?;
    }

    // dedupe via hard links if we saw an identical symlink before
    if let Some(existing_target) = context.existing_hard_link_target(metadata) {
        if mode.is_dry_run() {
            context.summary_mut().record_symlink();
            context.summary_mut().record_hard_link();
            if let Some(path) = &record_path {
                let metadata_snapshot =
                    LocalCopyMetadata::from_metadata(metadata, Some(target));
                let total_bytes = Some(metadata_snapshot.len());
                context.record(LocalCopyRecord::new(
                    path.clone(),
                    LocalCopyAction::HardLink,
                    0,
                    total_bytes,
                    Duration::default(),
                    Some(metadata_snapshot),
                ));
            }
            context.register_progress();
            remove_source_entry_if_requested(context, source, record_path.as_deref(), file_type)?;
            return Ok(());
        }

        let mut attempted_commit = false;
        loop {
            match create_hard_link(&existing_target, destination) {
                Ok(()) => break,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    remove_existing_destination(destination)?;
                    create_hard_link(&existing_target, destination).map_err(|link_error| {
                        LocalCopyError::io(
                            "create hard link",
                            destination.to_path_buf(),
                            link_error,
                        )
                    })?;
                    break;
                }
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound
                        && context.delay_updates_enabled()
                        && !attempted_commit =>
                {
                    context.commit_deferred_update_for(&existing_target)?;
                    attempted_commit = true;
                    continue;
                }
                Err(error) => {
                    return Err(LocalCopyError::io(
                        "create hard link",
                        destination.to_path_buf(),
                        error,
                    ));
                }
            }
        }

        context.record_hard_link(metadata, destination);
        context.summary_mut().record_hard_link();
        context.summary_mut().record_symlink();
        if let Some(path) = &record_path {
            let metadata_snapshot =
                LocalCopyMetadata::from_metadata(metadata, Some(target));
            let total_bytes = Some(metadata_snapshot.len());
            context.record(LocalCopyRecord::new(
                path.clone(),
                LocalCopyAction::HardLink,
                0,
                total_bytes,
                Duration::default(),
                Some(metadata_snapshot),
            ));
        }
        context.register_created_path(
            destination,
            CreatedEntryKind::HardLink,
            destination_previously_existed,
        );
        context.register_progress();
        remove_source_entry_if_requested(context, source, record_path.as_deref(), file_type)?;
        return Ok(());
    }

    // dry-run: just record
    if mode.is_dry_run() {
        context.summary_mut().record_symlink();
        if let Some(path) = &record_path {
            let metadata_snapshot =
                LocalCopyMetadata::from_metadata(metadata, Some(target));
            let total_bytes = Some(metadata_snapshot.len());
            context.record(LocalCopyRecord::new(
                path.clone(),
                LocalCopyAction::SymlinkCopied,
                0,
                total_bytes,
                Duration::default(),
                Some(metadata_snapshot),
            ));
        }
        context.register_progress();
        remove_source_entry_if_requested(context, source, record_path.as_deref(), file_type)?;
        return Ok(());
    }

    // actually create it
    create_symlink(&target, source, destination).map_err(|error| {
        LocalCopyError::io("create symbolic link", destination.to_path_buf(), error)
    })?;

    context.register_created_path(
        destination,
        CreatedEntryKind::Symlink,
        destination_previously_existed,
    );

    let symlink_options = if context.omit_link_times_enabled() {
        metadata_options.clone().preserve_times(false)
    } else {
        metadata_options.clone()
    };
    apply_symlink_metadata_with_options(destination, metadata, &symlink_options)
        .map_err(map_metadata_error)?;

    #[cfg(all(unix, feature = "xattr"))]
    sync_xattrs_if_requested(
        preserve_xattrs,
        mode,
        source,
        destination,
        false,
        context.filter_program(),
    )?;
    #[cfg(all(unix, feature = "acl"))]
    sync_acls_if_requested(preserve_acls, mode, source, destination, false)?;

    context.record_hard_link(metadata, destination);
    context.summary_mut().record_symlink();
    if let Some(path) = &record_path {
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, Some(target));
        let total_bytes = Some(metadata_snapshot.len());
        context.record(LocalCopyRecord::new(
            path.clone(),
            LocalCopyAction::SymlinkCopied,
            0,
            total_bytes,
            Duration::default(),
            Some(metadata_snapshot),
        ));
    }
    context.register_progress();
    remove_source_entry_if_requested(context, source, record_path.as_deref(), file_type)?;
    Ok(())
}

#[cfg(unix)]
pub(crate) fn create_symlink(target: &Path, _source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::unix::fs::symlink;
    symlink(target, destination)
}

#[cfg(windows)]
pub(crate) fn create_symlink(target: &Path, source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::fs::{symlink_dir, symlink_file};

    match source.metadata() {
        Ok(metadata) if metadata.file_type().is_dir() => symlink_dir(target, destination),
        Ok(_) => symlink_file(target, destination),
        Err(_) => symlink_file(target, destination),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== symlink_target_is_safe tests ====================

    #[test]
    fn safe_simple_relative_target() {
        // Simple relative path is safe
        assert!(symlink_target_is_safe(
            Path::new("file.txt"),
            Path::new("link")
        ));
    }

    #[test]
    fn safe_nested_relative_target() {
        // Nested relative path within the tree
        assert!(symlink_target_is_safe(
            Path::new("subdir/file.txt"),
            Path::new("link")
        ));
    }

    #[test]
    fn safe_parent_then_sibling() {
        // Going up one level and into sibling is safe if link is deep enough
        assert!(symlink_target_is_safe(
            Path::new("../sibling/file.txt"),
            Path::new("dir/link")
        ));
    }

    #[test]
    fn unsafe_absolute_path() {
        // Absolute paths are never safe
        assert!(!symlink_target_is_safe(
            Path::new("/etc/passwd"),
            Path::new("link")
        ));
    }

    #[test]
    fn unsafe_empty_target() {
        // Empty target is unsafe
        assert!(!symlink_target_is_safe(Path::new(""), Path::new("link")));
    }

    #[test]
    fn unsafe_escapes_root() {
        // Link at root level, target goes up - escapes
        // "link" has depth 1, so 2 parent dirs escapes
        assert!(!symlink_target_is_safe(
            Path::new("../../outside"),
            Path::new("link")
        ));
    }

    #[test]
    fn unsafe_escapes_with_multiple_parents() {
        // More parent components than depth allows
        // dir/link has depth 2, so 3 parent dirs escapes
        assert!(!symlink_target_is_safe(
            Path::new("../../../outside"),
            Path::new("dir/link")
        ));
    }

    #[test]
    fn safe_same_level_parent() {
        // Going up one level is safe if we're one level deep
        assert!(symlink_target_is_safe(
            Path::new("../file.txt"),
            Path::new("dir/link")
        ));
    }

    #[test]
    fn safe_current_dir_prefix() {
        // Current dir prefix is fine
        assert!(symlink_target_is_safe(
            Path::new("./file.txt"),
            Path::new("link")
        ));
    }

    #[test]
    fn unsafe_parent_after_normal() {
        // Parent after normal component is unsafe (trying to escape)
        assert!(!symlink_target_is_safe(
            Path::new("subdir/../.."),
            Path::new("link")
        ));
    }

    #[test]
    fn unsafe_ends_with_parent() {
        // Multiple components ending with parent is suspicious
        assert!(!symlink_target_is_safe(
            Path::new("subdir/.."),
            Path::new("link")
        ));
    }

    #[test]
    fn safe_deep_link_shallow_escape() {
        // Deep link can go up multiple levels safely
        assert!(symlink_target_is_safe(
            Path::new("../../file.txt"),
            Path::new("a/b/c/link")
        ));
    }

    #[test]
    fn unsafe_deep_escape() {
        // Even deep links can't escape past root
        // link at a/b/c/link has depth 4 (includes link name)
        // So 5 parent dirs would be needed to escape
        assert!(!symlink_target_is_safe(
            Path::new("../../../../../outside"),
            Path::new("a/b/c/link")
        ));
    }

    #[test]
    fn safe_dot_only() {
        // Just current directory
        assert!(symlink_target_is_safe(Path::new("."), Path::new("link")));
    }

    #[test]
    fn safe_complex_but_valid_path() {
        // Complex but valid relative path
        assert!(symlink_target_is_safe(
            Path::new("./subdir/./nested/file.txt"),
            Path::new("link")
        ));
    }

    #[test]
    fn unsafe_root_component() {
        // Root component in target is unsafe
        assert!(!symlink_target_is_safe(
            Path::new("/absolute/path"),
            Path::new("deep/link")
        ));
    }

    #[test]
    fn safe_link_in_subdir_target_in_same() {
        // Link in subdir, target in same subdir
        assert!(symlink_target_is_safe(
            Path::new("sibling.txt"),
            Path::new("subdir/link")
        ));
    }

    #[test]
    fn safe_exactly_at_boundary() {
        // Going up exactly as many levels as depth allows
        // link at a/b/c/link has depth 4, so 4 parent dirs is exactly at boundary
        assert!(symlink_target_is_safe(
            Path::new("../../../../file.txt"),
            Path::new("a/b/c/link")
        ));
    }

    #[test]
    fn unsafe_one_past_boundary() {
        // Going up one more level than allowed
        // link at a/b/c/link has depth 4, so 5 parent dirs escapes
        assert!(!symlink_target_is_safe(
            Path::new("../../../../../file.txt"),
            Path::new("a/b/c/link")
        ));
    }
}
