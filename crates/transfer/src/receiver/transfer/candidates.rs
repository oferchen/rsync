//! File transfer candidate selection.
//!
//! Builds the list of files that need transfer by applying quick-check
//! heuristics, size bounds, failed directory tracking, and parallel stat.
//! Emits metadata-only itemize lines for up-to-date files.
//!
//! # Upstream Reference
//!
//! - `generator.c:recv_generator()` - per-file quick-check and skip logic
//! - `generator.c:954` - `try_dests_reg()` for reference directory handling
//! - `generator.c:624` - `quick_check_ok()` evaluation order

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use logging::{debug_gte, debug_log, info_log};
use metadata::{MetadataOptions, apply_metadata_with_cached_stat, metadata_unchanged};
use protocol::flist::FileEntry;

use crate::receiver::directory::FailedDirectories;
use crate::receiver::quick_check::{
    dest_mtime_newer, dest_type_matches_source, is_hardlink_follower, quick_check_matches,
    try_reference_dest,
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
    #[allow(clippy::too_many_arguments)]
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
        acl_id_map: Option<&metadata::AclIdMapper>,
    ) -> Vec<(usize, &'a FileEntry, PathBuf, u32)> {
        // upstream: generator.c:1246-1247 - "recv_generator(%s,%d)" emitted at
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
                // upstream: receiver.c:711-716 - check_filter(&daemon_filter_list, ...)
                // rejects daemon-excluded files before accepting transfer data.
                if has_daemon_filters {
                    let filters =
                        daemon_filters.expect("daemon_filters is Some when has_daemon_filters");
                    let name = e.name();
                    if name != "." && !filters.allows(Path::new(name), false) {
                        stats.files_skipped += 1;
                        return false;
                    }
                }
                if has_failed_dirs {
                    let fd = failed_dirs.expect("failed_dirs is Some when has_failed_dirs");
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

        // upstream: generator.c:1858 - dry_run (!do_xfers) skips stat and data
        // transfer but still builds the candidate list so NDX requests are sent
        // to the sender, which logs each file name for verbose output. List-only
        // also skips the destination stat/quick-check; its caller never issues
        // per-file NDX requests (the list_only branch in `run_pipelined`).
        if self.config.flags.skip_dest_writes() {
            // upstream: generator.c:1938-1939 - dry-run still itemizes with
            // ITEM_TRANSFER; the dry-run loop writes the bare ITEM_TRANSFER
            // attrs over the wire and does not consume this precomputed value.
            return candidates
                .into_iter()
                .filter(|(_, entry)| {
                    // upstream: generator.c:1704-1718 - the max/min-size skip
                    // (`goto cleanup`) fires before the `do_xfers` gate, so a
                    // dry run still excludes out-of-range files and emits the
                    // SKIP-gated notice in flist order.
                    !has_size_bounds
                        || !self.emit_size_bound_skip(writer, entry, min_size, max_size)
                })
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
        // upstream: generator.c:quick_check_ok() -> same_time() honours the
        // `--modify-window` tolerance for every transfer, not just local copies.
        let modify_window = self.config.file_selection.modify_window;
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
            // upstream: generator.c:2348-2353 generate_files() - the per-file
            // generate loop pokes maybe_send_keepalive once the I/O lull has
            // elapsed so a remote sender's --timeout does not fire while the
            // generator quick-checks a long run of up-to-date files without
            // writing any NDX request. A strict no-op unless --timeout is set
            // (allowed_lull None), keeping the default path wire-identical.
            let _ = writer.maybe_send_keepalive();
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
                        // upstream: generator.c:1398-1408 - the notice is
                        // "%s exists%s"; the suffix is empty at SKIP1 and gains
                        // a parenthesised reason (type/sum/file/attr change or
                        // uptodate) at SKIP2.
                        let suffix = self.ignore_existing_suffix(
                            entry,
                            &file_path,
                            meta,
                            preserve_times,
                            size_only,
                            always_checksum,
                            modify_window,
                            metadata_opts,
                        );
                        let _ = self.emit_info_line(writer, &format!("{name} exists{suffix}\n"));
                    }
                    continue;
                }
                // upstream: generator.c:1704-1718 - the max/min-size skip is
                // tested per file after the `--ignore-existing` "exists" notice
                // (1395) and before the `--update` "is newer" notice (1721), so
                // the size notices interleave with the other skip notices in
                // strict flist order rather than as a separate batch.
                if has_size_bounds && self.emit_size_bound_skip(writer, entry, min_size, max_size) {
                    continue;
                }
                if update_only
                    && dest_type_matches_source(&file_path, entry)
                    && dest_mtime_newer(meta, entry)
                {
                    // upstream: generator.c:1721 - the `-u` skip is guarded by
                    // `stype == ftype`, so a newer destination only suppresses
                    // the transfer when it is the SAME file type as the source.
                    // A type mismatch (e.g. dest symlink vs source regular file)
                    // always transfers regardless of mtime.
                    //
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
                    modify_window,
                ) {
                    // upstream: generator.c:1816 - itemize() with iflags=0 for an
                    // up-to-date file; the attr-comparison may still surface a
                    // metadata-only row (perms/owner/group differing while
                    // size+mtime match).
                    let unchanged_iflags = self.itemize_existing_flags(entry, meta, 0);
                    self.apply_no_change_metadata(
                        writer,
                        idx,
                        &file_path,
                        entry,
                        meta,
                        metadata_opts,
                        metadata_errors,
                        acl_cache,
                        acl_id_map,
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
                    // upstream: generator.c:1380-1395 - --existing /
                    // --ignore-non-existing never creates an absent
                    // destination; a missing regular file is skipped with a
                    // SKIP-gated "not creating new file" notice. Directories
                    // take the same path in receiver/directory/creation.rs.
                    if logging::info_gte(logging::InfoFlag::Skip, 1) {
                        let name = entry.path().to_string_lossy();
                        let _ = self
                            .emit_info_line(writer, &format!("not creating new file \"{name}\"\n"));
                    }
                    continue;
                }
                // upstream: generator.c:1704-1718 - a not-yet-existing file
                // still hits the max/min-size skip (after the not-creating
                // check at 1368), so the size notice for an absent file appears
                // in flist order alongside the other per-file notices.
                if has_size_bounds && self.emit_size_bound_skip(writer, entry, min_size, max_size) {
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
                        modify_window,
                        self.config.flags.copy_links,
                        metadata_opts,
                        metadata_errors,
                        acl_cache,
                        acl_id_map,
                    )
                {
                    continue;
                }
            }
            // upstream: generator.c:511-579 itemize() - compute the base itemize
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
            if base_iflags & crate::generator::ItemFlags::ITEM_IS_NEW != 0 {
                // upstream: receiver.c:777-778 - a regular file being received
                // whose destination was absent (ITEM_IS_NEW) bumps
                // stats.created_files; reg is the implicit remainder of the
                // "Number of created files" breakdown. Counts a new empty file
                // too, since it is still requested and materialised.
                self.record_created(entry.mode());
            }
            // upstream: generator.c:1938-1950 - the generator emits the transfer
            // itemize right after write_ndx(ndx), in flist order. With
            // log_before_transfer (`!am_server`, i.e. client mode) the row is
            // written to stdout before the data moves, so emit it here in the
            // linear candidate pass to preserve the interleaving with the
            // skip/unchanged rows. Server-mode receivers defer to the pipeline
            // (after the transfer completes) to match `log_before_transfer == 0`.
            if emit_itemize && self.config.connection.client_mode {
                let iflags = crate::generator::ItemFlags::from_raw(base_iflags);
                // Deferred on the run_pipelined path so this transfer row
                // interleaves with directory rows in flist-index order at flush
                // time; emitted immediately on every other path.
                let _ = self.emit_or_record_itemize(writer, idx, &iflags, entry);
            }
            files_to_transfer.push((idx, entry, file_path, base_iflags));
        }
        files_to_transfer
    }

    /// Records the deferred itemize rows for a `--dry-run` receive, one per
    /// file-list entry in flist-index order.
    ///
    /// Upstream runs the identical `recv_generator()` per-entry loop under
    /// `--dry-run`; only the data transfer and the filesystem mutation are
    /// suppressed (`set_file_attrs()` returns early when `dry_run`, rsync.c;
    /// `do_mkdir`/`do_open` sit behind `if (!do_xfers) goto notify_others`).
    /// The `itemize()` call itself always runs, so `-ni` prints a row for every
    /// entry (`>f+++++++++`, `cd+++++++++`, `cL+++++++++`, ...). oc's shipped
    /// remote receive path (`run_pipelined`) instead early-returns out of the
    /// directory-creation and candidate passes when `skip_dest_writes()` is set,
    /// so no itemize row was ever recorded on a network dry run. This read-only
    /// pass restores the rows without writing anything to the destination.
    ///
    /// Recording (not immediate emission) keeps the rows interleaved in
    /// flist-index order via the deferred flush (`flush_itemize_rows`), matching
    /// upstream's single flist-index-order walk. `record_itemize` already gates
    /// on `should_emit_itemize() && client_mode`, so this is a no-op for a plain
    /// `-n` (no `-i`) and for a server-mode receiver (a push dry run, whose rows
    /// travel as wire iflags and are printed by the client's sender).
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1481-1483` - directory `itemize()` (runs under dry-run).
    /// - `generator.c:1935-1947` - regular-file `itemize()` at `notify_others`,
    ///   reached even when `!do_xfers`.
    /// - `generator.c:1594` / `generator.c:1462` - symlink / special `itemize()`.
    /// - `rsync.c` `set_file_attrs()` - `if (dry_run) return 1;` (no mutation).
    pub(in crate::receiver) fn record_dry_run_itemize(&self, dest_dir: &Path) {
        if !self.should_emit_itemize() || !self.config.connection.client_mode {
            return;
        }
        let preserve_times = self.config.flags.times && !self.config.flags.ignore_times;
        let size_only = self.config.file_selection.size_only;
        let modify_window = self.config.file_selection.modify_window;
        let always_checksum = if self.config.flags.checksum {
            Some(self.get_checksum_algorithm())
        } else {
            None
        };
        for (idx, entry) in self.file_list.iter().enumerate() {
            let rel = entry.path();
            let dest_path = if rel.as_os_str() == "." {
                dest_dir.to_path_buf()
            } else {
                dest_dir.join(rel)
            };
            let iflags = crate::generator::ItemFlags::from_raw(self.dry_run_entry_iflags(
                entry,
                &dest_path,
                preserve_times,
                size_only,
                always_checksum,
                modify_window,
            ));
            self.record_itemize(idx, &iflags, entry);
        }
    }

    /// Computes the itemize flags a single entry would carry on a `--dry-run`
    /// receive, by comparing the sender's file-list entry against the current
    /// (pre-transfer) destination via a read-only `lstat`.
    ///
    /// The classification mirrors the non-dry-run record sites so a dry run
    /// predicts exactly what the real transfer would print: a new entry
    /// (destination absent) is `ITEM_IS_NEW`; an existing one OR-s the per-attr
    /// report bits computed by [`Self::itemize_existing_flags`] /
    /// [`Self::existing_dir_iflags`]. `render_itemize_line`'s significance gate
    /// then drops an all-unchanged existing entry, matching upstream.
    fn dry_run_entry_iflags(
        &self,
        entry: &FileEntry,
        dest_path: &Path,
        preserve_times: bool,
        size_only: bool,
        always_checksum: Option<protocol::ChecksumAlgorithm>,
        modify_window: metadata::ModifyWindow,
    ) -> u32 {
        use crate::generator::ItemFlags;
        let new_iflags = |base: u32| base | ItemFlags::ITEM_IS_NEW;
        if entry.is_dir() {
            // upstream: generator.c:1481-1483 - new dir -> ITEM_LOCAL_CHANGE |
            // ITEM_IS_NEW; existing dir -> itemize() attribute diff.
            match fs::metadata(dest_path) {
                Ok(_) => self.existing_dir_iflags(entry, dest_path),
                Err(_) => new_iflags(ItemFlags::ITEM_LOCAL_CHANGE),
            }
        } else if entry.is_symlink() {
            // upstream: generator.c:1561-1594 - an up-to-date symlink (same
            // target) is metadata-only; an absent, obstructed, or re-pointed one
            // is (re)created and itemized ITEM_LOCAL_CHANGE (+ ITEM_IS_NEW when
            // absent). Mirror the receiver's own create_symlinks classification.
            match fs::symlink_metadata(dest_path) {
                Ok(meta) if meta.file_type().is_symlink() => match fs::read_link(dest_path) {
                    Ok(target) if entry.link_target() == Some(&target) => 0,
                    _ => new_iflags(ItemFlags::ITEM_LOCAL_CHANGE),
                },
                Ok(_) => new_iflags(ItemFlags::ITEM_LOCAL_CHANGE),
                Err(_) => new_iflags(ItemFlags::ITEM_LOCAL_CHANGE),
            }
        } else if entry.is_device() || entry.is_special() {
            // upstream: generator.c:1462 - a node newly materialised via do_mknod
            // is ITEM_IS_NEW; an existing node of the same type is metadata-only.
            match fs::symlink_metadata(dest_path) {
                Ok(_) => 0,
                Err(_) => new_iflags(ItemFlags::ITEM_LOCAL_CHANGE),
            }
        } else {
            // upstream: generator.c:1935-1947 - regular file at notify_others:
            // absent -> ITEM_TRANSFER | ITEM_IS_NEW; present and quick-check
            // match -> itemize(...,0,...); present and differing -> ITEM_TRANSFER
            // plus the attribute diff.
            match fs::symlink_metadata(dest_path) {
                Ok(meta) => {
                    let base = if quick_check_matches(
                        entry,
                        dest_path,
                        &meta,
                        preserve_times,
                        size_only,
                        always_checksum,
                        modify_window,
                    ) {
                        0
                    } else {
                        ItemFlags::ITEM_TRANSFER
                    };
                    self.itemize_existing_flags(entry, &meta, base)
                }
                Err(_) => new_iflags(ItemFlags::ITEM_TRANSFER),
            }
        }
    }

    /// Computes the attribute-comparison itemize flags for a destination file
    /// that already exists, mirroring upstream `generator.c:515-556` `itemize()`.
    ///
    /// `base` is `ITEM_TRANSFER` for a file being transferred, or `0` for an
    /// up-to-date file (quick-check match). The returned raw flags OR `base`
    /// with `ITEM_REPORT_{SIZE,TIME,PERMS,OWNER,GROUP}` for every attribute that
    /// differs between `entry` (the sender's view) and `dest_meta` (the
    /// pre-transfer destination stat). Both regular files (quick-check match)
    /// and existing directories reach this path: the `ITEM_REPORT_SIZE` check
    /// is gated on `entry.is_file()` so it never fires for a directory, and
    /// `keep_time` reduces to the `--times` preservation flag in both cases
    /// (`--omit-dir-times` is not modelled by the server flag set, matching
    /// [`super::super::directory::creation`]'s `touch_up_dirs`).
    pub(in crate::receiver) fn itemize_existing_flags(
        &self,
        entry: &FileEntry,
        dest_meta: &fs::Metadata,
        base: u32,
    ) -> u32 {
        use crate::generator::ItemFlags;
        let mut iflags = base;
        // upstream: generator.c:521 - S_ISREG(file->mode) && F_LENGTH(file) != st_size
        if entry.is_file() && entry.size() != dest_meta.len() {
            iflags |= ItemFlags::ITEM_REPORT_SIZE;
        }
        // upstream: generator.c:526-530 - keep_time ? mtime_differs(&st, file).
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
            // upstream: generator.c:547-549 - preserve_perms && CHMOD_BITS differ
            if self.config.flags.perms {
                const CHMOD_BITS: u32 = 0o7777;
                if (dest_meta.mode() & CHMOD_BITS) != (entry.mode() & CHMOD_BITS) {
                    iflags |= ItemFlags::ITEM_REPORT_PERMS;
                }
            }
            // upstream: generator.c:553-554 - uid_ndx && am_root && uid differs
            if self.config.flags.owner && metadata::am_root() {
                if let Some(uid) = entry.uid() {
                    if dest_meta.uid() != uid {
                        iflags |= ItemFlags::ITEM_REPORT_OWNER;
                    }
                }
            }
            // upstream: generator.c:555-556 - gid_ndx && !FLAG_SKIP_GROUP && gid differs
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

    /// Emits the upstream size-bound SKIP notice for a candidate whose flist
    /// length is outside the `--min-size`/`--max-size` window, returning `true`
    /// when the entry is filtered out.
    ///
    /// Over max-size is tested before under min-size, matching upstream's
    /// evaluation order. The notice text and `INFO_GTE(SKIP,1)` gate mirror
    /// upstream exactly; below the gate the entry is still skipped silently.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1704-1711` - `"%s is over max-size\n"`
    /// - `generator.c:1712-1718` - `"%s is under min-size\n"`
    pub(in crate::receiver) fn emit_size_bound_skip<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        writer: &mut W,
        entry: &FileEntry,
        min_size: Option<u64>,
        max_size: Option<u64>,
    ) -> bool {
        let size = entry.size();
        if let Some(max) = max_size {
            if size > max {
                if logging::info_gte(logging::InfoFlag::Skip, 1) {
                    let name = entry.path().to_string_lossy();
                    let _ = self.emit_info_line(writer, &format!("{name} is over max-size\n"));
                }
                return true;
            }
        }
        if let Some(min) = min_size {
            if size < min {
                if logging::info_gte(logging::InfoFlag::Skip, 1) {
                    let name = entry.path().to_string_lossy();
                    let _ = self.emit_info_line(writer, &format!("{name} is under min-size\n"));
                }
                return true;
            }
        }
        false
    }

    /// Computes the parenthesised reason suffix for the `--ignore-existing`
    /// `"%s exists%s"` notice.
    ///
    /// Empty unless `INFO_GTE(SKIP,2)`; at SKIP2 it classifies why the existing
    /// destination is being kept, reusing oc's already-computed compare
    /// primitives so the mapping tracks the upstream decision cascade exactly.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1399-1408` - suffix selection cascade (type change ->
    ///   sum/file change -> attr change -> uptodate)
    #[allow(clippy::too_many_arguments)]
    fn ignore_existing_suffix(
        &self,
        entry: &FileEntry,
        dest_path: &Path,
        dest_meta: &fs::Metadata,
        preserve_times: bool,
        size_only: bool,
        always_checksum: Option<protocol::ChecksumAlgorithm>,
        modify_window: metadata::ModifyWindow,
        metadata_opts: &MetadataOptions,
    ) -> &'static str {
        if !logging::info_gte(logging::InfoFlag::Skip, 2) {
            return "";
        }
        if !dest_type_matches_source(dest_path, entry) {
            " (type change)"
        } else if !quick_check_matches(
            entry,
            dest_path,
            dest_meta,
            preserve_times,
            size_only,
            always_checksum,
            modify_window,
        ) {
            if always_checksum.is_some() {
                " (sum change)"
            } else {
                " (file change)"
            }
        } else if !metadata_unchanged(entry, metadata_opts, dest_meta) {
            " (attr change)"
        } else {
            " (uptodate)"
        }
    }

    /// Applies metadata updates for a file that passed quick-check (no transfer needed).
    ///
    /// This is the hot path for no-change scans at scale. Each guard check
    /// avoids a function call and potential syscalls when the corresponding
    /// feature is disabled.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1827` - `set_file_attrs()` on quick-check match
    /// - `generator.c:1816` - `itemize()` on quick-check match
    #[allow(clippy::too_many_arguments)]
    fn apply_no_change_metadata<W: Write + crate::writer::MsgInfoSender + ?Sized>(
        &self,
        writer: &mut W,
        flist_idx: usize,
        file_path: &Path,
        entry: &FileEntry,
        stat_meta: &fs::Metadata,
        metadata_opts: &MetadataOptions,
        metadata_errors: &mut Vec<(PathBuf, String)>,
        acl_cache: Option<&protocol::acl::AclCache>,
        acl_id_map: Option<&metadata::AclIdMapper>,
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
            // Deferred on the run_pipelined path so an up-to-date file's
            // metadata-only row interleaves with directory and transfer rows in
            // flist-index order at flush time; emitted immediately otherwise.
            let _ = self.emit_or_record_itemize(writer, flist_idx, &iflags, entry);
        }

        // upstream: generator.c:468 unchanged_attrs() - fast-path check avoids
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
            if let Err(e) = apply_acls_from_receiver_cache(
                file_path,
                entry,
                acl_cache,
                acl_id_map,
                !entry.is_symlink(),
            ) {
                metadata_errors.push((file_path.to_path_buf(), e.to_string()));
                return;
            }
        }

        // upstream: xattrs.c:set_xattr() - apply xattrs after metadata
        if has_xattrs {
            if let Some(ref xattr_list) = self.resolve_xattr_list(entry) {
                let filter = self
                    .xattr_name_filter()
                    .map(|set| move |name: &str| set.xattr_name_allowed(name));
                let filter_ref = filter.as_ref().map(|f| f as &dyn Fn(&str) -> bool);
                if let Err(e) =
                    metadata::apply_xattrs_from_list(file_path, xattr_list, true, filter_ref)
                {
                    metadata_errors.push((file_path.to_path_buf(), e.to_string()));
                }
            }
        }
    }
}

