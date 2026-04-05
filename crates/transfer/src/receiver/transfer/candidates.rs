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

use logging::info_log;
use metadata::{MetadataOptions, apply_metadata_with_cached_stat};
use protocol::flist::FileEntry;

use crate::receiver::directory::FailedDirectories;
use crate::receiver::quick_check::{
    dest_mtime_newer, is_hardlink_follower, quick_check_matches, try_reference_dest,
};
use crate::receiver::stats::TransferStats;
use crate::receiver::{PARALLEL_STAT_THRESHOLD, ReceiverContext, apply_acls_from_receiver_cache};

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
        // upstream: receiver.c:693 - dry_run (!do_xfers) skips all file transfers
        if self.config.flags.dry_run {
            return Vec::new();
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
                PARALLEL_STAT_THRESHOLD,
                move |(idx, file_path)| {
                    let meta = fs::metadata(&file_path).ok();
                    (idx, file_path, meta)
                },
            );

        // Phase C: Sequential post-processing with stat results.
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
                    // upstream: generator.c:2260 - itemize() for up-to-date files
                    let iflags = crate::generator::ItemFlags::from_raw(0);
                    let _ = self.emit_itemize(writer, &iflags, entry);
                    if let Err(e) =
                        apply_metadata_with_cached_stat(&file_path, entry, metadata_opts, dest_meta)
                    {
                        metadata_errors.push((file_path.clone(), e.to_string()));
                    }
                    if let Err(e) = apply_acls_from_receiver_cache(
                        &file_path,
                        entry,
                        acl_cache,
                        !entry.is_symlink(),
                    ) {
                        metadata_errors.push((file_path, e.to_string()));
                    } else if let Some(ref xattr_list) = self.resolve_xattr_list(entry) {
                        // upstream: xattrs.c:set_xattr() - apply xattrs after metadata
                        if let Err(e) =
                            metadata::apply_xattrs_from_list(&file_path, xattr_list, true)
                        {
                            metadata_errors.push((file_path, e.to_string()));
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
