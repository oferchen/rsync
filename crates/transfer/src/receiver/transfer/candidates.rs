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
use crate::receiver::stats::{ListOnlyEntry, TransferStats};
use crate::receiver::{ReceiverContext, apply_acls_from_receiver_cache};

impl ReceiverContext {
    /// Snapshots every active file-list entry for `--list-only` rendering.
    ///
    /// In list-only mode the receiver issues no per-file NDX request; it simply
    /// captures each entry's metadata so the client can print the upstream
    /// listing line (perms / size / date / name). Every active entry is emitted:
    /// directories (including the root `.`), symlinks, and regular/special files
    /// alike.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1249` - `list_file_entry()` renders one line per entry
    pub(in crate::receiver) fn collect_list_only_entries(&self) -> Vec<ListOnlyEntry> {
        self.file_list
            .iter()
            .map(|entry| {
                let is_symlink = entry.is_symlink();
                ListOnlyEntry {
                    path: entry.path().clone(),
                    mode: entry.mode(),
                    size: entry.size(),
                    mtime: entry.mtime(),
                    mtime_nsec: entry.mtime_nsec(),
                    // upstream: generator.c list_file_entry() renders F_ATIME(f)
                    // and F_CRTIME(f) when the atimes/crtimes ndx columns are
                    // active. The flist FileEntry carries no crtime nanosecond
                    // component, so crtime_nsec is always 0.
                    atime: entry.atime(),
                    atime_nsec: entry.atime_nsec(),
                    crtime: entry.crtime(),
                    crtime_nsec: 0,
                    symlink_target: if is_symlink {
                        entry.link_target().cloned()
                    } else {
                        None
                    },
                    is_symlink,
                }
            })
            .collect()
    }

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
    ) -> Vec<(usize, &'a FileEntry, PathBuf, u32)> {
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
        // to the sender, which logs each file name for verbose output. List-only
        // also skips the destination stat/quick-check; its caller never issues
        // per-file NDX requests (the list_only branch in `run_pipelined`).
        if self.config.flags.skip_dest_writes() {
            // upstream: generator.c:1925-1926 - dry-run still itemizes with
            // ITEM_TRANSFER; the dry-run loop writes the bare ITEM_TRANSFER
            // attrs over the wire and does not consume this precomputed value.
            return candidates
                .into_iter()
                .map(|(idx, entry)| {
                    (
                        idx,
                        entry,
                        dest_dir.join(entry.path()),
                        crate::generator::ItemFlags::ITEM_TRANSFER,
                    )
                })
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
                    // upstream: generator.c:1409 - `if (ignore_existing > 0 &&
                    // statret == 0 && (!is_dir || stype != FT_DIR)) { if
                    // (INFO_GTE(SKIP, 1) ...) rprintf(FINFO, "%s exists\n",
                    // fname); }`. An already-present file is skipped with a
                    // SKIP-gated notice; existing directories stay silent.
                    if !entry.is_dir() && logging::info_gte(logging::InfoFlag::Skip, 1) {
                        let name = entry.path().to_string_lossy();
                        let _ = self.emit_info_line(writer, &format!("{name} exists\n"));
                    }
                    continue;
                }
                if update_only && dest_mtime_newer(meta, entry) {
                    // upstream: generator.c:1723-1724 - `if (INFO_GTE(SKIP, 1))
                    // rprintf(FINFO, "%s is newer\n", fname)`. Report the skip on
                    // the same sink as itemize so the ordering matches upstream.
                    if logging::info_gte(logging::InfoFlag::Skip, 1) {
                        let name = entry.path().to_string_lossy();
                        let _ = self.emit_info_line(writer, &format!("{name} is newer\n"));
                    }
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
                    // upstream: generator.c:1816 - itemize() with iflags=0 for an
                    // up-to-date file; the attr-comparison may still surface a
                    // metadata-only row (perms/owner/group differing while
                    // size+mtime match).
                    let unchanged_iflags = self.itemize_existing_flags(entry, meta, 0);
                    self.apply_no_change_metadata(
                        writer,
                        &file_path,
                        entry,
                        meta,
                        metadata_opts,
                        metadata_errors,
                        acl_cache,
                        emit_itemize,
                        unchanged_iflags,
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
            // upstream: generator.c:504-572 itemize() - compute the base itemize
            // flags before the data transfer so the row reflects attribute
            // changes against the pre-transfer destination. A non-existent dest
            // (statret < 0) is ITEM_IS_NEW; an existing one OR-s the per-attr
            // report bits onto ITEM_TRANSFER.
            let base_iflags = match dest_meta {
                Some(ref meta) => self.itemize_existing_flags(
                    entry,
                    meta,
                    crate::generator::ItemFlags::ITEM_TRANSFER,
                ),
                None => {
                    crate::generator::ItemFlags::ITEM_TRANSFER
                        | crate::generator::ItemFlags::ITEM_IS_NEW
                }
            };
            // upstream: generator.c:1925-1937 - the generator emits the transfer
            // itemize right after write_ndx(ndx), in flist order. With
            // log_before_transfer (`!am_server`, i.e. client mode) the row is
            // written to stdout before the data moves, so emit it here in the
            // linear candidate pass to preserve the interleaving with the
            // skip/unchanged rows. Server-mode receivers defer to the pipeline
            // (after the transfer completes) to match `log_before_transfer == 0`.
            if emit_itemize && self.config.connection.client_mode {
                let iflags = crate::generator::ItemFlags::from_raw(base_iflags);
                let _ = self.emit_itemize(writer, &iflags, entry);
            }
            files_to_transfer.push((idx, entry, file_path, base_iflags));
        }
        files_to_transfer
    }

    /// Computes the attribute-comparison itemize flags for a destination file
    /// that already exists, mirroring upstream `generator.c:508-549` `itemize()`.
    ///
    /// `base` is `ITEM_TRANSFER` for a file being transferred, or `0` for an
    /// up-to-date file (quick-check match). The returned raw flags OR `base`
    /// with `ITEM_REPORT_{SIZE,TIME,PERMS,OWNER,GROUP}` for every attribute that
    /// differs between `entry` (the sender's view) and `dest_meta` (the
    /// pre-transfer destination stat). Only regular-file candidates reach this
    /// path, so `keep_time` reduces to the `--times` preservation flag.
    fn itemize_existing_flags(
        &self,
        entry: &FileEntry,
        dest_meta: &fs::Metadata,
        base: u32,
    ) -> u32 {
        use crate::generator::ItemFlags;
        let mut iflags = base;
        // upstream: generator.c:514 - S_ISREG(file->mode) && F_LENGTH(file) != st_size
        if entry.is_file() && entry.size() != dest_meta.len() {
            iflags |= ItemFlags::ITEM_REPORT_SIZE;
        }
        // upstream: generator.c:519-523 - keep_time ? mtime_differs(&st, file).
        // For regular files keep_time == preserve_mtimes (`--times`).
        if self.config.flags.times {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if dest_meta.mtime() != entry.mtime() {
                    iflags |= ItemFlags::ITEM_REPORT_TIME;
                }
            }
            #[cfg(not(unix))]
            {
                let dest_secs = dest_meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64);
                if dest_secs != Some(entry.mtime()) {
                    iflags |= ItemFlags::ITEM_REPORT_TIME;
                }
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            // upstream: generator.c:540-542 - preserve_perms && CHMOD_BITS differ
            if self.config.flags.perms {
                const CHMOD_BITS: u32 = 0o7777;
                if (dest_meta.mode() & CHMOD_BITS) != (entry.mode() & CHMOD_BITS) {
                    iflags |= ItemFlags::ITEM_REPORT_PERMS;
                }
            }
            // upstream: generator.c:546-547 - uid_ndx && am_root && uid differs
            if self.config.flags.owner && metadata::am_root() {
                if let Some(uid) = entry.uid() {
                    if dest_meta.uid() != uid {
                        iflags |= ItemFlags::ITEM_REPORT_OWNER;
                    }
                }
            }
            // upstream: generator.c:548-549 - gid_ndx && !FLAG_SKIP_GROUP && gid differs
            if self.config.flags.group {
                if let Some(gid) = entry.gid() {
                    if dest_meta.gid() != gid {
                        iflags |= ItemFlags::ITEM_REPORT_GROUP;
                    }
                }
            }
        }
        iflags
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
        unchanged_iflags: u32,
        has_acls: bool,
        has_xattrs: bool,
        needs_metadata_apply: bool,
    ) {
        // upstream: generator.c:1816 - itemize() for an up-to-date file. The
        // attr-comparison flags were computed against the pre-apply dest stat;
        // emit_itemize's own gate drops the row when nothing is significant
        // unless the itemize level requests unchanged rows (generator.c:574-576).
        if emit_itemize {
            let iflags = crate::generator::ItemFlags::from_raw(unchanged_iflags);
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