#[cfg(test)]
mod itemize_order_tests {
    use std::ffi::OsString;

    use protocol::ProtocolVersion;
    use protocol::flist::FileEntry;

    use crate::config::ServerConfig;
    use crate::flags::{InfoFlags, ParsedServerFlags};
    use crate::handshake::HandshakeResult;
    use crate::receiver::ReceiverContext;
    use crate::receiver::stats::TransferStats;
    use crate::role::ServerRole;

    fn handshake() -> HandshakeResult {
        HandshakeResult {
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
        }
    }

    /// A client-mode pull receiver with `-i` (itemize) requested.
    fn itemize_client_config() -> ServerConfig {
        let mut config = ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-ri".to_owned(),
            flags: ParsedServerFlags {
                recursive: true,
                info_flags: InfoFlags {
                    itemize: true,
                    ..InfoFlags::default()
                },
                ..ParsedServerFlags::default()
            },
            args: vec![OsString::from(".")],
            ..Default::default()
        };
        config.connection.client_mode = true;
        config
    }

    /// The deferred flush must interleave directory and file itemize rows in
    /// flist-index order (a dir row immediately precedes its children), not
    /// batch every directory ahead of every file.
    ///
    /// Upstream itemizes in a single flist-index-order walk: `generate_files`
    /// (generator.c:2329-2344) calls `recv_generator` per `cur_flist->sorted[i]`
    /// in index order, and `recv_generator` (generator.c:1480-1483) itemizes
    /// each directory at its own flist position. For the flist
    /// `a/ a/f1 b/ b/f2` upstream prints `.d a/`, `>f a/f1`, `.d b/`, `>f b/f2`.
    /// oc's two-phase receiver recorded every `.d` row in the directory-creation
    /// pass before any `>f` row from the candidate pass, so a raw emission would
    /// yield `.d a/`, `.d b/`, `>f a/f1`, `>f b/f2`. Keying each row by its flist
    /// index and draining the BTreeMap in key order restores the interleave.
    ///
    /// This test drives the real `run_pipelined` record sites
    /// (`create_directories` + `build_files_to_transfer`) with deferral on and
    /// asserts the buffered rows land in index order. It fails if the batch
    /// emission returns: reverting to an immediate emit leaves the buffer empty
    /// (0 rows), and recording without the per-index key would order the two
    /// directory rows ahead of the two file rows.
    #[test]
    fn deferred_itemize_rows_interleave_in_flist_index_order() {
        let dir = test_support::create_tempdir();
        let dest = dir.path();

        let hs = handshake();
        let mut ctx = ReceiverContext::new_for_test(&hs, itemize_client_config());
        ctx.defer_itemize = true;
        ctx.file_list = vec![
            FileEntry::new_directory("a".into(), 0o755),  // idx 0
            FileEntry::new_file("a/f1".into(), 5, 0o644), // idx 1
            FileEntry::new_directory("b".into(), 0o755),  // idx 2
            FileEntry::new_file("b/f2".into(), 5, 0o644), // idx 3
        ];

        let opts = metadata::MetadataOptions::default();
        let mut writer = crate::writer::ServerWriter::new_plain(Vec::new());

        // Directory-creation pass records the `.d` rows (flist indices 0 and 2).
        ctx.create_directories(
            dest,
            &opts,
            None,
            None,
            &mut writer,
            #[cfg(unix)]
            None,
        )
        .expect("create_directories succeeds");

        // Candidate pass records the new-file transfer rows (indices 1 and 3).
        let mut metadata_errors = Vec::new();
        let mut stats = TransferStats::default();
        let _ = ctx.build_files_to_transfer(
            &mut writer,
            dest,
            &opts,
            None,
            &mut metadata_errors,
            &mut stats,
            None,
            None,
        );

        let rows: Vec<(usize, String)> = ctx
            .itemize_rows
            .borrow()
            .iter()
            .map(|(idx, lines)| (*idx, lines[0].clone()))
            .collect();

        let keys: Vec<usize> = rows.iter().map(|(idx, _)| *idx).collect();
        assert_eq!(
            keys,
            vec![0, 1, 2, 3],
            "itemize rows must be keyed by flist index and drain in index order"
        );

        // Interleaved dir/file/dir/file, not batched dir/dir/file/file.
        assert!(
            rows[0].1.starts_with("cd") && rows[0].1.contains('a'),
            "row 0 must be the created directory a/: {:?}",
            rows[0].1
        );
        assert!(
            rows[1].1.starts_with(">f") && rows[1].1.contains("a/f1"),
            "row 1 must be the new file a/f1 (before b/), not the b/ directory: {:?}",
            rows[1].1
        );
        assert!(
            rows[2].1.starts_with("cd") && rows[2].1.contains('b'),
            "row 2 must be the created directory b/ AFTER a/f1: {:?}",
            rows[2].1
        );
        assert!(
            rows[3].1.starts_with(">f") && rows[3].1.contains("b/f2"),
            "row 3 must be the new file b/f2: {:?}",
            rows[3].1
        );
    }

    /// A `--dry-run` remote pull must itemize every file-list entry, matching
    /// upstream's per-entry `itemize()` which runs even when `!do_xfers`. The
    /// shipped receive path early-returns out of the directory-creation and
    /// candidate passes under `skip_dest_writes()`, so before the fix `-ni`
    /// printed zero itemize lines over the network. `record_dry_run_itemize`
    /// records one deferred row per entry (in flist-index order) without writing
    /// anything to the destination.
    ///
    /// upstream: generator.c:1481-1483 / :1935-1947 / :1594 - itemize() under
    /// dry-run; rsync.c set_file_attrs() returns early when dry_run (no mutation).
    #[test]
    fn dry_run_itemize_records_a_row_per_entry_without_writing() {
        let dir = test_support::create_tempdir();
        let dest = dir.path();

        let hs = handshake();
        let mut ctx = ReceiverContext::new_for_test(&hs, itemize_client_config());
        ctx.defer_itemize = true;
        ctx.file_list = vec![
            FileEntry::new_directory("d".into(), 0o755),  // idx 0
            FileEntry::new_file("d/f1".into(), 5, 0o644), // idx 1
            FileEntry::new_symlink("d/lnk".into(), "target".into()), // idx 2
        ];

        // Read-only pass against an empty destination: every entry is new.
        ctx.record_dry_run_itemize(dest);

        let rows: Vec<(usize, String)> = ctx
            .itemize_rows
            .borrow()
            .iter()
            .map(|(idx, lines)| (*idx, lines[0].clone()))
            .collect();

        let keys: Vec<usize> = rows.iter().map(|(idx, _)| *idx).collect();
        assert_eq!(
            keys,
            vec![0, 1, 2],
            "one itemize row per entry, in flist-index order"
        );
        assert!(
            rows[0].1.starts_with("cd") && rows[0].1.contains('d'),
            "new directory row: {:?}",
            rows[0].1
        );
        assert!(
            rows[1].1.starts_with(">f") && rows[1].1.contains("d/f1"),
            "new regular-file row: {:?}",
            rows[1].1
        );
        assert!(
            rows[2].1.starts_with("cL") && rows[2].1.contains("d/lnk"),
            "new symlink row: {:?}",
            rows[2].1
        );

        // The pass writes nothing: the destination stays empty.
        let entries: Vec<_> = std::fs::read_dir(dest)
            .expect("dest readable")
            .collect::<Result<Vec<_>, _>>()
            .expect("dest entries");
        assert!(
            entries.is_empty(),
            "dry-run itemize must not create any destination entry"
        );
    }

    /// Without `-i`, the dry-run itemize pass records nothing (the bare `-v`
    /// name path handles verbose output instead), and a server-mode receiver
    /// (a push dry run) records nothing here either - its rows travel as wire
    /// iflags printed by the client's sender.
    #[test]
    fn dry_run_itemize_is_noop_without_itemize_flag() {
        let dir = test_support::create_tempdir();
        let dest = dir.path();

        let hs = handshake();
        let mut config = itemize_client_config();
        config.flags.info_flags.itemize = false;
        let mut ctx = ReceiverContext::new_for_test(&hs, config);
        ctx.defer_itemize = true;
        ctx.file_list = vec![FileEntry::new_file("f".into(), 1, 0o644)];

        ctx.record_dry_run_itemize(dest);

        assert!(
            ctx.itemize_rows.borrow().is_empty(),
            "no itemize rows recorded without -i"
        );
    }
}

