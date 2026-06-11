//! File transfer candidate selection.
//!
//! Builds the list of files that need transfer by applying quick-check
//! heuristics, size bounds, failed directory tracking, and parallel stat.
//! Emits metadata-only itemize lines for up-to-date files.
//!
//! # Upstream Reference
//!
//! - `generator.c:recv_generator()` - per-file quick-check and skip logic
//! - `generator.c:942` - `try_dests_reg()` for reference directory handling
//! - `generator.c:617` - `quick_check_ok()` evaluation order

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use logging::{debug_gte, debug_log, info_log};
use metadata::{MetadataOptions, apply_metadata_with_cached_stat, metadata_unchanged};
use protocol::flist::FileEntry;

use crate::receiver::directory::FailedDirectories;
use crate::receiver::quick_check::{
    dest_mtime_newer, is_hardlink_follower, quick_check_matches, try_reference_dest,
};
use crate::receiver::stats::TransferStats;
use crate::receiver::{ReceiverContext, apply_acls_from_receiver_cache};

impl ReceiverContext {
    /// Builds the list of files that need transfer, applying quick-check to skip
    /// unchanged files and respecting size bounds and failed directory tracking.
    ///
    /// For files that are up-to-date (quick-check match), emits a metadata-only
    /// itemize line via MSG_INFO when the daemon has itemize output enabled, and
    /// applies any pending metadata updates (ownership, permissions, timestamps).
    ///
    /// Optimized for the 100K-file no-change scan path: pre-computes config
    /// flags, skips metadata/ACL/xattr work when the corresponding features are
    /// disabled, and avoids per-file allocations where possible.
    pub(in crate::receiver) fn build_files_to_transfer<
        'a,
        W: Write + crate::writer::MsgInfoSender + ?Sized,
    >(
        &'a self,
        writer: &mut W,
        dest_dir: &Path,
        metadata_opts: &MetadataOptions,
        failed_dirs: Option<&FailedDirectories>,
        metadata_errors: &mut Vec<(PathBuf, String)>,
        stats: &mut TransferStats,
        acl_cache: Option<&protocol::acl::AclCache>,
    ) -> Vec<(usize, &'a FileEntry, PathBuf)> {
        // upstream: generator.c:1234-1235 - "recv_generator(%s,%d)" emitted at
        // the top of recv_generator() for every file the generator considers
        // (regular files, directories, symlinks, devices, specials). Skipping
        // the loop when the flag is off keeps the hot path allocation-free.
        if debug_gte(logging::DebugFlag::Genr, 1) {
            for (flat_idx, entry) in self.file_list.iter().enumerate() {
                let ndx = self.flat_to_wire_ndx(flat_idx);
                debug_log!(
                    Genr,
                    1,
                    "recv_generator({},{})",
                    entry.path().display(),
                    ndx
                );
            }
        }

        // Phase A: Filter candidates (cheap, in-memory checks only).
        // Pre-extract config values to avoid repeated field access in the
        // filter closures at 100K scale.
        let daemon_filters = self.daemon_filter_set();
        let min_size = self.config.file_selection.min_file_size;
        let max_size = self.config.file_selection.max_file_size;
        let has_size_bounds = min_size.is_some() || max_size.is_some();
        let has_daemon_filters = daemon_filters.is_some();
        let has_failed_dirs = failed_dirs.is_some();
        let verbose_client = self.config.flags.verbose && self.config.connection.client_mode;

        let candidates: Vec<(usize, &FileEntry)> = self
            .file_list
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_file())
            .filter(|(_, e)| !is_hardlink_follower(e))
            .filter(|(_, e)| {
                if !has_size_bounds {
                    return true;
                }
                let sz = e.size();
                min_size.is_none_or(|m| sz >= m) && max_size.is_none_or(|m| sz <= m)
            })
            .filter(|(_, e)| {
                // upstream: receiver.c:599-604 - check_filter(&daemon_filter_list, ...)
                // rejects daemon-excluded files before accepting transfer data.
                if has_daemon_filters {
                    let filters = daemon_filters.unwrap();
                    let name = e.name();
                    if name != "." && !filters.allows(Path::new(name), false) {
                        stats.files_skipped += 1;
                        return false;
                    }
                }
                if has_failed_dirs {
                    let fd = failed_dirs.unwrap();
                    if let Some(failed_parent) = fd.failed_ancestor(e.name()) {
                        if verbose_client {
                            info_log!(
                                Skip,
                                1,
                                "skipping {} (parent {} failed)",
                                e.name(),
                                failed_parent
                            );
                        }
                        stats.files_skipped += 1;
                        return false;
                    }
                }
                true
            })
            .collect();

        // upstream: generator.c:1845 - dry_run (!do_xfers) skips stat and data
        // transfer but still builds the candidate list so NDX requests are sent
        // to the sender, which logs each file name for verbose output.
        if self.config.flags.dry_run {
            return candidates
                .into_iter()
                .map(|(idx, entry)| (idx, entry, dest_dir.join(entry.path())))
                .collect();
        }

        let preserve_times = self.config.flags.times && !self.config.flags.ignore_times;
        let size_only = self.config.file_selection.size_only;
        let ignore_existing = self.config.file_selection.ignore_existing;
        let existing_only = self.config.file_selection.existing_only;
        let update_only = self.config.flags.update;
        let always_checksum = if self.config.flags.checksum {
            Some(self.get_checksum_algorithm())
        } else {
            None
        };

