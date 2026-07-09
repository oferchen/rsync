//! FIFO (named pipe) copy with hard-link deduplication.
//!
//! Recreates FIFOs at the destination using `mkfifo(3)`, with optional
//! hard-link deduplication to earlier FIFOs.
//!
//! upstream: receiver.c - FIFO handling, syscall.c:do_mknod()

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::link_special_from_link_dest;
use crate::local_copy::remove_existing_destination;
#[cfg(all(any(unix, windows), feature = "acl"))]
use crate::local_copy::sync_acls_if_requested;
#[cfg(all(unix, feature = "xattr"))]
use crate::local_copy::sync_xattrs_if_requested;
use crate::local_copy::{
    CopyContext, CreatedEntryKind, LocalCopyAction, LocalCopyArgumentError, LocalCopyChangeSet,
    LocalCopyError, LocalCopyMetadata, LocalCopyRecord, map_metadata_error,
    overrides::create_hard_link, remove_source_entry_if_requested,
};
#[cfg(unix)]
use ::metadata::create_fifo_with_fake_super;
use ::metadata::{
    MetadataOptions, apply_file_metadata_if_changed, apply_file_metadata_with_options,
};

/// Copies a FIFO (named pipe) from source to destination.
///
/// Handles hard-link deduplication, `--existing`, directory replacement via
/// `--force`, backup, and dry-run mode.
// upstream: receiver.c - FIFO handling
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

    #[cfg(all(unix, feature = "xattr"))]
    let preserve_xattrs = context.xattrs_enabled();
    #[cfg(all(any(unix, windows), feature = "acl"))]
    let preserve_acls = context.acls_enabled();

    #[cfg(not(all(unix, feature = "xattr")))]
    let _ = context;
    #[cfg(not(all(any(unix, windows), feature = "acl")))]
    let _ = mode;

    // upstream: generator.c:558-563 / 550-556 - a replace itemize reports
    // ITEM_REPORT_XATTR / ITEM_REPORT_ACL when those features are active and the
    // basis differs. Surface the enabled flags on all platforms so the recreate
    // change-set is derivable without cfg branching at the record site.
    #[cfg(all(unix, feature = "xattr"))]
    let itemize_xattrs = preserve_xattrs;
    #[cfg(not(all(unix, feature = "xattr")))]
    let itemize_xattrs = false;
    #[cfg(all(any(unix, windows), feature = "acl"))]
    let itemize_acls = preserve_acls;
    #[cfg(not(all(any(unix, windows), feature = "acl")))]
    let itemize_acls = false;

    let record_path = relative
        .map(Path::to_path_buf)
        .or_else(|| destination.file_name().map(PathBuf::from));

    context.summary_mut().record_fifo_total();

    let mut existing_metadata = match fs::symlink_metadata(destination) {
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

    let destination_previously_existed = existing_metadata.is_some();

    if let Some(existing) = existing_metadata.as_ref()
        && existing.file_type().is_dir()
    {
        if context.force_replacements_enabled() {
            context.force_remove_destination(destination, relative, existing)?;
            existing_metadata = None;
        } else {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::ReplaceDirectoryWithSpecial,
            ));
        }
    }

    if context.existing_only_enabled() && existing_metadata.is_none() {
        if let Some(path) = &record_path {
            let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
            context.record(LocalCopyRecord::new(
                path.clone(),
                LocalCopyAction::SkippedMissingDestination,
                0,
                Some(metadata_snapshot.len()),
                Duration::default(),
                Some(metadata_snapshot),
            ));
        }
        return Ok(());
    }

    // upstream: generator.c:1615-1630 - a special file (FIFO/socket) whose
    // destination already holds another special of the same FT_SPECIAL bucket is
    // recreated rather than treated as new; quick_check_ok() (generator.c:657-660)
    // compares the `_S_IFMT` bits (FIFO vs socket). Snapshot the existing special
    // and whether its type differs before removal so the itemize change-set can
    // render `cSc.T.` for a differing replace instead of `cS+++++++++` for a
    // create. A non-special existing entry matches upstream's statret = -1 path
    // and stays a fresh creation.
    #[cfg(unix)]
    let (replaced_special, replaced_content_differs) = {
        use std::os::unix::fs::FileTypeExt;
        let source_type = metadata.file_type();
        match existing_metadata.as_ref().filter(|existing| {
            let existing_type = existing.file_type();
            existing_type.is_fifo() || existing_type.is_socket()
        }) {
            Some(existing) => {
                let existing_type = existing.file_type();
                let differs = source_type.is_fifo() != existing_type.is_fifo()
                    || source_type.is_socket() != existing_type.is_socket();
                (Some(existing.clone()), differs)
            }
            None => (None, false),
        }
    };
    #[cfg(not(unix))]
    let (replaced_special, replaced_content_differs): (Option<fs::Metadata>, bool) = (None, false);

    // could be deduped to an earlier FIFO we created
    let mut existing_hard_link_target = context.existing_hard_link_target(metadata);

    if let Some(parent) = destination.parent() {
        context.prepare_parent_directory(parent)?;
    }

    if mode.is_dry_run() {
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
            let record = LocalCopyRecord::new(
                path.clone(),
                action.clone(),
                0,
                total_bytes,
                Duration::default(),
                Some(metadata_snapshot),
            );
            // upstream: generator.c:1462 itemize() sets ITEM_IS_NEW (statret < 0)
            // so log.c:736-738 fills slots 2-10 with `+` for a FIFO/socket the
            // receiver newly materialises. A special replacing a differing
            // special is itemized as a local change instead; the generator runs
            // itemize() even under --dry-run.
            let record = if matches!(action, LocalCopyAction::FifoCopied)
                && let Some(existing) = replaced_special.as_ref()
            {
                let change_set = LocalCopyChangeSet::for_recreated_device(
                    metadata,
                    existing,
                    metadata_options,
                    context.options().modify_window(),
                    replaced_content_differs,
                    itemize_xattrs,
                    itemize_acls,
                );
                record.with_creation(false).with_change_set(change_set)
            } else {
                record.with_creation(!destination_previously_existed)
            };
            context.record(record);
        }

        context.register_progress();
        remove_source_entry_if_requested(context, source, record_path.as_deref(), file_type)?;
        return Ok(());
    }

    if let Some(existing) = existing_metadata.take() {
        context.backup_existing_entry(destination, relative, existing.file_type())?;
        remove_existing_destination(destination)?;
    }

    // upstream: generator.c:1643-1658 try_dests_non() - a `--link-dest` basis
    // special matching the source (same FT_SPECIAL bucket, same `_S_IFMT`,
    // unchanged attrs) is hard-linked into place and itemized `hS` + blank
    // rather than recreated. Only applies when creating fresh.
    if !destination_previously_existed {
        let link_relative = relative
            .or(record_path.as_deref())
            .unwrap_or_else(|| Path::new(""));
        if !link_relative.as_os_str().is_empty()
            && let Some(basis) =
                context.link_dest_special_target(link_relative, metadata, metadata_options)?
        {
            link_special_from_link_dest(
                context,
                source,
                destination,
                metadata,
                &basis,
                record_path.as_deref(),
                file_type,
                destination_previously_existed,
                false,
            )?;
            return Ok(());
        }
    }

    // try to hard-link to an earlier FIFO we made
    if let Some(link_source) = existing_hard_link_target.take() {
        // upstream: log.c:643-654 - the `%L` field renders ` => leader` for a
        // hard-linked non-symlink. Capture the leader's destination-relative
        // path before the match may move `link_source` back on EXDEV so the
        // itemize row can emit `hS+++++++++ alias => leader`.
        let leader_display = link_source
            .strip_prefix(context.destination_root())
            .ok()
            .map(std::path::Path::to_path_buf);
        match create_hard_link(&link_source, destination) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                remove_existing_destination(destination)?;
                create_hard_link(&link_source, destination).map_err(|link_error| {
                    LocalCopyError::io("create hard link", destination, link_error)
                })?;
            }
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(code) if code == crate::local_copy::CROSS_DEVICE_ERROR_CODE
                ) =>
            {
                // degrade to "recreate fifo" below
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
            apply_file_metadata_with_options(destination, metadata, metadata_options)
                .map_err(map_metadata_error)?;
            #[cfg(all(unix, feature = "xattr"))]
            sync_xattrs_if_requested(
                preserve_xattrs,
                mode,
                source,
                destination,
                true,
                context.filter_program(),
            )?;
            #[cfg(all(any(unix, windows), feature = "acl"))]
            sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;

            context.record_hard_link(metadata, destination);
            context.summary_mut().record_hard_link();

            if let Some(path) = &record_path {
                let leader_display =
                    leader_display.filter(|relative| relative.as_path() != path.as_path());
                let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, leader_display);
                let total_bytes = Some(metadata_snapshot.len());
                context.record(
                    LocalCopyRecord::new(
                        path.clone(),
                        LocalCopyAction::HardLink,
                        0,
                        total_bytes,
                        Duration::default(),
                        Some(metadata_snapshot),
                    )
                    // upstream: hlink.c:218-222 itemize(..., ITEM_LOCAL_CHANGE, ...)
                    // ORs in ITEM_IS_NEW when the destination did not exist, so a
                    // freshly hard-linked FIFO alias renders `hS+++++++++`.
                    .with_creation(!destination_previously_existed),
                );
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

    // actually create a FIFO, or a 0600 placeholder when --fake-super is
    // active (mirrors upstream syscall.c:do_mknod()'s am_root < 0 branch).
    #[cfg(unix)]
    {
        let fake_super = metadata_options.fake_super_enabled();
        create_fifo_with_fake_super(destination, metadata, fake_super)
            .map_err(map_metadata_error)?;
    }
    #[cfg(not(unix))]
    {
        // Windows / non-Unix: no FIFO support in this crate path.
        // Create an empty file so the caller sees a tangible result and the rest of the
        // metadata application doesn’t fail.
        fs::File::create(destination)
            .map_err(|error| LocalCopyError::io("create fifo placeholder", destination, error))?;
    }

    context.register_created_path(
        destination,
        CreatedEntryKind::Fifo,
        destination_previously_existed,
    );

    // upstream: rsync.c:set_file_attrs() chmods only when the freshly created
    // node's permission bits differ from the source (`!BITS_EQUAL`). do_mknod()
    // already created the FIFO with the source mode, so the perms match and the
    // chmod is skipped. Skipping it is not merely an optimisation: under
    // fakeroot a `mknod(S_IFIFO)` node is a regular inode carrying a faked type,
    // and any chmod - even one masked to the permission bits - rewrites that
    // faked type back to a regular file. Comparing against the just-created node
    // (rather than an unconditional apply) mirrors upstream and preserves the
    // FIFO type.
    if let Ok(created_metadata) = fs::symlink_metadata(destination) {
        apply_file_metadata_if_changed(destination, metadata, &created_metadata, metadata_options)
            .map_err(map_metadata_error)?;
    } else {
        apply_file_metadata_with_options(destination, metadata, metadata_options)
            .map_err(map_metadata_error)?;
    }

    #[cfg(all(unix, feature = "xattr"))]
    sync_xattrs_if_requested(
        preserve_xattrs,
        mode,
        source,
        destination,
        true,
        context.filter_program(),
    )?;
    #[cfg(all(any(unix, windows), feature = "acl"))]
    sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;

    // Under fake-super, capture the would-be FIFO/socket's mode/uid/gid in
    // the rsync.%stat xattr so the destination can be restored later.
    // upstream: xattrs.c:set_stat_xattr() under am_root < 0
    #[cfg(all(unix, feature = "xattr"))]
    if metadata_options.fake_super_enabled() {
        store_fake_super_for_local_metadata(destination, metadata)?;
    }

    context.record_hard_link(metadata, destination);
    context.summary_mut().record_fifo();

    if let Some(path) = &record_path {
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
        let total_bytes = Some(metadata_snapshot.len());
        let record = LocalCopyRecord::new(
            path.clone(),
            LocalCopyAction::FifoCopied,
            0,
            total_bytes,
            Duration::default(),
            Some(metadata_snapshot),
        );
        let record = if let Some(existing) = replaced_special.as_ref() {
            // upstream: generator.c:1665-1669 - a special replacing a differing
            // special of the same FT_SPECIAL bucket recreates it and itemizes via
            // ITEM_LOCAL_CHANGE|ITEM_REPORT_CHANGE, not ITEM_IS_NEW. Derive the
            // change-set from the `_S_IFMT` comparison.
            let change_set = LocalCopyChangeSet::for_recreated_device(
                metadata,
                existing,
                metadata_options,
                context.options().modify_window(),
                replaced_content_differs,
                itemize_xattrs,
                itemize_acls,
            );
            record.with_creation(false).with_change_set(change_set)
        } else {
            // upstream: generator.c:1462 itemize() sets ITEM_IS_NEW (statret < 0)
            // so log.c:736-738 fills slots 2-10 with `+` for a FIFO/socket the
            // receiver newly materialises via do_mknod(). A non-special existing
            // entry hits upstream's statret = -1 path and is likewise new.
            record.with_creation(true)
        };
        context.record(record);
    }

    context.register_progress();
    remove_source_entry_if_requested(context, source, record_path.as_deref(), file_type)?;
    Ok(())
}

/// Stores the would-be FIFO/socket metadata in the `rsync.%stat` xattr.
///
/// Encodes mode (with `S_IFMT` bits), uid, and gid so a later fake-super
/// read can faithfully reconstruct the original node.
// upstream: xattrs.c:set_stat_xattr()
#[cfg(all(unix, feature = "xattr"))]
fn store_fake_super_for_local_metadata(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), LocalCopyError> {
    use ::metadata::{FakeSuperStat, store_fake_super};

    let stat = FakeSuperStat::from_metadata(metadata);
    store_fake_super(destination, &stat).map_err(|error| {
        LocalCopyError::io(
            "store fake-super metadata",
            destination.to_path_buf(),
            error,
        )
    })
}