/// Receiver SKIP-notice fidelity: the generator emits `rprintf(FINFO, ...)`
/// chatter for files it declines to transfer. These tests pin the exact
/// upstream text and the `INFO_GTE(SKIP, N)` gate for each notice, and assert
/// silence below the gate.
#[cfg(test)]
mod skip_notice_tests {
    use std::ffi::OsString;
    use std::io;
    use std::path::Path;

    use logging::{InfoFlag, VerbosityConfig};
    use metadata::MetadataOptions;
    use protocol::ProtocolVersion;
    use protocol::flist::FileEntry;

    use crate::config::ServerConfig;
    use crate::flags::ParsedServerFlags;
    use crate::handshake::HandshakeResult;
    use crate::receiver::ReceiverContext;
    use crate::receiver::stats::TransferStats;
    use crate::role::ServerRole;

    /// Records every `MSG_INFO` frame emitted by a server-mode receiver so the
    /// tests can assert the exact skip-notice bytes.
    #[derive(Default)]
    struct CaptureWriter {
        lines: Vec<String>,
    }

    impl io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl crate::writer::MsgInfoSender for CaptureWriter {
        fn send_msg_info(&mut self, data: &[u8]) -> io::Result<()> {
            self.lines.push(String::from_utf8_lossy(data).into_owned());
            Ok(())
        }
    }

