//! Copy logic for FIFOs, devices, and symbolic links.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use crate::local_copy::{
    CopyContext, CreatedEntryKind, LocalCopyAction, LocalCopyArgumentError, LocalCopyError,
    LocalCopyMetadata, LocalCopyRecord, copy_directory_recursive, copy_file,
    follow_symlink_metadata, map_metadata_error, overrides::create_hard_link,
    remove_source_entry_if_requested,
};

#[cfg(feature = "acl")]
use crate::local_copy::sync_acls_if_requested;
#[cfg(feature = "xattr")]
use crate::local_copy::sync_xattrs_if_requested;

use rsync_meta::{
    MetadataOptions, apply_file_metadata_with_options, apply_symlink_metadata_with_options,
    create_device_node, create_fifo,
};

use super::{is_device, is_fifo};
use crate::local_copy::remove_existing_destination;

pub(crate) fn copy_fifo(
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
    #[cfg(feature = "xattr")]
    let preserve_xattrs = context.xattrs_enabled();
    #[cfg(feature = "acl")]
    let preserve_acls = context.acls_enabled();
    let record_path = relative
        .map(Path::to_path_buf)
        .or_else(|| destination.file_name().map(PathBuf::from));
    context.summary_mut().record_fifo_total();
    let mut existing_hard_link_target = context.existing_hard_link_target(metadata);
    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() {
            if mode.is_dry_run() {
                match fs::symlink_metadata(parent) {
                    Ok(existing) if !existing.file_type().is_dir() => {
                        return Err(LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                        ));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(LocalCopyError::io(
                            "inspect existing destination",
                            parent.to_path_buf(),
                            error,
                        ));
                    }
                }
            } else {
                fs::create_dir_all(parent).map_err(|error| {
                    LocalCopyError::io("create parent directory", parent.to_path_buf(), error)
                })?;
                context.register_progress();
            }
        }
    }

    if mode.is_dry_run() {
        match fs::symlink_metadata(destination) {
            Ok(existing) => {
                if existing.file_type().is_dir() {
                    return Err(LocalCopyError::invalid_argument(
                        LocalCopyArgumentError::ReplaceDirectoryWithSpecial,
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(LocalCopyError::io(
                    "inspect existing destination",
                    destination.to_path_buf(),
                    error,
                ));
            }
        }

        if existing_hard_link_target.is_some() {
            context.summary_mut().record_hard_link();
        } else {
            context.summary_mut().record_fifo();
        }
        if let Some(path) = &record_path {
            let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
            let total_bytes = Some(metadata_snapshot.len());
            let action = if existing_hard_link_target.is_some() {
                LocalCopyAction::HardLink
            } else {
                LocalCopyAction::FifoCopied
            };
            context.record(LocalCopyRecord::new(
                path.clone(),
                action,
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

    let mut destination_previously_existed = false;
    match fs::symlink_metadata(destination) {
        Ok(existing) => {
            destination_previously_existed = true;
            if existing.file_type().is_dir() {
                return Err(LocalCopyError::invalid_argument(
                    LocalCopyArgumentError::ReplaceDirectoryWithSpecial,
                ));
            }

            context.backup_existing_entry(destination, relative, existing.file_type())?;
            remove_existing_destination(destination)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(LocalCopyError::io(
                "inspect existing destination",
                destination.to_path_buf(),
                error,
            ));
        }
    }

    if let Some(link_source) = existing_hard_link_target.take() {
        match create_hard_link(&link_source, destination) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                remove_existing_destination(destination)?;
                create_hard_link(&link_source, destination).map_err(|link_error| {
                    LocalCopyError::io("create hard link", destination.to_path_buf(), link_error)
                })?;
            }
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(code) if code == crate::local_copy::CROSS_DEVICE_ERROR_CODE
                ) =>
            {
                existing_hard_link_target = Some(link_source);
            }
            Err(error) => {
                return Err(LocalCopyError::io(
                    "create hard link",
                    destination.to_path_buf(),
                    error,
                ));
            }
        }

        if existing_hard_link_target.is_none() {
            apply_file_metadata_with_options(destination, metadata, metadata_options.clone())
                .map_err(map_metadata_error)?;
            #[cfg(feature = "xattr")]
            sync_xattrs_if_requested(
                preserve_xattrs,
                mode,
                source,
                destination,
                true,
                context.filter_program(),
            )?;
            #[cfg(feature = "acl")]
            sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;
            context.record_hard_link(metadata, destination);
            context.summary_mut().record_hard_link();
            if let Some(path) = &record_path {
                let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
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
    }

    create_fifo(destination, metadata).map_err(map_metadata_error)?;
    context.register_created_path(
        destination,
        CreatedEntryKind::Fifo,
        destination_previously_existed,
    );
    apply_file_metadata_with_options(destination, metadata, metadata_options.clone())
        .map_err(map_metadata_error)?;
    #[cfg(feature = "xattr")]
    sync_xattrs_if_requested(
        preserve_xattrs,
        mode,
        source,
        destination,
        true,
        context.filter_program(),
    )?;
    #[cfg(feature = "acl")]
    sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;
    context.record_hard_link(metadata, destination);
    context.summary_mut().record_fifo();
    if let Some(path) = &record_path {
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
        let total_bytes = Some(metadata_snapshot.len());
        context.record(LocalCopyRecord::new(
            path.clone(),
            LocalCopyAction::FifoCopied,
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

pub(crate) fn copy_device(
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
    #[cfg(feature = "xattr")]
    let preserve_xattrs = context.xattrs_enabled();
    #[cfg(feature = "acl")]
    let preserve_acls = context.acls_enabled();
    let record_path = relative
        .map(Path::to_path_buf)
        .or_else(|| destination.file_name().map(PathBuf::from));
    context.summary_mut().record_device_total();
    let mut existing_hard_link_target = context.existing_hard_link_target(metadata);
    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() {
            if mode.is_dry_run() {
                match fs::symlink_metadata(parent) {
                    Ok(existing) if !existing.file_type().is_dir() => {
                        return Err(LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                        ));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(LocalCopyError::io(
                            "inspect existing destination",
                            parent.to_path_buf(),
                            error,
                        ));
                    }
                }
            } else {
                fs::create_dir_all(parent).map_err(|error| {
                    LocalCopyError::io("create parent directory", parent.to_path_buf(), error)
                })?;
                context.register_progress();
            }
        }
    }

    if mode.is_dry_run() {
        match fs::symlink_metadata(destination) {
            Ok(existing) => {
                if existing.file_type().is_dir() {
                    return Err(LocalCopyError::invalid_argument(
                        LocalCopyArgumentError::ReplaceDirectoryWithSpecial,
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(LocalCopyError::io(
                    "inspect existing destination",
                    destination.to_path_buf(),
                    error,
                ));
            }
        }

        if existing_hard_link_target.is_some() {
            context.summary_mut().record_hard_link();
        } else {
            context.summary_mut().record_device();
        }
        if let Some(path) = &record_path {
            let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
            let total_bytes = Some(metadata_snapshot.len());
            let action = if existing_hard_link_target.is_some() {
                LocalCopyAction::HardLink
            } else {
                LocalCopyAction::DeviceCopied
            };
            context.record(LocalCopyRecord::new(
                path.clone(),
                action,
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

    let mut destination_previously_existed = false;
    match fs::symlink_metadata(destination) {
        Ok(existing) => {
            destination_previously_existed = true;
            if existing.file_type().is_dir() {
                return Err(LocalCopyError::invalid_argument(
                    LocalCopyArgumentError::ReplaceDirectoryWithSpecial,
                ));
            }

            context.backup_existing_entry(destination, relative, existing.file_type())?;
            remove_existing_destination(destination)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(LocalCopyError::io(
                "inspect existing destination",
                destination.to_path_buf(),
                error,
            ));
        }
    }

    if let Some(link_source) = existing_hard_link_target.take() {
        match create_hard_link(&link_source, destination) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                remove_existing_destination(destination)?;
                create_hard_link(&link_source, destination).map_err(|link_error| {
                    LocalCopyError::io("create hard link", destination.to_path_buf(), link_error)
                })?;
            }
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(code) if code == crate::local_copy::CROSS_DEVICE_ERROR_CODE
                ) =>
            {
                existing_hard_link_target = Some(link_source);
            }
            Err(error) => {
                return Err(LocalCopyError::io(
                    "create hard link",
                    destination.to_path_buf(),
                    error,
                ));
            }
        }

        if existing_hard_link_target.is_none() {
            apply_file_metadata_with_options(destination, metadata, metadata_options.clone())
                .map_err(map_metadata_error)?;
            #[cfg(feature = "xattr")]
            sync_xattrs_if_requested(
                preserve_xattrs,
                mode,
                source,
                destination,
                true,
                context.filter_program(),
            )?;
            #[cfg(feature = "acl")]
            sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;
            context.record_hard_link(metadata, destination);
            context.summary_mut().record_hard_link();
            if let Some(path) = &record_path {
                let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
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
    }

    create_device_node(destination, metadata).map_err(map_metadata_error)?;
    context.register_created_path(
        destination,
        CreatedEntryKind::Device,
        destination_previously_existed,
    );
    apply_file_metadata_with_options(destination, metadata, metadata_options.clone())
        .map_err(map_metadata_error)?;
    #[cfg(feature = "xattr")]
    sync_xattrs_if_requested(
        preserve_xattrs,
        mode,
        source,
        destination,
        true,
        context.filter_program(),
    )?;
    #[cfg(feature = "acl")]
    sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;
    context.record_hard_link(metadata, destination);
    context.summary_mut().record_device();
    if let Some(path) = &record_path {
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
        let total_bytes = Some(metadata_snapshot.len());
        context.record(LocalCopyRecord::new(
            path.clone(),
            LocalCopyAction::DeviceCopied,
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
    #[cfg(feature = "xattr")]
    let preserve_xattrs = context.xattrs_enabled();
    #[cfg(feature = "acl")]
    let preserve_acls = context.acls_enabled();
    let record_path = relative
        .map(Path::to_path_buf)
        .or_else(|| destination.file_name().map(PathBuf::from));
    context.summary_mut().record_symlink_total();
    let target = fs::read_link(source)
        .map_err(|error| LocalCopyError::io("read symbolic link", source.to_path_buf(), error))?;

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

            if is_fifo(&target_type) {
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

            if is_device(&target_type) {
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

        context.record_skipped_unsafe_symlink(record_path.as_deref(), metadata, target);
        context.register_progress();
        return Ok(());
    }

    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() {
            if mode.is_dry_run() {
                match fs::symlink_metadata(parent) {
                    Ok(existing) if !existing.file_type().is_dir() => {
                        return Err(LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                        ));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(LocalCopyError::io(
                            "inspect existing destination",
                            parent.to_path_buf(),
                            error,
                        ));
                    }
                }
            } else {
                fs::create_dir_all(parent).map_err(|error| {
                    LocalCopyError::io("create parent directory", parent.to_path_buf(), error)
                })?;
                context.register_progress();
            }
        }
    }

    let mut destination_previously_existed = false;
    match fs::symlink_metadata(destination) {
        Ok(existing) => {
            destination_previously_existed = true;
            let file_type = existing.file_type();
            if file_type.is_dir() {
                return Err(LocalCopyError::invalid_argument(
                    LocalCopyArgumentError::ReplaceDirectoryWithSymlink,
                ));
            }

            if !mode.is_dry_run() {
                context.backup_existing_entry(destination, relative, file_type)?;
                remove_existing_destination(destination)?;
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(LocalCopyError::io(
                "inspect existing destination",
                destination.to_path_buf(),
                error,
            ));
        }
    }

    if let Some(existing_target) = context.existing_hard_link_target(metadata) {
        if mode.is_dry_run() {
            context.summary_mut().record_symlink();
            context.summary_mut().record_hard_link();
            if let Some(path) = &record_path {
                let metadata_snapshot =
                    LocalCopyMetadata::from_metadata(metadata, Some(target.clone()));
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
                LocalCopyMetadata::from_metadata(metadata, Some(target.clone()));
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

    if mode.is_dry_run() {
        context.summary_mut().record_symlink();
        if let Some(path) = &record_path {
            let metadata_snapshot =
                LocalCopyMetadata::from_metadata(metadata, Some(target.clone()));
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
    apply_symlink_metadata_with_options(destination, metadata, symlink_options)
        .map_err(map_metadata_error)?;
    #[cfg(feature = "xattr")]
    sync_xattrs_if_requested(
        preserve_xattrs,
        mode,
        source,
        destination,
        false,
        context.filter_program(),
    )?;
    #[cfg(feature = "acl")]
    sync_acls_if_requested(preserve_acls, mode, source, destination, false)?;

    context.record_hard_link(metadata, destination);
    context.summary_mut().record_symlink();
    if let Some(path) = &record_path {
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, Some(target.clone()));
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
