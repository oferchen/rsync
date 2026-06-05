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
    /// itemize line via MSG_INFO when the daemon has itemize output enabled.
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
        let daemon_filters = self.daemon_filter_set();
        let candidates: Vec<(usize, &FileEntry)> = self
            .file_list
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_file())
            .filter(|(_, e)| !is_hardlink_follower(e))
            .filter(|(_, e)| {
                let sz = e.size();
                self.config
                    .file_selection
                    .min_file_size
                    .is_none_or(|m| sz >= m)
                    && self
                        .config
                        .file_selection
                        .max_file_size
                        .is_none_or(|m| sz <= m)
            })
            .filter(|(_, e)| {
                // upstream: receiver.c:599-604 - check_filter(&daemon_filter_list, ...)
                // rejects daemon-excluded files before accepting transfer data.
                if let Some(filters) = daemon_filters {
                    let name = e.name();
                    if name != "." && !filters.allows(Path::new(name), false) {
                        stats.files_skipped += 1;
                        return false;
                    }
                }
                if let Some(fd) = failed_dirs {
                    if let Some(failed_parent) = fd.failed_ancestor(e.name()) {
                        if self.config.flags.verbose && self.config.connection.client_mode {
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
        //
        // Pre-compute feature flags to avoid per-file method dispatch on the
        // quick-check skip path. For a no-change scan (SSH push where all
        // files are up-to-date), this loop processes every file without
        // producing any transfer work, so minimising per-iteration overhead
        // is critical.
        //
        // upstream: generator.c generate_files() loop uses global variables
        // for the same purpose (preserve_perms, preserve_mtimes, etc.).
        let needs_metadata_apply = metadata_opts.requires_apply();
        let has_acl_cache = acl_cache.is_some();
        let has_xattrs = self.config.flags.xattrs;
        let emit_itemize = self.should_emit_itemize();

        let mut files_to_transfer = Vec::with_capacity(stat_results.len());
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
                    // upstream: generator.c:1814-1816 - quick-check matched:
                    // apply metadata (set_file_attrs) then itemize. Skip the
                    // chain entirely when no preservation flags are active.
                    // upstream: generator.c:461 unchanged_attrs() - fast-path
                    // check avoids apply_metadata overhead when all attributes
                    // already match.
                    if needs_metadata_apply && !metadata_unchanged(entry, metadata_opts, meta) {
                        if let Err(e) = apply_metadata_with_cached_stat(
                            &file_path,
                            entry,
                            metadata_opts,
                            dest_meta,
                        ) {
                            metadata_errors.push((file_path.clone(), e.to_string()));
                        }
                    }

                    // upstream: generator.c:574-576 - itemize() for up-to-date
                    // files. Only emitted when significant iflags are set; zero
                    // iflags (completely unchanged) produces no output.
                    if emit_itemize {
                        let iflags = crate::generator::ItemFlags::from_raw(0);
                        let _ = self.emit_itemize(writer, &iflags, entry);
                    }

                    if has_acl_cache {
                        if let Err(e) = apply_acls_from_receiver_cache(
                            &file_path,
                            entry,
                            acl_cache,
                            !entry.is_symlink(),
                        ) {
                            metadata_errors.push((file_path.clone(), e.to_string()));
                        }
                    }
                    if has_xattrs {
                        if let Some(ref xattr_list) = self.resolve_xattr_list(entry) {
                            // upstream: xattrs.c:set_xattr() - apply xattrs after metadata
                            if let Err(e) =
                                metadata::apply_xattrs_from_list(&file_path, xattr_list, true)
                            {
                                metadata_errors.push((file_path, e.to_string()));
                            }
                        }
                    }
                    continue;
                }
            } else {
                if existing_only {
                    continue;
                }
                if try_reference_dest(
                    entry,
                    dest_dir,
                    &self.config.reference_directories,
                    preserve_times,
                    size_only,
                    always_checksum,
                    metadata_opts,
                    metadata_errors,
                    acl_cache,
                ) {
                    continue;
                }
            }
            files_to_transfer.push((idx, entry, file_path));
        }
        files_to_transfer
    }
}