    fn handshake() -> HandshakeResult {
        HandshakeResult {
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
        }
    }

    /// A server-mode receiver: skip notices route through `send_msg_info` (the
    /// `MSG_INFO` sink) instead of the client's stdout, so `CaptureWriter` sees
    /// them verbatim.
    fn server_config() -> ServerConfig {
        let mut config = ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-r".to_owned(),
            flags: ParsedServerFlags {
                recursive: true,
                ..ParsedServerFlags::default()
            },
            args: vec![OsString::from(".")],
            ..Default::default()
        };
        config.connection.client_mode = false;
        config
    }

    /// Runs the candidate pass at a given `--info=skip` level and returns the
    /// captured notice lines.
    fn run(
        config: ServerConfig,
        skip_level: u8,
        files: Vec<FileEntry>,
        dest: &Path,
    ) -> Vec<String> {
        let mut cfg = VerbosityConfig::default();
        cfg.info.set(InfoFlag::Skip, skip_level);
        logging::init(cfg);

        let hs = handshake();
        let mut ctx = ReceiverContext::new_for_test(&hs, config);
        ctx.file_list = files;

        let mut writer = CaptureWriter::default();
        let opts = MetadataOptions::default();
        let mut errs = Vec::new();
        let mut stats = TransferStats::default();
        let _ = ctx.build_files_to_transfer(
            &mut writer,
            dest,
            &opts,
            None,
            &mut errs,
            &mut stats,
            None,
            None,
        );
        writer.lines
    }

    /// #46 - upstream: generator.c:1704-1718. A file over `--max-size` or under
    /// `--min-size` is skipped with `"%s is over max-size"` / `"%s is under
    /// min-size"`, gated on `INFO_GTE(SKIP, 1)`. Below the gate the skip is
    /// silent. The order (over-max before under-min) mirrors upstream.
    #[test]
    fn size_bound_notices_match_upstream_text_and_gate() {
        let dir = test_support::create_tempdir();
        let dest = dir.path();
        let files = || {
            vec![
                FileEntry::new_file("big".into(), 200, 0o644),
                FileEntry::new_file("small".into(), 5, 0o644),
                FileEntry::new_file("ok".into(), 50, 0o644),
            ]
        };
        let cfg = || {
            let mut c = server_config();
            c.file_selection.max_file_size = Some(100);
            c.file_selection.min_file_size = Some(10);
            c
        };

        // SKIP1: both out-of-bounds files are named; the in-bounds file is not.
        let lines = run(cfg(), 1, files(), dest);
        assert_eq!(
            lines,
            vec![
                "big is over max-size\n".to_owned(),
                "small is under min-size\n".to_owned(),
            ]
        );

        // Below the gate the skip is silent (business rule: no chatter without
        // -vv / --info=skip).
        assert!(run(cfg(), 0, files(), dest).is_empty());
    }

    /// #44 - upstream: generator.c:1380-1395. With `--existing`, a regular file
    /// absent at the destination is never created; upstream prints `not
    /// creating new file "%s"` (literal quotes) at `INFO_GTE(SKIP, 1)`, silent
    /// otherwise.
    #[test]
    fn not_creating_new_file_notice_match_upstream_text_and_gate() {
        let dir = test_support::create_tempdir();
        let dest = dir.path();
        let files = || vec![FileEntry::new_file("newfile".into(), 10, 0o644)];
        let cfg = || {
            let mut c = server_config();
            c.file_selection.existing_only = true;
            c
        };

        let lines = run(cfg(), 1, files(), dest);
        assert_eq!(
            lines,
            vec!["not creating new file \"newfile\"\n".to_owned()]
        );

        assert!(run(cfg(), 0, files(), dest).is_empty());
    }

    /// #81 - upstream: generator.c:1368-1719. `recv_generator` tests every
    /// per-file skip in one strictly sequential pass, so the max/min-size skip
    /// (1704-1718) is evaluated right after the `--existing` not-creating check
    /// (1368) for the *same* file. The notices therefore interleave in flist
    /// order; a size notice must never batch ahead of a not-creating notice for
    /// an earlier file. This matters because drop-in tools parse rsync's output
    /// stream line-by-line in order: a reordered notice block changes the
    /// observable transcript even though every individual line is correct.
    #[test]
    fn skip_notices_interleave_in_flist_order() {
        let dir = test_support::create_tempdir();
        let dest = dir.path();
        // `b_big` / `d_small` must exist at the destination so `--existing`
        // routes them to the size check (Some branch) rather than emitting a
        // not-creating notice; `a_new` / `e_new` stay absent.
        std::fs::write(dest.join("b_big"), b"x").expect("seed dest b_big");
        std::fs::write(dest.join("d_small"), b"x").expect("seed dest d_small");
        let files = || {
            vec![
                FileEntry::new_file("a_new".into(), 50, 0o644),
                FileEntry::new_file("b_big".into(), 200, 0o644),
                FileEntry::new_file("d_small".into(), 5, 0o644),
                FileEntry::new_file("e_new".into(), 50, 0o644),
            ]
        };
        let cfg = || {
            let mut c = server_config();
            c.file_selection.existing_only = true;
            c.file_selection.max_file_size = Some(100);
            c.file_selection.min_file_size = Some(10);
            c
        };

        let lines = run(cfg(), 1, files(), dest);
        assert_eq!(
            lines,
            vec![
                "not creating new file \"a_new\"\n".to_owned(),
                "b_big is over max-size\n".to_owned(),
                "d_small is under min-size\n".to_owned(),
                "not creating new file \"e_new\"\n".to_owned(),
            ]
        );
    }

    /// #45 - upstream: generator.c:1395-1410. With `--ignore-existing`, a file
    /// already present is skipped with `"%s exists%s"`. At SKIP1 the suffix is
    /// empty; at SKIP2 it names the reason. The suffix must NOT leak at SKIP1
    /// (else oc would out-chatter upstream).
    #[test]
    fn ignore_existing_notice_suffix_matches_upstream_per_level() {
        let dir = test_support::create_tempdir();
        let dest = dir.path();
        std::fs::write(dest.join("exists.txt"), b"12345").expect("seed dest file (5 bytes)");

        // Source length differs from the 5-byte destination.
        let files = || vec![FileEntry::new_file("exists.txt".into(), 10, 0o644)];
        let base_cfg = || {
            let mut c = server_config();
            c.file_selection.ignore_existing = true;
            c
        };

        // SKIP1: no suffix.
        let lines = run(base_cfg(), 1, files(), dest);
        assert_eq!(lines, vec!["exists.txt exists\n".to_owned()]);

        // SKIP2, no --checksum: size mismatch -> " (file change)".
        let lines = run(base_cfg(), 2, files(), dest);
        assert_eq!(lines, vec!["exists.txt exists (file change)\n".to_owned()]);

        // SKIP2, --checksum: the same size mismatch reports " (sum change)".
        let sum_cfg = || {
            let mut c = base_cfg();
            c.flags.checksum = true;
            c
        };
        let lines = run(sum_cfg(), 2, files(), dest);
        assert_eq!(lines, vec!["exists.txt exists (sum change)\n".to_owned()]);

        // Below the gate: the skip is entirely silent.
        assert!(run(base_cfg(), 0, files(), dest).is_empty());
    }

    /// #45 (type-change branch) - upstream: generator.c:1400-1401. When the
    /// destination is a directory but the source is a regular file, the SKIP2
    /// suffix is " (type change)".
    #[test]
    fn ignore_existing_type_change_suffix() {
        let dir = test_support::create_tempdir();
        let dest = dir.path();
        std::fs::create_dir(dest.join("typed")).expect("seed dest directory");

        let files = || vec![FileEntry::new_file("typed".into(), 10, 0o644)];
        let cfg = || {
            let mut c = server_config();
            c.file_selection.ignore_existing = true;
            c
        };

        let lines = run(cfg(), 2, files(), dest);
        assert_eq!(lines, vec!["typed exists (type change)\n".to_owned()]);
    }
}
