//! Top-level source processing orchestration and deferred operation flushing.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use logging::{debug_log, info_log};

use crate::local_copy::overrides::device_identifier;
use crate::local_copy::{
    CopyContext, CopyOutcome, LocalCopyAction, LocalCopyArgumentError, LocalCopyError,
    LocalCopyExecution, LocalCopyOptions, LocalCopyPlan, LocalCopyRecord, LocalCopyRecordHandler,
    SourceSpec,
};

use super::super::file::remove_existing_destination;
use super::super::non_empty_path;
use super::destination::{ensure_destination_directory, query_destination_state};
use super::handlers::{
    handle_directory_contents_copy, handle_directory_copy, handle_non_directory_source,
};
use super::metadata::{compute_relative_paths, fetch_source_metadata};
use super::types::{SourceMetadataResult, SourceProcessingContext};

/// Entry point for copying all sources to the destination.
///
/// Sets up the copy context, iterates over sources, and handles deferred
/// operations and error rollback.
pub(crate) fn copy_sources(
    plan: &LocalCopyPlan,
    mode: LocalCopyExecution,
    options: LocalCopyOptions,
    handler: Option<&mut dyn LocalCopyRecordHandler>,
) -> Result<CopyOutcome, LocalCopyError> {
    // upstream: rsync.c:do_as_root() - switch effective UID/GID before receiver
    // file operations. The RAII guard restores the original identity on drop.
    let _copy_as_guard = options
        .copy_as_ids()
        .map(::metadata::switch_effective_ids)
        .transpose()
        .map_err(|err| {
            LocalCopyError::io(
                "switch effective identity for --copy-as",
                plan.destination_spec().path(),
                err,
            )
        })?;

    let run_start = Instant::now();
    let destination_root = plan.destination_spec().path().to_path_buf();
    let mut context = CopyContext::new(mode, options, handler, destination_root);
    context.set_multi_source(plan.sources().len() > 1);

    // upstream: generator.c:2290-2295 - the generator prints the
    // delta-transmission status once, gated on DEBUG_GTE(FLIST, 1) (first
    // active at -vv). Local copies default to whole-file mode; delta is
    // used only when --no-whole-file is explicitly set.
    debug_log!(
        Flist,
        1,
        "delta-transmission {}",
        if context.options().whole_file_enabled() {
            "disabled for local transfer or --whole-file"
        } else {
            "enabled"
        }
    );

    let result = {
        let context = &mut context;
        (|| -> Result<(), LocalCopyError> {
            let multiple_sources = plan.sources().len() > 1;
            let destination_path = plan.destination_spec().path();
            let mut destination_state = query_destination_state(destination_path)?;
            if context.keep_dirlinks_enabled() && destination_state.symlink_to_dir {
                destination_state.is_dir = true;
            }

            // upstream: main.c:794-799 - the receiver pre-flight-mkdirs the
            // destination root, flags the synthetic "." flist entry with
            // FLAG_DIR_CREATED, and emits `created directory %s\n` when
            // INFO_GTE(NAME, 1) || stdout_format_has_i. Surface the same
            // signal here so the CLI itemize gate (rendered in
            // emit_transfer_summary) and the synthesized `cd+++++++++ ./`
            // root record (rendered by copy_directory_recursive) both fire.
            let mut destination_root_created = false;
            let mkpath = context.mkpath_enabled();
            if plan.destination_spec().force_directory() {
                destination_root_created |= ensure_destination_directory(
                    destination_path,
                    &mut destination_state,
                    context.mode(),
                    mkpath,
                )?;
            }

            if multiple_sources {
                destination_root_created |= ensure_destination_directory(
                    destination_path,
                    &mut destination_state,
                    context.mode(),
                    mkpath,
                )?;
            }

            // upstream: main.c:778 get_local_name() - `if (file_total > 1 ||
            // trailing_slash) { do_mkdir(dest_path); ... }`. The transfer-level
            // decision is made ONCE, from the flist entry count. For a single
            // no-trailing-slash directory source `file_total > 1` requires the
            // directory's children to be enumerated into the flist, which only
            // happens under recursion (`-r`/`-a`): then pre-create the
            // destination root here (counting it and emitting `created directory
            // <dest>`) and let `destination_behaves_like_directory` keep the
            // source name. Without `-r`/`-d` the directory operand is skipped
            // entirely (`flist.c:2451` `!xfer_dirs`) and no destination is
            // created; with `-d` alone a no-trailing-slash directory contributes
            // only its own entry (`file_total == 1`), so the name is dropped and
            // the destination is materialised AS the directory. Gating on
            // recursion keeps all three cases byte-for-byte with upstream.
            if !multiple_sources
                && context.recursive_enabled()
                && !plan.destination_spec().force_directory()
                && !destination_state.is_dir
                && let Some(source) = plan.sources().first()
                && !source.copy_contents()
                && fs::symlink_metadata(source.path()).is_ok_and(|meta| meta.is_dir())
                && fs::read_dir(source.path()).is_ok_and(|mut entries| entries.next().is_some())
            {
                // A pre-existing non-directory destination blocks the mkdir.
                // When force replacements are enabled, remove it first (mirrors
                // the per-entry force-replace path in handlers.rs) so the
                // source name is preserved as `dest/<source>/`. Without force,
                // refuse to replace the existing file with a directory, mirroring
                // the recursive executor's error for the same conflict.
                if destination_state.exists && !destination_state.is_dir {
                    if context.force_replacements_enabled() && !context.mode().is_dry_run() {
                        remove_existing_destination(destination_path)?;
                        destination_state.exists = false;
                    } else {
                        return Err(LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                        ));
                    }
                }
                destination_root_created |= ensure_destination_directory(
                    destination_path,
                    &mut destination_state,
                    context.mode(),
                    mkpath,
                )?;
            }

            if destination_root_created {
                context.summary_mut().mark_destination_root_created();
            }

            let destination_behaves_like_directory =
                destination_state.is_dir || plan.destination_spec().force_directory();

            let mut first_io_error: Option<LocalCopyError> = None;
            for source in plan.sources() {
                let result = process_single_source(
                    context,
                    plan,
                    source,
                    destination_path,
                    destination_behaves_like_directory,
                    multiple_sources,
                );
                if let Err(error) = result {
                    if error.is_vanished_error() {
                        // upstream: flist.c:1289 - vanished files produce a warning
                        // and set IOERR_VANISHED, but transfer continues.
                        // full_fname() wraps the path in double quotes (util1.c:1228).
                        eprintln!("file has vanished: \"{}\"", source.path().display());
                        context.record_io_error();
                        if first_io_error.is_none() {
                            first_io_error = Some(error);
                        }
                    } else if error.is_link_stat_failed() {
                        // upstream: flist.c send_file_list() - a missing source
                        // argument prints `link_stat "%s" failed: %s` to stderr,
                        // sets IOERR_GENERAL (exit 23), and the transfer
                        // continues with the remaining sources.
                        eprintln!("{error}");
                        context.record_io_error();
                        if first_io_error.is_none() {
                            first_io_error = Some(error);
                        }
                    } else if error.is_io_error() {
                        // upstream: rsync continues transferring remaining sources
                        // when individual entries fail with I/O errors, regardless
                        // of whether --delete is active.
                        context.record_io_error();
                        if first_io_error.is_none() {
                            first_io_error = Some(error);
                        }
                    } else {
                        return Err(error);
                    }
                }
            }

            // Write the flist end-of-list marker, ID lists, then delta data.
            // upstream: flist.c:2513-2514 - without INC_RECURSE, send_id_lists()
            // writes uid/gid name mappings after the flist end marker.
            // Since names are already embedded inline via XMIT_USER_NAME_FOLLOWS,
            // the ID lists are empty (just varint30(0) terminators), but they
            // must be present for upstream's recv_id_list() to consume.
            context.finalize_batch_flist()?;
            context.write_batch_id_lists()?;
            context.flush_batch_delta_to_batch()?;
            // Stats are written by core::client::run::batch::finalize_batch()
            // after the engine returns, using actual transfer byte counts.

            // upstream: main.c:1839-1840 - `if (write_batch < 0) dry_run = 1`
            // forces dry_run when `--only-write-batch` is set, so the receiver
            // never reaches do_recv() / finish_transfer(). Mirror that by
            // returning before flushing deferred destination updates: in
            // OnlyWrite mode the batch file is the sole output and no
            // destination-side writes should be performed.
            //
            // The combination `DryRun + batch_writer present` distinguishes
            // `--only-write-batch` (this branch) from plain `--dry-run`
            // (no batch writer; falls through to the deferred-ops flush so
            // empty queues drain cleanly without side effects).
            if context.mode().is_dry_run() && context.options().get_batch_writer().is_some() {
                if let Some(error) = first_io_error {
                    return Err(error);
                }
                if context.iconv_conversion_error_occurred()
                    || context.unsupported_operation_skipped()
                {
                    return Err(LocalCopyError::partial_transfer());
                }
                return Ok(());
            }

            flush_deferred_operations(context)?;

            if let Some(error) = first_io_error {
                return Err(error);
            }
            // A platform-unsupported entry (a Windows unprivileged file symlink)
            // was skipped with a warning; finish RERR_PARTIAL (23) like upstream
            // FERROR_XFER after a failed do_symlink().
            if context.unsupported_operation_skipped() {
                return Err(LocalCopyError::partial_transfer());
            }
            // upstream: flist.c:1631 send_file1() sets io_error |= IOERR_GENERAL
            // when a filename cannot be transcoded under --iconv; main.c:1356
            // then exits RERR_PARTIAL (23). The per-entry diagnostic was already
            // printed at the skip site, so surface only the summary error here.
            if context.iconv_conversion_error_occurred() {
                return Err(LocalCopyError::partial_transfer());
            }
            // upstream: sender.c:successful_send() - a source refused by a
            // --remove-source-files safety guard (changed file / destination
            // inode) or a failed unlink sets got_xfer_error; main.c:1630 then
            // exits RERR_PARTIAL (23). The per-entry diagnostic was already
            // printed at the guard site, so surface only the summary error here.
            if context.sender_remove_error_occurred() {
                return Err(LocalCopyError::partial_transfer());
            }
            Ok(())
        })()
    };

    match result {
        Ok(()) => {
            // upstream main.c:418: the transfer rate uses a single wall-clock
            // span, not the sum of per-file copy durations (which is ~0 for CoW).
            context
                .summary_mut()
                .record_wall_clock_elapsed(run_start.elapsed());
            Ok(context.into_outcome())
        }
        Err(error) => {
            context.rollback_on_error(&error);
            Err(error)
        }
    }
}