        // Pre-compute whether itemize emission is active so we skip the
        // per-file method dispatch for the common no-itemize case.
        let emit_itemize = self.should_emit_itemize();

        // Pre-compute whether ACLs and xattrs are enabled. When disabled
        // (the common case), the per-file function call overhead is avoided
        // entirely in the no-change path. At 100K files this eliminates
        // 100K-200K function calls that would each immediately return None/Ok.
        let has_acls = acl_cache.is_some() && self.config.flags.acls;
        let has_xattrs = self.config.flags.xattrs;
        let has_reference_dirs = !self.config.reference_directories.is_empty();

        // Phase B: Parallel stat - preserve PathBufs for reuse in Phase C and
        // the pipeline loop, avoiding a second dest_dir.join() per file.
        let stat_paths: Vec<(usize, PathBuf)> = candidates
            .iter()
            .map(|&(idx, entry)| (idx, dest_dir.join(entry.path())))
            .collect();

        let stat_results: Vec<(usize, PathBuf, Option<fs::Metadata>)> =
            crate::parallel_io::map_blocking(
                stat_paths,
                self.parallel_thresholds
                    .for_op(crate::parallel_io::ParallelOp::Stat),
                move |(idx, file_path)| {
                    let meta = fs::metadata(&file_path).ok();
                    (idx, file_path, meta)
                },
            );

        // Phase C: Sequential post-processing with stat results.
        // Pre-size for the expected minority that need transfer.
        let needs_metadata_apply = metadata_opts.requires_apply();
        let mut files_to_transfer = Vec::with_capacity(stat_results.len() / 4 + 1);
        for (idx, file_path, dest_meta) in stat_results {
            let entry = &self.file_list[idx];
            if let Some(ref meta) = dest_meta {
                if ignore_existing {
                    continue;
                }
                if update_only && dest_mtime_newer(meta, entry) {
                    continue;
                }
                if quick_check_matches(
                    entry,
                    &file_path,
                    meta,
                    preserve_times,
                    size_only,
                    always_checksum,
                ) {
                    self.apply_no_change_metadata(
                        writer,
                        &file_path,
                        entry,
                        meta,
                        metadata_opts,
                        metadata_errors,
                        acl_cache,
                        emit_itemize,
                        has_acls,
                        has_xattrs,
                        needs_metadata_apply,
                    );
                    continue;
                }
            } else {
                if existing_only {
                    continue;
                }
                if has_reference_dirs
                    && try_reference_dest(
                        entry,
                        dest_dir,
                        &self.config.reference_directories,
                        preserve_times,
                        size_only,
                        always_checksum,
                        self.config.flags.copy_links,
                        metadata_opts,
                        metadata_errors,
                        acl_cache,
                    )
                {
                    continue;
                }
            }
            files_to_transfer.push((idx, entry, file_path));
        }
        files_to_transfer
    }

    /// Applies metadata updates for a file that passed quick-check (no transfer needed).
    ///
    /// This is the hot path for no-change scans at scale. Each guard check
    /// avoids a function call and potential syscalls when the corresponding
    /// feature is disabled.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1814` - `set_file_attrs()` on quick-check match
    /// - `generator.c:1816` - `itemize()` on quick-check match
    #[allow(clippy::too_many_arguments)]
    fn apply_no_change_metadata<W: Write + crate::writer::MsgInfoSender + ?Sized>(
        &self,
        writer: &mut W,
        file_path: &Path,
        entry: &FileEntry,
        stat_meta: &fs::Metadata,
        metadata_opts: &MetadataOptions,
        metadata_errors: &mut Vec<(PathBuf, String)>,
        acl_cache: Option<&protocol::acl::AclCache>,
        emit_itemize: bool,
        has_acls: bool,
        has_xattrs: bool,
        needs_metadata_apply: bool,
    ) {
        // upstream: generator.c:1816 - itemize() with iflags=0 for up-to-date
        // files. iflags=0 has no significant flags, so emit_itemize will suppress
        // output (generator.c:574-576). Skip the call entirely.
        if emit_itemize {
            let iflags = crate::generator::ItemFlags::from_raw(0);
            let _ = self.emit_itemize(writer, &iflags, entry);
        }

        // upstream: generator.c:461 unchanged_attrs() - fast-path check avoids
        // the per-function-call overhead of apply_metadata when all attributes
        // already match. Skip entirely when no preservation flags are active.
        // On a no-change scan this eliminates ownership mapping, permission
        // comparison, and timestamp construction for every file.
        if needs_metadata_apply && !metadata_unchanged(entry, metadata_opts, stat_meta) {
            if let Err(e) = apply_metadata_with_cached_stat(
                file_path,
                entry,
                metadata_opts,
                Some(stat_meta.clone()),
            ) {
                metadata_errors.push((file_path.to_path_buf(), e.to_string()));
            }
        }

        // upstream: rsync.c:set_file_attrs() -> set_acl() for ACL preservation
        if has_acls {
            if let Err(e) =
                apply_acls_from_receiver_cache(file_path, entry, acl_cache, !entry.is_symlink())
            {
                metadata_errors.push((file_path.to_path_buf(), e.to_string()));
                return;
            }
        }

        // upstream: xattrs.c:set_xattr() - apply xattrs after metadata
        if has_xattrs {
            if let Some(ref xattr_list) = self.resolve_xattr_list(entry) {
                if let Err(e) = metadata::apply_xattrs_from_list(file_path, xattr_list, true) {
                    metadata_errors.push((file_path.to_path_buf(), e.to_string()));
                }
            }
        }
    }
}
