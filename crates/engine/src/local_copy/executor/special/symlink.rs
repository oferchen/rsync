//! Symbolic link copy with safe-links validation and munge support.
//!
//! Recreates symlinks at the destination, optionally munging unsafe targets
//! (absolute paths or `..` escapes) when `--safe-links` or `--munge-links`
//! is active.
//!
//! upstream: receiver.c - symlink handling, syscall.c:do_symlink()

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use logging::info_log;

use crate::local_copy::remove_existing_destination;
#[cfg(all(any(unix, windows), feature = "acl"))]
use crate::local_copy::sync_acls_if_requested;
#[cfg(all(unix, feature = "xattr"))]
use crate::local_copy::sync_xattrs_if_requested;
use crate::local_copy::{
    CopyContext, CreatedEntryKind, LocalCopyAction, LocalCopyArgumentError, LocalCopyChangeSet,
    LocalCopyError, LocalCopyMetadata, LocalCopyRecord, copy_directory_recursive, copy_file,
    follow_symlink_metadata, map_metadata_error, overrides::create_hard_link,
    remove_source_entry_if_requested,
};
use ::metadata::{MetadataOptions, apply_symlink_metadata_with_options};

use super::super::{is_device, is_fifo};
use super::{device::copy_device, fifo::copy_fifo};

/// Returns `true` when a symlink target stays within the transfer tree.
///
/// Rejects absolute paths, empty targets, and targets whose `..` components
/// would escape above the directory depth implied by `link_relative`.
// upstream: clientserver.c - safe symlink checking
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

    // The last component of link_relative is the symlink filename itself,
    // not a directory level.  Symlink targets resolve relative to the
    // *containing* directory, so exclude the filename from the depth budget.
    depth = (depth - 1).max(0);

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