/// Processes a single source entry in the copy operation.
fn process_single_source(
    context: &mut CopyContext,
    plan: &LocalCopyPlan,
    source: &SourceSpec,
    destination_path: &Path,
    destination_behaves_like_directory: bool,
    multiple_sources: bool,
) -> Result<(), LocalCopyError> {
    // Directory copy handlers set the correct offset before recursing.
    context.set_safety_depth_offset(0);
    context.enforce_timeout()?;

    let source_path = source.path();
    let metadata_start = Instant::now();

    let (relative_root, relative_parent) = compute_relative_paths(context, source);

    let metadata_result = fetch_source_metadata(
        context,
        source,
        source_path,
        destination_path,
        destination_behaves_like_directory,
        multiple_sources,
        relative_root.as_deref(),
        metadata_start,
    )?;

    let metadata = match metadata_result {
        SourceMetadataResult::Found(m) => m,
        SourceMetadataResult::Handled => return Ok(()),
        SourceMetadataResult::NotFoundError(error) => {
            // upstream: flist.c send_file_list() - a source argument (operand or
            // --files-from entry) whose link_stat fails is reported as
            // `link_stat "%s" failed`, sets IOERR_GENERAL (exit 23,
            // RERR_PARTIAL), and the transfer continues with the remaining
            // sources. Distinct from a file that vanishes mid-transfer (exit
            // 24, "file has vanished").
            return Err(LocalCopyError::link_stat_failed(
                source_path.to_path_buf(),
                error,
            ));
        }
        SourceMetadataResult::IoError(error) => {
            return Err(LocalCopyError::io(
                "access source",
                source_path.to_path_buf(),
                error,
            ));
        }
    };

    context.record_file_list_generation(metadata_start.elapsed());

    // upstream: flist.c:make_file() - skip files with bogus zero st_mode
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() == 0 {
            context.record_io_error();
            return Ok(());
        }
    }

    let file_type = metadata.file_type();

    let destination_base = if let Some(parent) = &relative_parent {
        destination_path.join(parent)
    } else {
        destination_path.to_path_buf()
    };

    let root_device = if context.one_file_system_enabled() {
        device_identifier(source_path, &metadata)
    } else {
        None
    };

    // With -xx (level >= 2), skip root-level source directories that are mount
    // points - i.e. their device ID differs from their parent directory.
    if context.one_file_system_level() >= 2 && file_type.is_dir() {
        if let Some(parent) = source_path.parent() {
            if let Ok(parent_meta) = fs::symlink_metadata(parent) {
                if let Some(parent_dev) = device_identifier(parent, &parent_meta) {
                    if let Some(source_dev) = root_device {
                        if source_dev != parent_dev {
                            let record_relative = relative_root
                                .as_deref()
                                .and_then(|p| non_empty_path(p))
                                .or_else(|| source_path.file_name().map(Path::new));
                            // upstream: flist.c:1319 - INFO_GTE(MOUNT, 1) gates
                            // `rprintf(FINFO, "[%s] skipping mount-point dir %s", who_am_i(),
                            // thisname)` when `-xx` prunes a root-level mount-point source.
                            // The role prefix is added downstream by the renderer.
                            info_log!(
                                Mount,
                                1,
                                "skipping mount-point dir {}",
                                source_path.display()
                            );
                            context.record_skipped_mount_point(record_relative);
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    let proc_ctx = SourceProcessingContext {
        source,
        source_path,
        metadata: metadata.clone(),
        file_type,
        relative_root: relative_root.clone(),
        relative_parent: relative_parent.clone(),
        destination_path,
        destination_base,
        destination_behaves_like_directory,
        multiple_sources,
        root_device,
    };

    let record_relative = proc_ctx.compute_record_relative();
    context.record_file_list_entry(record_relative.as_deref());

    if proc_ctx.requires_directory_destination() && !destination_behaves_like_directory {
        return Err(LocalCopyError::invalid_argument(
            LocalCopyArgumentError::DestinationMustBeDirectory,
        ));
    }

    if file_type.is_dir() {
        if source.copy_contents() {
            handle_directory_contents_copy(
                context,
                source_path,
                &metadata,
                relative_root.as_ref(),
                destination_path,
                root_device,
            )?;
        } else {
            handle_directory_copy(
                context,
                source_path,
                &metadata,
                relative_root.as_ref(),
                destination_path,
                &proc_ctx.destination_base,
                destination_behaves_like_directory,
                multiple_sources,
                root_device,
            )?;
        }
    } else {
        handle_non_directory_source(context, &proc_ctx, plan)?;
    }

    retouch_relative_implied_dirs(
        context,
        source,
        source_path,
        destination_path,
        relative_root.as_deref(),
    )?;

    context.enforce_timeout()?;
    Ok(())
}

/// Retouches the implied parent directories materialized along this source's
/// `--relative` chain so they carry the source's directory metadata rather
/// than the wall-clock timestamps deposited by `create_dir_all` and the FS
/// side-effect of writing children.
///
/// Mirrors upstream rsync's two-phase approach:
///
/// 1. `flist.c:2417-2419` + `flist.c:1948` (`send_implied_dirs`) emit each
///    implied parent (and the leading `.` when the operand carries the dot
///    marker) into the flist with `FLAG_IMPLIED_DIR`.
/// 2. `generator.c:1503` (`set_file_attrs` during `recv_generator`) and
///    `generator.c:2128-2136` (`touch_up_dirs` at end-of-transfer) make sure
///    every implied dir ends the transfer with the source's mtime/perms even
///    after the receiver wrote children into it.
///
/// We replay both stages in a single post-source pass so file children added
/// via a sibling source cannot leave the parent dir's mtime stuck at the
/// wall-clock value the FS assigned during the file write.
fn retouch_relative_implied_dirs(
    context: &mut CopyContext,
    source: &SourceSpec,
    source_path: &Path,
    destination_path: &Path,
    relative_root: Option<&Path>,
) -> Result<(), LocalCopyError> {
    if context.mode().is_dry_run()
        || !context.relative_paths_enabled()
        || !context.implied_dirs_enabled()
    {
        return Ok(());
    }

    let metadata_options = if context.omit_dir_times_enabled() {
        context.metadata_options().preserve_times(false)
    } else {
        context.metadata_options()
    };

    // Phase 1: stamp the destination operand from the dot-dir anchor when the
    // operand carries an explicit `./` marker. Upstream emits this as the
    // synthetic `.` entry in `flist.c:2419`.
    if source.has_dot_dir_marker()
        && let Some(anchor) = source.dot_dir_anchor()
    {
        stamp_directory_from_source(destination_path, &anchor, &metadata_options)?;
    }

    // Phase 2: walk every implied parent dir along the relative chain and
    // stamp each one from its source counterpart. For directory sources we
    // skip the leaf because copy_directory_recursive's own
    // apply_final_directory_metadata stamps it; for file/symlink/special
    // sources every component of the relative path IS an implied parent.
    let Some(relative) = relative_root else {
        return Ok(());
    };
    let components: Vec<&std::ffi::OsStr> = relative.iter().collect();
    if components.is_empty() {
        return Ok(());
    }
    let parent_count = components.len().saturating_sub(1);
    if parent_count == 0 {
        return Ok(());
    }

    let Some(source_root) = strip_path_suffix(source_path, relative) else {
        return Ok(());
    };
    let destination_root = strip_path_suffix(destination_path, relative)
        .unwrap_or_else(|| destination_path.to_path_buf());

    let mut accumulated = PathBuf::new();
    for component in &components[..parent_count] {
        accumulated.push(component);
        let src_dir = source_root.join(&accumulated);
        let dst_dir = destination_root.join(&accumulated);
        stamp_directory_from_source(&dst_dir, &src_dir, &metadata_options)?;
    }

    Ok(())
}

/// Applies `source_dir`'s directory metadata to `dest_dir`, silently skipping
/// the pair when either side is not a directory we can stat. Mirrors the
/// best-effort stance of upstream `set_file_attrs` for implied dirs - the
/// transfer is allowed to proceed even when an implied parent is unstable
/// (vanished or replaced with a non-dir between phases).
fn stamp_directory_from_source(
    dest_dir: &Path,
    source_dir: &Path,
    metadata_options: &::metadata::MetadataOptions,
) -> Result<(), LocalCopyError> {
    let source_meta = match fs::symlink_metadata(source_dir) {
        Ok(meta) if meta.file_type().is_dir() => meta,
        _ => return Ok(()),
    };
    match fs::symlink_metadata(dest_dir) {
        Ok(meta) if meta.file_type().is_dir() => {}
        _ => return Ok(()),
    }
    ::metadata::apply_directory_metadata_with_options(
        dest_dir,
        &source_meta,
        metadata_options.clone(),
    )
    .map_err(crate::local_copy::map_metadata_error)?;
    Ok(())
}

/// Strips `relative` from the trailing components of `path`. Returns the
/// remaining prefix. Used to recover the source/destination roots used to
/// join an implied-dir chain back together for stamping.
fn strip_path_suffix(path: &Path, relative: &Path) -> Option<PathBuf> {
    let path_components: Vec<_> = path.components().collect();
    let rel_components: Vec<_> = relative.components().collect();
    if rel_components.len() > path_components.len() {
        return None;
    }
    let split = path_components.len() - rel_components.len();
    for (idx, rel) in rel_components.iter().enumerate() {
        if path_components[split + idx].as_os_str() != rel.as_os_str() {
            return None;
        }
    }
    let mut root = PathBuf::new();
    for component in &path_components[..split] {
        root.push(component.as_os_str());
    }
    if root.as_os_str().is_empty() {
        return Some(PathBuf::from("."));
    }
    Some(root)
}

/// Flushes all deferred operations after source processing is complete.
fn flush_deferred_operations(context: &mut CopyContext) -> Result<(), LocalCopyError> {
    context.flush_deferred_updates()?;
    context.flush_deferred_deletions()?;
    context.flush_deferred_syncs()?;
    // Final directory-mtime touch-up. Runs once, after every late in-directory
    // mutation above (delayed-update renames, deletions, backups) has bumped
    // the destination directory mtimes that apply_final_directory_metadata set.
    // upstream: generator.c:2449-2451 touch_up_dirs after handle_delayed_updates.
    context.touch_up_dirs();
    context.enforce_timeout()?;
    Ok(())
}

/// Deletes a destination entry whose source has gone missing.
///
/// Invoked when `--delete-missing-args` is active and the source path
/// no longer exists on disk.
pub(super) fn delete_missing_source_entry(
    context: &mut CopyContext,
    source: &SourceSpec,
    destination_path: &Path,
    destination_behaves_like_directory: bool,
    multiple_sources: bool,
    relative_root: Option<&Path>,
) -> Result<(), LocalCopyError> {
    if source.copy_contents() {
        return Ok(());
    }

    let source_path = source.path();
    let relative = if let Some(root) = relative_root {
        root.to_path_buf()
    } else {
        let name = source_path.file_name().ok_or_else(|| {
            LocalCopyError::invalid_argument(LocalCopyArgumentError::FileNameUnavailable)
        })?;
        PathBuf::from(Path::new(name))
    };

    let target = if destination_behaves_like_directory || multiple_sources {
        destination_path.join(&relative)
    } else {
        destination_path.to_path_buf()
    };

    let metadata = match fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(LocalCopyError::io(
                "inspect destination entry",
                target.clone(),
                error,
            ));
        }
    };

    let file_type = metadata.file_type();

    if !context.allows_deletion(relative.as_path(), file_type.is_dir()) {
        return Ok(());
    }

    if let Some(limit) = context.options().max_deletion_limit()
        && context.summary().items_deleted() >= limit
    {
        return Err(LocalCopyError::delete_limit_exceeded(1));
    }

    let record_path = non_empty_path(relative.as_path());

    if context.mode().is_dry_run() {
        context.summary_mut().record_deletion(file_type);
        if let Some(path) = record_path {
            context.record(LocalCopyRecord::new(
                path.to_path_buf(),
                LocalCopyAction::EntryDeleted,
                0,
                None,
                Duration::default(),
                None,
            ));
        }
        context.register_progress();
        return Ok(());
    }

    // upstream: delete.c:167 - prefer_rename=True; the item is unlinked
    // outright right after, so skip the hard-link tier.
    context.backup_existing_entry(&target, record_path, file_type, true)?;
    let removal = if file_type.is_dir() {
        fs::remove_dir_all(&target)
    } else {
        fs::remove_file(&target)
    };

    match removal {
        Ok(()) => {
            context.summary_mut().record_deletion(file_type);
            if let Some(path) = record_path {
                context.record(LocalCopyRecord::new(
                    path.to_path_buf(),
                    LocalCopyAction::EntryDeleted,
                    0,
                    None,
                    Duration::default(),
                    None,
                ));
            }
            context.register_progress();
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            let action = if file_type.is_dir() {
                "remove destination directory"
            } else {
                "remove destination entry"
            };
            return Err(LocalCopyError::io(action, target, error));
        }
    }

    Ok(())
}