/// Copies a symbolic link from source to destination.
///
/// Handles `--safe-links`, `--copy-unsafe-links`, `--munge-links`, hard-link
/// deduplication, and dry-run mode. When `--copy-unsafe-links` is active and
/// the target is unsafe, the link target is copied as a regular entry instead.
// upstream: receiver.c:recv_files() - symlink handling
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
    let munge_links = context.munge_links_enabled();

    #[cfg(all(unix, feature = "xattr"))]
    let preserve_xattrs = context.xattrs_enabled();
    #[cfg(all(any(unix, windows), feature = "acl"))]
    let preserve_acls = context.acls_enabled();

    #[cfg(not(all(unix, feature = "xattr")))]
    let _ = context;
    #[cfg(not(all(any(unix, windows), feature = "acl")))]
    let _ = mode;

    let record_path = relative
        .map(Path::to_path_buf)
        .or_else(|| destination.file_name().map(PathBuf::from));
    context.summary_mut().record_symlink_total();
    // upstream: flist.c:691/1243 - `if (S_ISREG(mode) || S_ISLNK(mode))
    // stats.total_size += F_LENGTH(file)`. A symlink's F_LENGTH is its lstat
    // st_size, i.e. the byte length of its target, so count it into the flist
    // total_size exactly once per preserved symlink. Without this the
    // `--stats` "Total file size" line and the "total size is N" trailer are
    // short by the target length (e.g. 50 vs upstream's 55 for a 5-byte link).
    context.summary_mut().record_total_bytes(metadata.len());

    let raw_target = fs::read_link(source)
        .map_err(|error| LocalCopyError::io("read symbolic link", source, error))?;

    // upstream: clientserver.c - on the sender side, unmunge already-munged
    // targets so safety checks and comparisons work on the real path.
    let target = if munge_links {
        let raw_str = raw_target.to_string_lossy();
        match ::metadata::unmunge_symlink(&raw_str) {
            Some(unmangled) => PathBuf::from(unmangled),
            None => raw_target,
        }
    } else {
        raw_target
    };

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

    if context.existing_only_enabled() && destination_metadata.is_none() {
        if let Some(relative_path) = record_path.as_ref() {
            let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, Some(target));
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

    // When copying a directory without a trailing slash, the `relative` path
    // starts with the source directory name (e.g. "source/a/b/link").  That
    // first component is the transfer root itself, not an actual depth level,
    // so strip it via `strip_safety_prefix` to avoid inflating the depth
    // budget in `symlink_target_is_safe`.
    let safety_relative = relative
        .map(|r| context.strip_safety_prefix(r).to_path_buf())
        .or_else(|| {
            destination
                .strip_prefix(context.destination_root())
                .ok()
                .and_then(|path| (!path.as_os_str().is_empty()).then(|| path.to_path_buf()))
        })
        .or_else(|| destination.file_name().map(PathBuf::from))
        .unwrap_or_else(|| destination.to_path_buf());

    let unsafe_target = (context.safe_links_enabled() || context.copy_unsafe_links_enabled())
        && !symlink_target_is_safe(&target, &safety_relative);

    // If the link is unsafe but we were told to copy what it points to, do that.
    if unsafe_target {
        if context.copy_unsafe_links_enabled() {
            // upstream: flist.c:229 - INFO_GTE(SYMSAFE, 1) fires before
            // an unsafe symlink is dereferenced into a regular entry.
            info_log!(
                Symsafe,
                1,
                "copying unsafe symlink \"{}\" -> \"{}\"",
                source.display(),
                target.display()
            );
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
                let _ = copy_file(context, source, destination, &target_metadata, relative)?;
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

    // upstream: generator.c:1140 + try_dests_non() - a `--compare-dest` symlink
    // whose target matches the basis is neither recreated nor transferred; it
    // itemizes `.L` against the basis. Detect that before any creation work so
    // the destination stays empty (compare-dest semantics) and the row collapses
    // to `.L` + blank, suppressed at plain `-i`.
    if destination_metadata.is_none()
        && let Some(path) = record_path.as_ref()
        && !path.as_os_str().is_empty()
        && let Some(basis_meta) =
            super::super::find_compare_dest_symlink(context, destination, path, &target)?
    {
        let symlink_options = if context.omit_link_times_enabled() {
            metadata_options.clone().preserve_times(false)
        } else {
            metadata_options.clone()
        };
        let change_set = LocalCopyChangeSet::for_file(
            metadata,
            Some(&basis_meta),
            &symlink_options,
            true,
            false,
            false,
            false,
            context.options().modify_window(),
        );
        // upstream: generator.c:1140 + receiver.c:734 - a `--compare-dest`
        // match leaves the destination absent and itemizes `.L` with no
        // ITEM_IS_NEW, so `stats.created_files`/`created_symlinks` do NOT count
        // it. It is still tallied into num_symlinks via `record_symlink_total`
        // above; only the created-count is suppressed here.
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, Some(target));
        context.record(
            LocalCopyRecord::new(
                path.clone(),
                LocalCopyAction::MetadataReused,
                0,
                Some(metadata_snapshot.len()),
                Duration::default(),
                Some(metadata_snapshot),
            )
            .with_change_set(change_set),
        );
        context.register_progress();
        remove_source_entry_if_requested(
            context,
            source,
            destination,
            metadata,
            record_path.as_deref(),
            file_type,
        )?;
        return Ok(());
    }

    if let Some(parent) = destination.parent() {
        context.prepare_parent_directory(parent)?;
    }

    // upstream: generator.c:1572-1585 - `quick_check_ok(FT_SYMLINK, ...)` -
    // when the existing destination is already a symlink pointing at the
    // same target, skip the re-create and only re-apply metadata. The
    // itemize line collapses to `.L         ` under `-vv` and is suppressed
    // outright under plain `-i` (iflags=0 path), matching the upstream
    // `testsuite/itemize.test` golden for the post-setup assertion at line
    // 74-79.
    let symlink_target_unchanged = destination_metadata
        .as_ref()
        .is_some_and(|existing| existing.file_type().is_symlink())
        && fs::read_link(destination)
            .map(|existing_target| existing_target == target)
            .unwrap_or(false);

    if symlink_target_unchanged {
        let existing = destination_metadata
            .take()
            .expect("existence verified above");
        let symlink_options = if context.omit_link_times_enabled() {
            metadata_options.clone().preserve_times(false)
        } else {
            metadata_options.clone()
        };
        if !mode.is_dry_run() {
            apply_symlink_metadata_with_options(destination, metadata, &symlink_options)
                .map_err(map_metadata_error)?;
        }
        // upstream: generator.c:1572-1585 + receiver.c:731-746 - an existing
        // symlink already pointing at the same target quick-checks equal:
        // `itemize(..., 0, ...)` sets no ITEM_IS_NEW, so `stats.created_files`
        // does not count it. A no-change re-run therefore reports 0 created
        // files. The symlink is still counted in num_symlinks via
        // `record_symlink_total` above; only the created-count is suppressed.
        if let Some(path) = &record_path {
            let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, Some(target));
            let total_bytes = Some(metadata_snapshot.len());
            // upstream: generator.c:1577 - `itemize(..., 0, ...)` with
            // iflags=0 produces no significant change bits when the link
            // already points at the right place, so the `-i` golden omits
            // the entry entirely. Emit a MetadataReused record without
            // creation/changes so the suppression gate in
            // `out_format::should_suppress_event` collapses the row.
            let change_set = LocalCopyChangeSet::for_file(
                metadata,
                Some(&existing),
                metadata_options,
                true,
                false,
                false,
                false,
                context.options().modify_window(),
            );
            context.record(
                LocalCopyRecord::new(
                    path.clone(),
                    LocalCopyAction::MetadataReused,
                    0,
                    total_bytes,
                    Duration::default(),
                    Some(metadata_snapshot),
                )
                .with_change_set(change_set),
            );
        }
        context.register_progress();
        remove_source_entry_if_requested(
            context,
            source,
            destination,
            metadata,
            record_path.as_deref(),
            file_type,
        )?;
        return Ok(());
    }

    // upstream: generator.c:1606-1609 - when the existing destination is a
    // symlink with a different target, `statret == 0 && stype == FT_SYMLINK`
    // and the recreate-itemize fires with the existing `sx.st` so
    // `mtime_differs()` can compare against the OLD symlink's mtime.
    // Capture that snapshot before removal so the change-set can flag
    // `ITEM_REPORT_TIME` correctly.
    let pre_replace_symlink_metadata = destination_metadata
        .as_ref()
        .filter(|existing| existing.file_type().is_symlink())
        .cloned();

    if !mode.is_dry_run()
        && let Some(existing) = destination_metadata.take()
    {
        // upstream: generator.c:2019 atomic_create() - `make_backup(fname,
        // skip_atomic)` with `skip_atomic` false here, so the hard-link tier
        // runs before the rename.
        context.backup_existing_entry(destination, relative, existing.file_type(), false)?;
        remove_existing_destination(destination)?;
    }

    // upstream: generator.c:1117-1134 try_dests_non() - a `--link-dest` basis
    // symlink with the same target is hard-linked into place rather than
    // recreated, itemizing as `hL` + blank against the basis. Only applies when
    // the destination is being created fresh (no prior symlink to recreate).
    if !mode.is_dry_run() && pre_replace_symlink_metadata.is_none() {
        let link_relative = relative.unwrap_or(record_path.as_deref().unwrap_or(Path::new("")));
        if !link_relative.as_os_str().is_empty()
            && let Some(basis_symlink) = context.link_dest_symlink_target(link_relative, &target)?
        {
            let mut attempted_commit = false;
            loop {
                match create_hard_link(&basis_symlink, destination) {
                    Ok(()) => break,
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        remove_existing_destination(destination)?;
                        create_hard_link(&basis_symlink, destination).map_err(|link_error| {
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
                        context.commit_deferred_update_for(&basis_symlink)?;
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
                let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, Some(target));
                let total_bytes = Some(metadata_snapshot.len());
                // The hard-linked symlink shares the basis inode, so it matches
                // the source exactly: itemize `hL` + blank with the `-> target`
                // trailer from the symlink's own metadata.
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
            remove_source_entry_if_requested(
                context,
                source,
                destination,
                metadata,
                record_path.as_deref(),
                file_type,
            )?;
            return Ok(());
        }
    }

    if let Some(existing_target) = context.existing_hard_link_target(metadata) {
        if mode.is_dry_run() {
            context.summary_mut().record_symlink();
            context.summary_mut().record_hard_link();
            if let Some(path) = &record_path {
                let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, Some(target));
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
            remove_source_entry_if_requested(
                context,
                source,
                destination,
                metadata,
                record_path.as_deref(),
                file_type,
            )?;
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
            let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, Some(target));
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
        remove_source_entry_if_requested(
            context,
            source,
            destination,
            metadata,
            record_path.as_deref(),
            file_type,
        )?;
        return Ok(());
    }

    if mode.is_dry_run() {
        context.summary_mut().record_symlink();
        if let Some(path) = &record_path {
            // upstream: generator.c:1117-1147 - a dry-run still evaluates
            // alt-dest bases. A `--link-dest` symlink with a matching target
            // itemizes as `hL`; a `--copy-dest` symlink as `cL` against the
            // basis. Both leave attribute columns blank when identical.
            let link_dest_match = pre_replace_symlink_metadata.is_none()
                && !path.as_os_str().is_empty()
                && context.link_dest_symlink_target(path, &target)?.is_some();
            let copy_dest_basis = if pre_replace_symlink_metadata.is_none() && !link_dest_match {
                super::super::find_copy_dest_symlink(context, destination, path, &target)?
            } else {
                None
            };

            let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, Some(target));
            let total_bytes = Some(metadata_snapshot.len());

            if link_dest_match {
                context.summary_mut().record_hard_link();
                context.record(LocalCopyRecord::new(
                    path.clone(),
                    LocalCopyAction::HardLink,
                    0,
                    total_bytes,
                    Duration::default(),
                    Some(metadata_snapshot),
                ));
            } else {
                let was_created = copy_dest_basis.is_none()
                    && (!destination_previously_existed || pre_replace_symlink_metadata.is_none());
                let mut record = LocalCopyRecord::new(
                    path.clone(),
                    LocalCopyAction::SymlinkCopied,
                    0,
                    total_bytes,
                    Duration::default(),
                    Some(metadata_snapshot),
                )
                .with_creation(was_created);
                if let Some(existing_symlink) = pre_replace_symlink_metadata.as_ref() {
                    let change_set = LocalCopyChangeSet::for_recreated_symlink(
                        metadata,
                        existing_symlink,
                        metadata_options,
                        context.omit_link_times_enabled(),
                        context.options().modify_window(),
                    );
                    record = record.with_change_set(change_set);
                } else if let Some(basis_meta) = copy_dest_basis.as_ref() {
                    let symlink_options = if context.omit_link_times_enabled() {
                        metadata_options.clone().preserve_times(false)
                    } else {
                        metadata_options.clone()
                    };
                    let change_set = LocalCopyChangeSet::for_file(
                        metadata,
                        Some(basis_meta),
                        &symlink_options,
                        true,
                        false,
                        false,
                        false,
                        context.options().modify_window(),
                    );
                    record = record.with_change_set(change_set);
                }
                context.record(record);
            }
        }
        context.register_progress();
        remove_source_entry_if_requested(
            context,
            source,
            destination,
            metadata,
            record_path.as_deref(),
            file_type,
        )?;
        return Ok(());
    }

    // upstream: clientserver.c - on the receiver side, munge the target so
    // it cannot resolve outside the module root.
    let write_target = if munge_links {
        PathBuf::from(::metadata::munge_symlink(&target.to_string_lossy()))
    } else {
        target.clone()
    };

    if let Err(error) = create_symlink(&write_target, source, destination) {
        // A Windows file symbolic link cannot be created without privilege and
        // has no junction fallback (directory links do fall back inside
        // `create_symlink`). Skip it with a warning and a soft error so the
        // transfer still finishes but exits RERR_PARTIAL (23), matching
        // upstream's FERROR_XFER handling of an unsupported operation.
        #[cfg(windows)]
        if fast_io::is_unprivileged_symlink_error(&error) {
            context.record_skipped_unsupported_symlink(record_path.as_deref(), &target);
            context.register_progress();
            return Ok(());
        }
        return Err(LocalCopyError::io(
            "create symbolic link",
            destination,
            error,
        ));
    }

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
    #[cfg(all(any(unix, windows), feature = "acl"))]
    sync_acls_if_requested(
        preserve_acls,
        context.options().fake_super_enabled(),
        mode,
        source,
        destination,
        false,
    )?;

    context.record_hard_link(metadata, destination);
    context.summary_mut().record_symlink();
    if let Some(path) = &record_path {
        // upstream: generator.c:1119-1148 try_dests_non() - a symlink absent
        // from the destination but present (with the same target) in a
        // `--copy-dest` basis itemizes as a local change (`cL` + blank)
        // against the basis instead of a new entry (`cL+++++++++`).
        let copy_dest_basis = if !destination_previously_existed {
            super::super::find_copy_dest_symlink(context, destination, path, &target)?
        } else {
            None
        };

        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, Some(target));
        let total_bytes = Some(metadata_snapshot.len());

        // upstream: generator.c:1606-1607 - when the existing destination is
        // not a symlink (e.g. a regular file being replaced), the recreate
        // path flips `statret = -1` so `itemize()` lights up `ITEM_IS_NEW`
        // and the row renders `cL+++++++++`. Mirror that by treating a
        // non-symlink replacement as a fresh creation.
        let was_created = copy_dest_basis.is_none()
            && (!destination_previously_existed || pre_replace_symlink_metadata.is_none());

        let mut record = LocalCopyRecord::new(
            path.clone(),
            LocalCopyAction::SymlinkCopied,
            0,
            total_bytes,
            Duration::default(),
            Some(metadata_snapshot),
        )
        .with_creation(was_created);

        // upstream: generator.c:1605-1609 - when the existing destination was
        // a symlink with a different target, the recreate-itemize is called
        // with `ITEM_LOCAL_CHANGE|ITEM_REPORT_CHANGE` and `statret == 0`;
        // `itemize()` then ORs in `ITEM_REPORT_TIME` when `mtime_differs`
        // against the OLD symlink's stat. Mirror that here so the itemize
        // line carries the `c` (target changed) and `t` (mtime change)
        // glyphs, producing `cLc.t......` rather than `cL          `.
        if let Some(existing_symlink) = pre_replace_symlink_metadata.as_ref() {
            let change_set = LocalCopyChangeSet::for_recreated_symlink(
                metadata,
                existing_symlink,
                metadata_options,
                context.omit_link_times_enabled(),
                context.options().modify_window(),
            );
            record = record.with_change_set(change_set);
        } else if let Some(basis_meta) = copy_dest_basis.as_ref() {
            // Compare source against the copy-dest basis symlink so the row
            // stays blank when the reconstructed link is identical.
            let symlink_options = if context.omit_link_times_enabled() {
                metadata_options.clone().preserve_times(false)
            } else {
                metadata_options.clone()
            };
            let change_set = LocalCopyChangeSet::for_file(
                metadata,
                Some(basis_meta),
                &symlink_options,
                true,
                false,
                false,
                false,
                context.options().modify_window(),
            );
            record = record.with_change_set(change_set);
        }

        context.record(record);
    }
    context.register_progress();
    remove_source_entry_if_requested(
        context,
        source,
        destination,
        metadata,
        record_path.as_deref(),
        file_type,
    )?;
    Ok(())
}

/// Creates a symbolic link at `destination` pointing to `target`.
#[cfg(unix)]
pub(crate) fn create_symlink(target: &Path, _source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::unix::fs::symlink;
    symlink(target, destination)
}

/// Creates a symbolic link at `destination` pointing to `target` (Windows variant).
///
/// Directory links go through [`fast_io::create_directory_symlink_or_junction`],
/// which prefers a real directory symlink and falls back to a junction when the
/// caller lacks the create-symlink privilege (unprivileged, no Developer Mode).
/// File links have no junction equivalent, so a privilege refusal surfaces as
/// `ERROR_PRIVILEGE_NOT_HELD`; the caller skips the entry with a warning.
#[cfg(windows)]
pub(crate) fn create_symlink(target: &Path, source: &Path, destination: &Path) -> io::Result<()> {
    let is_dir = matches!(source.metadata(), Ok(metadata) if metadata.file_type().is_dir());
    if is_dir {
        fast_io::create_directory_symlink_or_junction(target, destination).map(|_| ())
    } else {
        fast_io::create_file_symlink(target, destination)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // "link" has depth 0 (filename excluded), so 1 parent dir escapes
        assert!(!symlink_target_is_safe(
            Path::new("../outside"),
            Path::new("link")
        ));
    }

    #[test]
    fn unsafe_escapes_with_multiple_parents() {
        // More parent components than depth allows
        // dir/link has depth 1 (filename excluded), so 2 parent dirs escapes
        assert!(!symlink_target_is_safe(
            Path::new("../../outside"),
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
        // link at a/b/c/link has depth 3 (filename excluded)
        // So 4 parent dirs would be needed to escape
        assert!(!symlink_target_is_safe(
            Path::new("../../../../outside"),
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
        // link at a/b/c/link has depth 3 (filename excluded), so 3 parent dirs is exactly at boundary
        assert!(symlink_target_is_safe(
            Path::new("../../../file.txt"),
            Path::new("a/b/c/link")
        ));
    }

    #[test]
    fn unsafe_one_past_boundary() {
        // Going up one more level than allowed
        // link at a/b/c/link has depth 3 (filename excluded), so 4 parent dirs escapes
        assert!(!symlink_target_is_safe(
            Path::new("../../../../file.txt"),
            Path::new("a/b/c/link")
        ));
    }
}
