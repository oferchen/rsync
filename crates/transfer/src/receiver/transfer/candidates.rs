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

use std::collections::HashSet;
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

/// One entry as a `--dry-run` reports it: its flist index, the file-list entry,
/// and the itemize flags a real run would have carried for it.
///
/// upstream: `generator.c:581-600` - `itemize()` writes `write_ndx(ndx)` plus
/// the `iflags` shortint for every entry whose flags are significant, so a
/// dry-run plan is exactly "the entries upstream would put on the wire".
pub(in crate::receiver) type DryRunItem<'a> = (usize, &'a FileEntry, u32);

/// Counts the directories a `--dry-run` plan would create, for the summary's
/// `directories_created` field.
///
/// upstream: `receiver.c:736-738` - `stats.created_dirs++` under
/// `iflags & ITEM_IS_NEW`, which runs whether or not the mkdir happened.
/// The itemize seed for a regular file the generator is about to request.
///
/// upstream: `generator.c:1940-1942`
///
/// ```c
/// int iflags = ITEM_TRANSFER;
/// if (always_checksum > 0)
///     iflags |= ITEM_REPORT_CHANGE;
/// ```
///
/// Shared by the live candidate pass and the `--dry-run` planner so `-i` and
/// `-ni` cannot disagree about the `c` glyph.
const fn transfer_seed(always_checksum: bool) -> u32 {
    use crate::generator::ItemFlags;
    if always_checksum {
        ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_REPORT_CHANGE
    } else {
        ItemFlags::ITEM_TRANSFER
    }
}

/// The destination's mtime as `(seconds, nanoseconds)`, the pair upstream's
/// `mtime_differs()` reads out of `stp->st_mtime` / `stp->ST_MTIME_NSEC`
/// (`generator.c:396-401`).
///
/// The nanosecond component only participates when `--modify-window` is
/// negative (`util1.c:1482`), but it has to be carried so that mode works.
/// A destination whose mtime predates the epoch or cannot be read compares as
/// `(0, 0)`, which differs from any real sender mtime and therefore reports the
/// time as changed - the safe direction.
fn dest_mtime(meta: &fs::Metadata) -> (i64, u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (meta.mtime(), meta.mtime_nsec() as u32)
    }
    #[cfg(not(unix))]
    {
        meta.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or((0, 0), |d| (d.as_secs() as i64, d.subsec_nanos()))
    }
}

pub(in crate::receiver) fn new_dir_count(plan: &[DryRunItem<'_>]) -> u64 {
    plan.iter()
        .filter(|(_, entry, iflags)| {
            entry.is_dir() && iflags & crate::generator::ItemFlags::ITEM_IS_NEW != 0
        })
        .count() as u64
}

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
                // upstream: generator.c:1273-1287 - check_filter(&daemon_filter_list, ...)
                // rejects daemon-excluded files before accepting transfer data.
                // The refusal is never silent: generator.c:1281-1283 reports
                // `ERROR: daemon refused to receive file "%s"` as FERROR_XFER,
                // which a server receiver forwards to the pushing client rather
                // than writing to the daemon's own stderr. Dropping the file
                // without that frame would let a push of module-excluded files
                // exit 0 with no diagnostic at all.
                if has_daemon_filters {
                    let filters =
                        daemon_filters.expect("daemon_filters is Some when has_daemon_filters");
                    let name = e.name();
                    if name != "." {
                        // upstream: generator.c:1258-1266 - the `skip_dir` check
                        // runs before the filter check, so a file below an
                        // already-refused directory is dropped in silence.
                        if crate::receiver::daemon_filter_refuses_ancestor(filters, name) {
                            stats.files_skipped += 1;
                            return false;
                        }
                        if !filters.allows(Path::new(name), false) {
                            let _ = self.emit_error_xfer_line(
                                writer,
                                &format!("ERROR: daemon refused to receive file \"{name}\"\n"),
                            );
                            stats.files_skipped += 1;
                            return false;
                        }
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

        // upstream: generator.c:642 - the quick-check mtime gate keys on
        // ignore_times alone; -t/--times only governs whether mtime is applied.
        let ignore_times = self.config.flags.ignore_times;
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

        // upstream: generator.c:1940-1942 - the seed the generator hands to
        // itemize() at `notify_others` is `ITEM_TRANSFER`, plus
        // `ITEM_REPORT_CHANGE` whenever `always_checksum > 0`. Under
        // `--checksum` every transferred file therefore carries the position-2
        // `c` glyph, because the checksum - not the mtime - is what decided the
        // file changed.
        let transfer_seed = transfer_seed(always_checksum.is_some());

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
                            ignore_times,
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
                    && dest_mtime_newer(meta, entry, modify_window)
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
                    ignore_times,
                    size_only,
                    always_checksum,
                    modify_window,
                ) {
                    // upstream: generator.c:1816 - itemize() with iflags=0 for an
                    // up-to-date file; the attr-comparison may still surface a
                    // metadata-only row (perms/owner/group differing while
                    // size+mtime match).
                    // The `x` column is now part of itemize_existing_flags
                    // itself (upstream generator.c:564-571), and this call
                    // still runs before the apply below overwrites the
                    // destination's xattrs.
                    let unchanged_iflags =
                        self.itemize_existing_flags(entry, &file_path, Some(meta), 0);
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
                        ignore_times,
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
            let base_iflags =
                self.itemize_existing_flags(entry, &file_path, dest_meta.as_ref(), transfer_seed);
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

    /// Reports a `--dry-run` receive and returns the items whose NDX + iflags
    /// still have to cross the wire, in flist-index order.
    ///
    /// This is the single dry-run reporting pass shared by both receiver
    /// drivers (`run_pipelined` and `run_pipelined_incremental`). Upstream runs
    /// the identical `recv_generator()` per-entry loop under `--dry-run`; only
    /// the data transfer and the filesystem mutation are suppressed
    /// (`set_file_attrs()` returns early when `dry_run`, rsync.c:498-499;
    /// `do_mkdir`/`do_unlink` are `if (dry_run) return 0;`, syscall.c:1010-1016).
    /// The `itemize()` call and the receiver's `created_files` tally always run,
    /// so `-ni` prints a row for every changing entry (`>f+++++++++`,
    /// `cd+++++++++`, `cL+++++++++`, ...) and `--stats` reports the counts a real
    /// run would produce. oc's remote receive paths early-return out of the
    /// directory-creation, symlink, and candidate passes when
    /// `skip_dest_writes()` is set, so this read-only pass is the only place the
    /// rows and tallies are produced. Nothing is written to the destination.
    ///
    /// Recording (not immediate emission) keeps the rows interleaved in
    /// flist-index order via the deferred flush (`flush_itemize_rows`), matching
    /// upstream's single flist-index-order walk. `record_itemize` gates on
    /// `should_emit_itemize() && client_mode`, so it is a no-op for a plain `-n`
    /// (no `-i`) and for a server-mode receiver (a push dry run, whose rows
    /// travel as wire iflags and are printed by the client's sender); the
    /// created-file tally is deliberately outside that gate because upstream
    /// counts it in the receiver regardless of `-i` (receiver.c:733-746).
    ///
    /// `candidates` is the regular-file candidate list from
    /// [`Self::build_files_to_transfer`]; a regular file it dropped (daemon
    /// filter, `--max-size`/`--min-size`) is neither reported nor requested,
    /// mirroring upstream's `goto cleanup` before `notify_others`. Hard-link
    /// followers are reported but never requested - the candidate pass excludes
    /// them because upstream services them through `finish_hard_link()`.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1480-1483` - directory `itemize()` (runs under dry-run).
    /// - `generator.c:1935-1947` - regular-file `itemize()` at `notify_others`,
    ///   reached even when `!do_xfers`.
    /// - `generator.c:1594` / `generator.c:1462` - symlink / special `itemize()`.
    /// - `generator.c:581-600` - `itemize()` writes NDX + iflags for every entry
    ///   whose flags are significant, transfer or not.
    /// - `receiver.c:732-746` / `sender.c:293-309` - `ITEM_IS_NEW` bumps
    ///   `stats.created_files` plus the per-type counter.
    pub(in crate::receiver) fn plan_dry_run<'a>(
        &'a self,
        dest_dir: &Path,
        candidates: &[(usize, &'a FileEntry, PathBuf, u32)],
    ) -> Vec<DryRunItem<'a>> {
        // upstream: generator.c:642 - the quick-check mtime gate keys on
        // ignore_times alone; -t/--times only governs whether mtime is applied.
        let ignore_times = self.config.flags.ignore_times;
        let size_only = self.config.file_selection.size_only;
        let modify_window = self.config.file_selection.modify_window;
        let always_checksum = if self.config.flags.checksum {
            Some(self.get_checksum_algorithm())
        } else {
            None
        };
        let requestable: HashSet<usize> = candidates.iter().map(|&(idx, ..)| idx).collect();
        let mut plan = Vec::with_capacity(candidates.len());
        for (idx, entry) in self.file_list.iter().enumerate() {
            let is_candidate = !entry.is_file() || requestable.contains(&idx);
            if !is_candidate && !is_hardlink_follower(entry) {
                continue;
            }
            let rel = entry.path();
            let dest_path = if rel.as_os_str() == "." {
                dest_dir.to_path_buf()
            } else {
                dest_dir.join(rel)
            };
            let raw = self.dry_run_entry_iflags(
                entry,
                &dest_path,
                ignore_times,
                size_only,
                always_checksum,
                modify_window,
            );
            let iflags = crate::generator::ItemFlags::from_raw(raw);
            self.record_itemize(idx, &iflags, entry, None);
            if raw & crate::generator::ItemFlags::ITEM_IS_NEW != 0 {
                self.record_created(entry.mode());
            }
            if is_candidate && iflags.has_significant_flags() {
                plan.push((idx, entry, raw));
            }
        }
        plan
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
        ignore_times: bool,
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
            // upstream: generator.c:1572-1610 - an up-to-date symlink (same
            // target) is metadata-only. A recreated one is itemized with base
            // ITEM_LOCAL_CHANGE|ITEM_REPORT_CHANGE (generator.c:1608-1609):
            // when the existing destination is itself a symlink, `statret`
            // stays 0 and itemize() diffs the attributes against that lstat; a
            // non-symlink obstacle flips `statret` to -1 (generator.c:1606-1607)
            // and an absent destination arrives with it, so both are
            // ITEM_IS_NEW. Mirror the receiver's create_symlinks classification.
            let base = ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_REPORT_CHANGE;
            match fs::symlink_metadata(dest_path) {
                Ok(meta) if meta.file_type().is_symlink() => match fs::read_link(dest_path) {
                    Ok(target) if entry.link_target() == Some(&target) => 0,
                    _ => self.itemize_existing_flags(entry, dest_path, Some(&meta), base),
                },
                Ok(_) | Err(_) => self.itemize_existing_flags(entry, dest_path, None, base),
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
                        ignore_times,
                        size_only,
                        always_checksum,
                        modify_window,
                    ) {
                        0
                    } else {
                        transfer_seed(always_checksum.is_some())
                    };
                    self.itemize_existing_flags(entry, dest_path, Some(&meta), base)
                }
                Err(_) => new_iflags(transfer_seed(always_checksum.is_some())),
            }
        }
    }

    /// Computes the itemize flags for `entry`, mirroring upstream `itemize()`
    /// (`generator.c:511-579`).
    ///
    /// `dest_meta` models upstream's `statret`: `Some(meta)` is the
    /// `statret >= 0` leg (`generator.c:515`) where the pre-transfer
    /// destination stat is compared attribute by attribute, and `None` is the
    /// `statret < 0` leg (`generator.c:573-578`) where the destination is
    /// absent and the row is simply `ITEM_IS_NEW`.
    ///
    /// `base` is `ITEM_TRANSFER` for a file being transferred, or `0` for an
    /// up-to-date file (quick-check match). The returned raw flags OR `base`
    /// with `ITEM_REPORT_{SIZE,TIME,PERMS,OWNER,GROUP}` for every attribute that
    /// differs between `entry` (the sender's view) and `dest_meta` (the
    /// pre-transfer destination stat). Both regular files (quick-check match)
    /// and existing directories reach this path: the `ITEM_REPORT_SIZE` check
    /// is gated on `entry.is_file()` so it never fires for a directory, and
    /// `keep_time` follows upstream's per-type gating on `--omit-dir-times` /
    /// `--omit-link-times` (`generator.c:513-517`).
    pub(in crate::receiver) fn itemize_existing_flags(
        &self,
        entry: &FileEntry,
        path: &Path,
        dest_meta: Option<&fs::Metadata>,
        base: u32,
    ) -> u32 {
        use crate::generator::ItemFlags;
        let mut iflags = base;
        let Some(dest_meta) = dest_meta else {
            // upstream: generator.c:572-576 - the `statret < 0` leg is not
            // ITEM_IS_NEW alone: it first runs `xattr_diff(file, NULL, 1)`,
            // which compares the sender's list against an empty destination
            // (xattrs.c:555-561 sets `rec_cnt = 0` for a NULL `sxp`), so a
            // brand-new file carrying xattrs reports the `x` column too.
            if self.xattr_itemize_active() && self.dest_xattrs_differ(entry, None) {
                iflags |= ItemFlags::ITEM_REPORT_XATTR;
            }
            return iflags | ItemFlags::ITEM_IS_NEW;
        };
        // upstream: generator.c:521 - S_ISREG(file->mode) && F_LENGTH(file) != st_size
        if entry.is_file() && entry.size() != dest_meta.len() {
            iflags |= ItemFlags::ITEM_REPORT_SIZE;
        }
        // upstream: generator.c:526-530 - REPORT_TIME is a TWO-branch ternary,
        // not a single `keep_time &&` test:
        //
        //     } else if (keep_time
        //      ? mtime_differs(&sxp->st, file)
        //      : iflags & (ITEM_TRANSFER|ITEM_LOCAL_CHANGE) && !(iflags & ITEM_MATCHED)
        //       && (!(iflags & ITEM_XNAME_FOLLOWS) || *xname))
        //             iflags |= ITEM_REPORT_TIME;
        //
        // With `keep_time` the row reports a genuine mtime difference. Without
        // it (no `--times`) every transfer/local-change row reports the time as
        // changed, because the receiver is about to leave the destination mtime
        // at "now" instead of the sender's value - which is what makes a plain
        // `rsync -i src/ dst/` print `>f..T......` rather than `>f.........`.
        // upstream: generator.c:513-517 - `keep_time` is not `preserve_mtimes`;
        // it is type-gated:
        //
        //     int keep_time = !preserve_mtimes ? 0
        //         : S_ISDIR(file->mode) ? !omit_dir_times
        //         : S_ISLNK(file->mode) ? !omit_link_times
        //         : 1;
        //
        // Under `-O`/`-J` the receiver deliberately leaves that type's mtime
        // alone, so upstream drops to the `!keep_time` branch for it rather
        // than comparing a timestamp it is not going to set. Both flags do
        // reach the receiver's flag set (core sets them on the embedded-SSH and
        // daemon pull paths); an older comment here claimed otherwise.
        let keep_time = self.config.flags.times
            && !(entry.is_dir() && self.config.effective_omit_dir_times())
            && !(entry.is_symlink() && self.config.flags.omit_link_times);
        let report_time = if keep_time {
            // upstream: generator.c:396-401 - `mtime_differs()` is
            // `!same_time(...)`, which applies the `--modify-window` tolerance
            // and, for a negative window, compares nanoseconds too
            // (util1.c:1478-1489). A raw `!=` on whole seconds ignored both.
            let (dest_secs, dest_nsec) = dest_mtime(dest_meta);
            !self.config.file_selection.modify_window.same_time(
                dest_secs,
                dest_nsec,
                entry.mtime(),
                entry.mtime_nsec(),
            )
        } else {
            // upstream: generator.c:528-530 - all three conjuncts.
            //
            // ITEM_MATCHED (rsync.h:229, log-only bit 18) is set by upstream's
            // `unchanged_attrs()`/hard-link bookkeeping; nothing in oc sets it
            // yet, so this conjunct cannot fire today. It is written out
            // anyway so the predicate stays a faithful transcription and
            // starts working the moment the bit is produced.
            //
            // The third conjunct is `!(iflags & ITEM_XNAME_FOLLOWS) || *xname`.
            // `xname` (the fuzzy-basis / hard-link-leader name) is not part of
            // this function's inputs and no call site seeds
            // ITEM_XNAME_FOLLOWS, so the `|| *xname` alternative is
            // unreachable; the conservative `== 0` half is transcribed here.
            iflags & (ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_LOCAL_CHANGE) != 0
                && iflags & ItemFlags::ITEM_MATCHED == 0
                && iflags & ItemFlags::ITEM_XNAME_FOLLOWS == 0
        };
        if report_time {
            iflags |= ItemFlags::ITEM_REPORT_TIME;
        }
        // upstream: generator.c:535-540 - under `--crtimes` the generator reads
        // the destination's creation time and reports a difference through
        // `same_time()`:
        //
        //     if (crtimes_ndx) {
        //         if (sxp->crtime == 0)
        //             sxp->crtime = get_create_time(fnamecmp, &sxp->st);
        //         if (!same_time(sxp->crtime, 0, F_CRTIME(file), 0))
        //             iflags |= ITEM_REPORT_CRTIME;
        //     }
        //
        // `get_create_time()` yields 0 when the birth time cannot be read
        // (utils.c), which `created()` returning `Err` reproduces: 0 differs
        // from any real sender crtime, so the column reports a change - the
        // same direction upstream takes. Upstream places this test between the
        // atime and perms comparisons; the order of independently OR-ed bits is
        // immaterial, and keeping it out of the `cfg(unix)` block below lets
        // Windows (which does carry a creation time) report the column too.
        if self.config.flags.crtimes {
            let dest_crtime = dest_meta
                .created()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64);
            // upstream: util1.c:1478 - `same_time()` honours `--modify-window`,
            // and both crtime arguments carry a zero nsec component.
            if !self.config.file_selection.modify_window.same_time(
                dest_crtime,
                0,
                entry.crtime(),
                0,
            ) {
                iflags |= ItemFlags::ITEM_REPORT_CRTIME;
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            // upstream: generator.c:531-533 - `atimes_ndx && !S_ISDIR &&
            // !S_ISLNK && !same_time(F_ATIME(file), 0, sxp->st.st_atime, 0)`.
            // Both nsec arguments are literal zeros upstream, but the
            // comparison still goes through `same_time()`, so a positive
            // `--modify-window` tolerates drift here exactly as it does for the
            // mtime. A raw `!=` ignored the window.
            if self.config.flags.atimes
                && !entry.is_dir()
                && !entry.is_symlink()
                && !self.config.file_selection.modify_window.same_time(
                    entry.atime(),
                    0,
                    dest_meta.atime(),
                    0,
                )
            {
                iflags |= ItemFlags::ITEM_REPORT_ATIME;
            }
            // Symlinks join the perm compare only where oc can actually chmod
            // a link (`metadata::CAN_CHMOD_SYMLINK` = macOS/BSD), so a reported
            // `p` is always backed by an applied chmod. Upstream defines
            // `CAN_CHMOD_SYMLINK` whenever HAVE_LCHMOD or HAVE_SETATTRLIST
            // (rsync.h:438-440, probed at configure.ac:911,918); where it is
            // undefined the `#ifndef` at generator.c:542-544 skips the compare.
            // On Linux the const is false and a link's `st_mode` is a fixed
            // 0777, so nothing is reported or applied. The matching apply lives
            // in metadata::apply_symlink_permissions_from_entry, and the same
            // guard gates engine's local-copy change-set detection.
            //
            // The compare itself is an if/else-if (generator.c:546-552), not a
            // lone `preserve_perms` test:
            //
            //     if (preserve_perms) {
            //         if (!BITS_EQUAL(sxp->st.st_mode, file->mode, CHMOD_BITS))
            //             iflags |= ITEM_REPORT_PERMS;
            //     } else if (preserve_executability
            //      && ((sxp->st.st_mode & 0111 ? 1 : 0) ^ (file->mode & 0111 ? 1 : 0)))
            //         iflags |= ITEM_REPORT_PERMS;
            //
            // Under `-E` without `-p` the receiver copies only the
            // executability bit, so only a change in *whether* the file is
            // executable is a permission change - the other mode bits stay as
            // they are and must not raise the `p` column.
            if !entry.is_symlink() || metadata::CAN_CHMOD_SYMLINK {
                if self.config.flags.perms {
                    const CHMOD_BITS: u32 = 0o7777;
                    if (dest_meta.mode() & CHMOD_BITS) != (entry.mode() & CHMOD_BITS) {
                        iflags |= ItemFlags::ITEM_REPORT_PERMS;
                    }
                } else if self.config.flags.preserve_executability
                    && (dest_meta.mode() & 0o111 != 0) != (entry.mode() & 0o111 != 0)
                {
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
            // upstream: generator.c:555-556 - `gid_ndx && !(file->flags &
            // FLAG_SKIP_GROUP) && sxp->st.st_gid != (gid_t)F_GROUP(file)`.
            //
            // FLAG_SKIP_GROUP is stamped on the mapped id at uidlist.c:284
            // (`!am_root && !is_in_group(id2)`), so a non-root receiver that
            // does not belong to the sender's group neither chowns
            // (rsync.c:527) nor reports the group as changed. Without this
            // test an unprivileged pull printed `g` on every file whose group
            // it could never set.
            if self.config.flags.group {
                if let Some(gid) = entry.gid() {
                    if dest_meta.gid() != gid && metadata::group_is_settable(gid) {
                        iflags |= ItemFlags::ITEM_REPORT_GROUP;
                    }
                }
            }
        }
        // upstream: generator.c:557-563 - with `preserve_acls` and a non-symlink
        // the generator reads the destination ACL (`get_acl`) and probes whether
        // applying the sender's cached ACL would change it
        // (`set_acl(NULL, file, sxp, file->mode)`); a non-zero return lights the
        // `a` column. Symlinks are excluded exactly as upstream's
        // `!S_ISLNK(file->mode)` guard, since their own ACL is never applied.
        if self.acl_itemize_active() && !entry.is_symlink() && self.dest_acl_differs(entry, path) {
            iflags |= ItemFlags::ITEM_REPORT_ACL;
        }
        // upstream: generator.c:564-571 - the last thing the `statret >= 0` leg
        // does is a lazy `get_xattr(fnamecmp, sxp)` followed by
        // `xattr_diff(file, sxp, 1)`. It runs for every itemized entry, not
        // just for one that was skipped as up to date.
        if self.xattr_itemize_active() && self.dest_xattrs_differ(entry, Some(path)) {
            iflags |= ItemFlags::ITEM_REPORT_XATTR;
        }
        iflags
    }

    /// Whether the `x` column's destination read should run at all.
    ///
    /// `preserve_xattrs` is the only gate upstream places *inside* `itemize()`
    /// (`generator.c:564`); the `itemizing` test sits at each of its call sites
    /// (`generator.c:1816`, `:1939`). Folding both in here keeps the extra
    /// `listxattr`/`getxattr` pair off the no-itemize scan path, where oc calls
    /// this function for its `ITEM_IS_NEW` answer alone.
    fn xattr_itemize_active(&self) -> bool {
        self.config.flags.xattrs && self.should_emit_itemize()
    }

    /// Whether the destination's extended attributes differ from the sender's,
    /// for the `ITEM_REPORT_XATTR` (`x`) itemize column.
    ///
    /// Resolves the sender's list from the flist and hands both sides to
    /// [`metadata::dest_xattrs_differ`], the shared comparison that the
    /// local-copy executor also uses. Callers gate this on the `-X` itemize path
    /// so the extra read matches upstream's lazy `get_xattr`, and invoke it
    /// before applying the sender's xattrs so the comparison sees the
    /// pre-transfer destination. A sender with no xattrs differs exactly when
    /// the destination still carries some, which the empty-list case expresses
    /// without a second code path.
    ///
    /// `path` is `None` for upstream's `xattr_diff(file, NULL, 1)`
    /// (`generator.c:573`): there is no destination to read, so the sender's
    /// list is compared against an empty one and the answer is simply "does
    /// the sender carry any xattrs".
    pub(in crate::receiver) fn dest_xattrs_differ(
        &self,
        entry: &FileEntry,
        path: Option<&Path>,
    ) -> bool {
        // upstream: xattrs.c:250-252 - `saw_xattr_filter` is a global consulted
        // on every `rsync_xal_get()` call, so the generator's destination read
        // screens names through the same `x`-modifier rules the sender applied
        // to its side. Without it an excluded name survives on the destination
        // list alone and flips the itemize `x` column upstream leaves clear.
        let filter = self
            .xattr_name_filter()
            .map(|set| move |name: &str| set.xattr_name_allowed(name));
        let opts = self.generator_xattr_opts(filter.as_ref().map(|f| f as &dyn Fn(&str) -> bool));
        let sender = self.resolve_xattr_list(entry).unwrap_or_default();
        match path {
            Some(path) => metadata::dest_xattrs_differ(&sender, path, &opts),
            // upstream: xattrs.c:555-561 - a NULL `sxp` yields `rec_cnt = 0`,
            // so the count test at :574 already decides the answer.
            None => protocol::xattr::xattr_diff(
                &sender,
                &protocol::xattr::XattrList::default(),
                self.checksum_seed,
            ),
        }
    }

    /// Builds the generator-side [`metadata::XattrSendOptions`] used to read the
    /// destination/basis attributes for both the itemize diff and the
    /// abbreviated-value request round-trip.
    ///
    /// Shared by [`Self::dest_xattrs_differ`] and [`Self::build_xattr_request`]
    /// so both read the destination through the identical namespace/filter
    /// screen upstream applies in `get_xattr` (`xattrs.c:237-267`).
    fn generator_xattr_opts<'f>(
        &self,
        filter: Option<&'f dyn Fn(&str) -> bool>,
    ) -> metadata::XattrSendOptions<'f> {
        metadata::XattrSendOptions {
            role: metadata::XattrRole::Generator,
            follow_symlinks: false,
            // upstream: xattrs.c:237 - `user_only = am_sender ? 0 : !am_root`,
            // so a non-root generator sees only the `user.*` namespace here. A
            // `security.*` or `trusted.*` difference it could never store must
            // not raise the itemize `x` column. When a filter is present
            // upstream skips this test entirely (xattrs.c:250-257 are the two
            // arms of one `if`/`else if`), which `read_xattrs_for_wire` honours.
            am_root: metadata::am_root(),
            // upstream: xattrs.c:262 - the strip is gated on `am_sender`, so
            // the generator keeps rsync.%FOO at either level.
            preserve_xattrs: self.config.flags.xattrs_level,
            fake_super: self.config.fake_super,
            filter,
            checksum_seed: self.checksum_seed,
        }
    }

    /// Builds the abbreviated-xattr request list for a file about to be
    /// requested from the sender, marking every large value the local basis
    /// cannot satisfy as [`XattrState::Todo`](protocol::xattr::XattrState::Todo).
    ///
    /// Returns `Some(list)` only when at least one entry needs the round-trip;
    /// the caller then sends the request via
    /// [`send_file_request_xattr`](crate::transfer_ops::send_file_request_xattr)
    /// so the sender replies with the full values. Returns `None` when `-X` is
    /// off, the file carries no xattrs, or every abbreviated value already
    /// matches the basis (resolved locally at apply time, exactly as upstream's
    /// `xattr_diff` returning 0 leaves the entry `XSTATE_ABBREV`).
    ///
    /// `basis_path` is the same basis the delta request selected
    /// (`find_basis_file_with_config`), so the diff honours `--fuzzy`,
    /// `--link-dest`, `--compare-dest`, and `--partial-dir` bases in upstream's
    /// priority order. `None` (a brand-new file) compares against an empty list,
    /// marking every abbreviated entry TODO like `xattr_diff(file, NULL, 1)`
    /// (`generator.c:575`).
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:569`,`generator.c:575` - `xattr_diff(file, sxp, 1)` marks
    ///   `XSTATE_TODO`; `generator.c:598` `send_xattr_request()` emits them.
    pub(in crate::receiver) fn build_xattr_request(
        &self,
        entry: &FileEntry,
        basis_path: Option<&Path>,
    ) -> Option<protocol::xattr::XattrList> {
        if !self.config.flags.xattrs {
            return None;
        }
        let mut sender = self.resolve_xattr_list(entry)?;
        // A short-value-only or empty list can never abbreviate, so it never
        // needs a request; skip the basis read entirely.
        if !sender.has_abbreviated() {
            return None;
        }

        let filter = self
            .xattr_name_filter()
            .map(|set| move |name: &str| set.xattr_name_allowed(name));
        let opts = self.generator_xattr_opts(filter.as_ref().map(|f| f as &dyn Fn(&str) -> bool));

        // upstream: xattrs.c:967 get_xattr_data(fnamecmp, ...) - the diff reads
        // the basis (fnamecmp) file. A missing basis or a read error yields an
        // empty list, so every abbreviated entry is not-same and lands in TODO
        // (the new-file path), never a fatal error.
        let basis = basis_path
            .and_then(|p| metadata::read_xattrs_for_wire(p, &opts).ok())
            .unwrap_or_default();

        if protocol::xattr::mark_xattr_requests(&mut sender, &basis, self.checksum_seed) {
            Some(sender)
        } else {
            None
        }
    }

    /// Whether the `a` column's destination ACL read should run at all.
    ///
    /// Mirrors the two gates upstream places on the ACL leg: `preserve_acls`
    /// inside `itemize()` (`generator.c:558`) and the `itemizing` test at each
    /// call site. Folding both in here keeps the extra `getfacl` off the
    /// no-itemize scan path, matching [`Self::xattr_itemize_active`].
    fn acl_itemize_active(&self) -> bool {
        self.config.flags.acls && self.should_emit_itemize()
    }

    /// Whether the destination's ACL differs from the sender's, for the
    /// `ITEM_REPORT_ACL` (`a`) itemize column.
    ///
    /// Resolves the sender's cached (condensed) access and, for directories,
    /// default ACLs from the flist reader's ACL cache - the same cache the apply
    /// path reads - and hands both sides to [`metadata::dest_acl_differs`], which
    /// reads the pre-transfer destination ACL and probes it exactly as upstream's
    /// `set_acl(NULL, file, sxp, mode)` does (`generator.c:561`,
    /// `acls.c:1024-1054`). Named-entry ids are remapped through the same
    /// cross-host id map the apply path uses (`build_acl_id_mapper`), so the
    /// comparison sees the local ids the receiver would write and an identical
    /// ACL never lights the column.
    ///
    /// An entry with no cached ACL index (`acl_ndx() == None`) yields `None`
    /// sender ACLs, matching upstream's `ndx >= 0` guard: no sender ACL, no `a`.
    pub(in crate::receiver) fn dest_acl_differs(&self, entry: &FileEntry, path: &Path) -> bool {
        let Some(reader) = self.flist_reader_cache.as_ref() else {
            return false;
        };
        let cache = reader.acl_cache();
        let sender_access = entry.acl_ndx().and_then(|ndx| cache.get_access(ndx));
        let sender_default = entry.def_acl_ndx().and_then(|ndx| cache.get_default(ndx));
        if sender_access.is_none() && sender_default.is_none() {
            return false;
        }
        // upstream: acls.c:1059-1081 match_acl_ids() - the cached ACL's named
        // entries are converted to local ids before use. Built per call because
        // the ACL itemize path is off unless `-A` and already dominated by the
        // `getfacl` syscall; the snapshot is otherwise identical to the one the
        // apply path builds once at setup.
        let id_map = self.build_acl_id_mapper();
        metadata::dest_acl_differs(
            path,
            sender_access,
            sender_default,
            entry.mode(),
            entry.is_dir(),
            Some(&id_map),
        )
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
        ignore_times: bool,
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
            ignore_times,
            size_only,
            always_checksum,
            modify_window,
        ) {
            if always_checksum.is_some() {
                " (sum change)"
            } else {
                " (file change)"
            }
        } else if !metadata_unchanged(entry, metadata_opts, dest_meta, modify_window) {
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
            self.record_server_no_transfer_itemize(flist_idx, unchanged_iflags);
        }

        // upstream: generator.c:468 unchanged_attrs() - fast-path check avoids
        // the per-function-call overhead of apply_metadata when all attributes
        // already match. Skip entirely when no preservation flags are active.
        // On a no-change scan this eliminates ownership mapping, permission
        // comparison, and timestamp construction for every file.
        if needs_metadata_apply
            && !metadata_unchanged(
                entry,
                metadata_opts,
                stat_meta,
                self.config.file_selection.modify_window,
            )
        {
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
                // upstream: rsync_xal_set resolves an abbreviated value against
                // fnamecmp; the file is its own basis for the in-place case.
                if let Err(e) = metadata::apply_xattrs_from_list(
                    file_path,
                    xattr_list,
                    true,
                    Some(file_path),
                    filter_ref,
                ) {
                    metadata_errors.push((file_path.to_path_buf(), e.to_string()));
                }
            }
        }
    }
}

#[cfg(test)]
mod itemize_order_tests {
    use std::ffi::OsString;
    use std::path::Path;

    use super::transfer_seed;

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

    /// A server-mode push receiver with `-i`: the remote end of a push.
    fn itemize_server_config() -> ServerConfig {
        let mut config = itemize_client_config();
        config.connection.client_mode = false;
        config
    }

    /// upstream: generator.c:582-593 - on a push the server-side generator
    /// writes `NDX + write_shortint(iflags)` for a quick-check-matched file
    /// whose attributes still differ; the pushing client's sender renders the
    /// `.f...p.....` row from those wire iflags (sender.c:292-293
    /// `maybe_log_item`). A server receiver that drops the record makes every
    /// metadata-only change invisible under `-i` on a push - against another
    /// oc peer and against upstream 3.4.4 alike - while a pull stays correct,
    /// so only the push direction ever exposes the loss.
    #[cfg(unix)]
    #[test]
    fn server_push_candidate_scan_records_metadata_only_wire_row() {
        use std::os::unix::fs::PermissionsExt;

        use crate::generator::ItemFlags;

        let dir = test_support::create_tempdir();
        let dest = dir.path();
        let path = dest.join("f.txt");
        std::fs::write(&path, b"x").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let t = filetime::FileTime::from_unix_time(1_600_000_000, 0);
        filetime::set_file_times(&path, t, t).unwrap();

        let mut entry = FileEntry::new_file("f.txt".into(), 1, 0o600);
        entry.set_mtime(1_600_000_000, 0);

        let hs = handshake();
        let run = |client_mode: bool| {
            let mut config = itemize_server_config();
            config.connection.client_mode = client_mode;
            config.flags.times = true;
            config.flags.perms = true;
            let mut ctx = ReceiverContext::new_for_test(&hs, config);
            ctx.file_list = vec![entry.clone()];
            let mut writer = crate::writer::ServerWriter::new_plain(Vec::new());
            let mut metadata_errors = Vec::new();
            let mut stats = TransferStats::default();
            let files = ctx.build_files_to_transfer(
                &mut writer,
                dest,
                &metadata::MetadataOptions::default(),
                None,
                &mut metadata_errors,
                &mut stats,
                None,
                None,
            );
            assert!(
                files.is_empty(),
                "a quick-check match must not request a transfer"
            );
            ctx.server_no_transfer_itemize.borrow().clone()
        };

        assert_eq!(
            run(false),
            vec![(0usize, ItemFlags::ITEM_REPORT_PERMS as u16)],
            "the server-mode candidate scan must record the metadata-only row \
             for the wire; without it the pushing client prints nothing"
        );
        assert!(
            run(true).is_empty(),
            "a client-mode (pull) receiver prints its row locally; recording \
             it for the wire too would double every row"
        );
    }

    /// upstream: generator.c:642 `quick_check_ok()` - the mtime quick-check runs
    /// whenever `ignore_times` is off; `-t`/`--times` only decides whether the
    /// mtime is *applied* to the destination, never whether the comparison runs.
    /// A bare `-r` (recursion, no `-t`, no `-I`) over an unchanged tree must
    /// therefore request zero transfers - the same result upstream produces.
    ///
    /// This encodes WHY the divergence mattered: gating on `!preserve_times`
    /// (i.e. `!times || ignore_times`) re-sent every byte of an already-current
    /// tree under a plain `-r`, an invisible efficiency regression because the
    /// on-disk result stayed byte-identical. The matrix below pins each gate to
    /// its own upstream rule so the fix cannot over-correct: `-I` still forces,
    /// `--size-only` still skips on a size match regardless of mtime.
    #[cfg(unix)]
    #[test]
    fn recursive_without_times_still_quick_checks_mtime() {
        let dir = test_support::create_tempdir();
        let dest = dir.path();
        let path = dest.join("f.txt");
        std::fs::write(&path, b"payload").unwrap();
        let matching = filetime::FileTime::from_unix_time(1_600_000_000, 0);
        filetime::set_file_times(&path, matching, matching).unwrap();

        let mut entry = FileEntry::new_file("f.txt".into(), b"payload".len() as u64, 0o644);
        entry.set_mtime(1_600_000_000, 0);

        let hs = handshake();
        // Returns how many files the generator would request a transfer for,
        // starting from a bare `-r` config the closure may further mutate.
        let requested = |mutate: &dyn Fn(&std::path::Path, &mut ServerConfig)| -> usize {
            let mut config = itemize_client_config();
            config.flags.times = false;
            config.flags.ignore_times = false;
            config.flags.perms = false;
            config.file_selection.size_only = false;
            mutate(&path, &mut config);
            let mut ctx = ReceiverContext::new_for_test(&hs, config);
            ctx.file_list = vec![entry.clone()];
            let mut writer = crate::writer::ServerWriter::new_plain(Vec::new());
            let mut metadata_errors = Vec::new();
            let mut stats = TransferStats::default();
            ctx.build_files_to_transfer(
                &mut writer,
                dest,
                &metadata::MetadataOptions::default(),
                None,
                &mut metadata_errors,
                &mut stats,
                None,
                None,
            )
            .len()
        };

        // Bare `-r`, size+mtime match -> skip (upstream requests 0). Pre-fix oc
        // forced a transfer here because `!preserve_times` was true without -t.
        assert_eq!(
            requested(&|_, _| {}),
            0,
            "-r without -t must skip an unchanged (size+mtime) file"
        );

        // `-I`/--ignore-times is the sole flag that forces a transfer on a match.
        assert_eq!(
            requested(&|_, config| config.flags.ignore_times = true),
            1,
            "-I must force the transfer despite matching size+mtime"
        );

        // --size-only skips on a size match even when the mtime differs, which
        // distinguishes it from the plain mtime gate (which would transfer).
        assert_eq!(
            requested(&|p, config| {
                config.file_selection.size_only = true;
                let stale = filetime::FileTime::from_unix_time(1_500_000_000, 0);
                filetime::set_file_times(p, stale, stale).unwrap();
            }),
            0,
            "--size-only skips on a size match regardless of mtime"
        );

        // Restore the matching mtime the size-only case perturbed, then confirm
        // the plain mtime gate DOES transfer when only the mtime differs - so the
        // -r skip above is genuinely the size+mtime match, not a dead gate.
        filetime::set_file_times(&path, matching, matching).unwrap();
        assert_eq!(
            requested(&|p, _| {
                let stale = filetime::FileTime::from_unix_time(1_500_000_000, 0);
                filetime::set_file_times(p, stale, stale).unwrap();
            }),
            1,
            "-r with a differing mtime (same size) must transfer"
        );
    }

    /// upstream: generator.c:581-583 - `iflags &= 0xffff` puts the full low
    /// word on the wire, but the record is only emitted when a significant
    /// flag survives (or `-ii` / `-vv` force unchanged rows), and this
    /// no-trailing-fields path must never advertise `ITEM_REPORT_XATTR`,
    /// `ITEM_BASIS_TYPE_FOLLOWS`, or `ITEM_XNAME_FOLLOWS` - the peer's sender
    /// would block reading a basis byte, xname vstring, or xattr request that
    /// never arrives.
    #[test]
    fn record_server_no_transfer_itemize_strips_framing_and_gates_significance() {
        use crate::generator::ItemFlags;

        let hs = handshake();
        let ctx = ReceiverContext::new_for_test(&hs, itemize_server_config());
        ctx.record_server_no_transfer_itemize(
            2,
            ItemFlags::ITEM_REPORT_PERMS
                | ItemFlags::ITEM_REPORT_XATTR
                | ItemFlags::ITEM_BASIS_TYPE_FOLLOWS
                | ItemFlags::ITEM_XNAME_FOLLOWS,
        );
        ctx.record_server_no_transfer_itemize(3, 0);
        assert_eq!(
            ctx.server_no_transfer_itemize.borrow().as_slice(),
            &[(2usize, ItemFlags::ITEM_REPORT_PERMS as u16)],
            "framing bits are stripped and an all-clear record is dropped"
        );

        let mut ii = itemize_server_config();
        ii.flags.info_flags.itemize_unchanged = true;
        let ctx = ReceiverContext::new_for_test(&hs, ii);
        ctx.record_server_no_transfer_itemize(3, 0);
        assert_eq!(
            ctx.server_no_transfer_itemize.borrow().as_slice(),
            &[(3usize, 0u16)],
            "-ii (stdout_format_has_i > 1) keeps the unchanged record"
        );
    }

    /// upstream: generator.c:531-533. Under `--atimes` (`-U`), an otherwise
    /// up-to-date regular file whose source atime differs from the destination's
    /// still itemizes an `ITEM_REPORT_ATIME` (`u`) row. Without `-U` the same
    /// atime difference is invisible, and matching mtimes must not add
    /// `ITEM_REPORT_TIME`. Regression guard for a receiver path that omitted the
    /// atime comparison, so remote `-a -U -i` printed no row where upstream emits
    /// `.f......u..`.
    ///
    /// Gated to unix: the atime comparison in `itemize_existing_flags` lives in
    /// a `#[cfg(unix)]` block (it reads `MetadataExt::atime`), matching the
    /// perms/owner/group report bits, so the reported flag only exists on unix.
    #[cfg(unix)]
    #[test]
    fn itemize_existing_flags_reports_atime_only_under_dash_u() {
        use crate::generator::ItemFlags;

        let dir = test_support::create_tempdir();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"x").unwrap();
        // Destination: mtime matches the entry, atime does not.
        filetime::set_file_times(
            &path,
            filetime::FileTime::from_unix_time(1_600_000_000, 0),
            filetime::FileTime::from_unix_time(1_700_000_000, 0),
        )
        .unwrap();
        let meta = std::fs::symlink_metadata(&path).unwrap();

        let mut entry = FileEntry::new_file("f.txt".into(), 1, 0o644);
        entry.set_mtime(1_700_000_000, 0);
        entry.set_atime(1_650_000_000);

        let hs = handshake();

        let mut with_u = itemize_client_config();
        with_u.flags.times = true;
        with_u.flags.atimes = true;
        let ctx = ReceiverContext::new_for_test(&hs, with_u);
        let flags = ctx.itemize_existing_flags(&entry, &path, Some(&meta), 0);
        assert!(
            flags & ItemFlags::ITEM_REPORT_ATIME != 0,
            "atime row missing under -U: {flags:#06x}"
        );
        assert!(
            flags & ItemFlags::ITEM_REPORT_TIME == 0,
            "matching mtime must not set ITEM_REPORT_TIME: {flags:#06x}"
        );

        let mut without_u = itemize_client_config();
        without_u.flags.times = true;
        without_u.flags.atimes = false;
        let ctx = ReceiverContext::new_for_test(&hs, without_u);
        let flags = ctx.itemize_existing_flags(&entry, &path, Some(&meta), 0);
        assert!(
            flags & ItemFlags::ITEM_REPORT_ATIME == 0,
            "atime row must be gated on --atimes: {flags:#06x}"
        );
    }

    /// upstream: generator.c:526-530. REPORT_TIME is a two-branch ternary. The
    /// `!keep_time` branch is what makes a plain `rsync -i` (no `--times`)
    /// print `>f..T......` for every transferred file: the receiver is about to
    /// leave the destination mtime at "now", so the time column reports a
    /// change even though nothing was compared.
    ///
    /// Why it matters: without this branch the `T` glyph is unreachable on the
    /// receiver, so `-i` without `-t` silently under-reports. Verified against
    /// rsync 3.4.4, which prints `>f.sT......` where oc printed `>f.s.......`.
    ///
    /// The branch must stay inert for a metadata-only row (`base == 0`, no
    /// ITEM_TRANSFER / ITEM_LOCAL_CHANGE) - upstream's first conjunct - and must
    /// not fire when `--times` is on and the mtimes agree.
    #[test]
    fn itemize_existing_flags_reports_time_without_times_on_transfer() {
        use crate::generator::ItemFlags;

        let dir = test_support::create_tempdir();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"x").unwrap();
        filetime::set_file_times(
            &path,
            filetime::FileTime::from_unix_time(1_600_000_000, 0),
            filetime::FileTime::from_unix_time(1_600_000_000, 0),
        )
        .unwrap();
        let meta = std::fs::symlink_metadata(&path).unwrap();

        // Source mtime deliberately equals the destination's, so the
        // `keep_time` branch can never be the reason the bit appears.
        let mut entry = FileEntry::new_file("f.txt".into(), 1, 0o644);
        entry.set_mtime(1_600_000_000, 0);

        let hs = handshake();

        let mut no_times = itemize_client_config();
        no_times.flags.times = false;
        let ctx = ReceiverContext::new_for_test(&hs, no_times);

        let transfer =
            ctx.itemize_existing_flags(&entry, &path, Some(&meta), ItemFlags::ITEM_TRANSFER);
        assert!(
            transfer & ItemFlags::ITEM_REPORT_TIME != 0,
            "a transfer without --times must report the time as changed: {transfer:#06x}"
        );

        let local =
            ctx.itemize_existing_flags(&entry, &path, Some(&meta), ItemFlags::ITEM_LOCAL_CHANGE);
        assert!(
            local & ItemFlags::ITEM_REPORT_TIME != 0,
            "ITEM_LOCAL_CHANGE is the second half of upstream's first conjunct: {local:#06x}"
        );

        let metadata_only = ctx.itemize_existing_flags(&entry, &path, Some(&meta), 0);
        assert!(
            metadata_only & ItemFlags::ITEM_REPORT_TIME == 0,
            "an attribute-only row is neither a transfer nor a local change: {metadata_only:#06x}"
        );

        // With --times the `keep_time` branch governs, and the mtimes agree.
        let mut with_times = itemize_client_config();
        with_times.flags.times = true;
        let ctx = ReceiverContext::new_for_test(&hs, with_times);
        let kept = ctx.itemize_existing_flags(&entry, &path, Some(&meta), ItemFlags::ITEM_TRANSFER);
        assert!(
            kept & ItemFlags::ITEM_REPORT_TIME == 0,
            "--times with equal mtimes must leave the time column clear: {kept:#06x}"
        );
    }

    /// upstream: generator.c:564-571 (`statret >= 0`) and :573-575
    /// (`statret < 0`). Both legs of `itemize()` compare xattrs, so the `x`
    /// column belongs on the *transfer* seed as much as on the up-to-date row.
    ///
    /// Why it matters: the comparison used to live at the quick-check skip site
    /// alone, so `x` could only ever appear on a file that was NOT transferred.
    /// A file whose data and xattrs both changed printed `>f.s.......` where
    /// upstream prints `>f.s......x`.
    ///
    /// Linux-only: macOS attaches `com.apple.provenance` to every file, which
    /// the sender and the generator-side read do not currently agree on (a
    /// separate, pre-existing defect in the shared comparison), so the
    /// destination-read half of this assertion is not stable there.
    #[cfg(target_os = "linux")]
    #[test]
    fn itemize_existing_flags_reports_xattr_on_a_transfer() {
        use crate::generator::ItemFlags;
        use protocol::xattr::{XattrEntry, XattrList};

        let dir = test_support::create_tempdir();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"x").unwrap();
        // The destination carries an xattr; the sender entry carries none, so
        // upstream's `xattr_diff` reports a differing count.
        let list = XattrList::with_entries(vec![XattrEntry::new(
            b"user.itemize".to_vec(),
            b"v".to_vec(),
        )]);
        if metadata::apply_xattrs_from_list(&path, &list, true, None, None).is_err() {
            // A filesystem without user-namespace xattr support cannot
            // exercise the comparison; skip rather than assert a false pass.
            return;
        }
        let meta = std::fs::symlink_metadata(&path).unwrap();

        let mut entry = FileEntry::new_file("f.txt".into(), 1, 0o644);
        entry.set_mtime(1_600_000_000, 0);

        let hs = handshake();
        let mut with_x = itemize_client_config();
        with_x.flags.times = true;
        with_x.flags.xattrs = true;
        let ctx = ReceiverContext::new_for_test(&hs, with_x);

        let transfer =
            ctx.itemize_existing_flags(&entry, &path, Some(&meta), ItemFlags::ITEM_TRANSFER);
        assert!(
            transfer & ItemFlags::ITEM_REPORT_XATTR != 0,
            "a transferred file with differing xattrs must report `x`: {transfer:#06x}"
        );

        let skipped = ctx.itemize_existing_flags(&entry, &path, Some(&meta), 0);
        assert!(
            skipped & ItemFlags::ITEM_REPORT_XATTR != 0,
            "the up-to-date row must keep reporting `x`: {skipped:#06x}"
        );

        // Without -X the destination is never read and the column stays clear.
        let mut without_x = itemize_client_config();
        without_x.flags.times = true;
        without_x.flags.xattrs = false;
        let ctx = ReceiverContext::new_for_test(&hs, without_x);
        let off = ctx.itemize_existing_flags(&entry, &path, Some(&meta), ItemFlags::ITEM_TRANSFER);
        assert!(
            off & ItemFlags::ITEM_REPORT_XATTR == 0,
            "the `x` column is gated on --xattrs: {off:#06x}"
        );
    }

    /// upstream: generator.c:542-552. The permission compare is an
    /// `if (preserve_perms) ... else if (preserve_executability ...)`, so `-E`
    /// without `-p` reports `p` only when the *executability* changes - the
    /// only bit the receiver is going to copy.
    ///
    /// Why it matters: oc tested `preserve_perms` alone, so `-tE` never
    /// produced a `p` column at all; rsync 3.4.4 prints `.f...p..... f` when
    /// the exec bit differs and nothing when only the other mode bits do.
    #[cfg(unix)]
    #[test]
    fn perms_column_follows_the_executability_leg_without_dash_p() {
        use crate::generator::ItemFlags;
        use std::os::unix::fs::PermissionsExt;

        let dir = test_support::create_tempdir();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"x").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let meta = std::fs::symlink_metadata(&path).unwrap();

        let hs = handshake();
        let mut dash_e = itemize_client_config();
        dash_e.flags.times = true;
        dash_e.flags.perms = false;
        dash_e.flags.preserve_executability = true;
        let ctx = ReceiverContext::new_for_test(&hs, dash_e);

        let mut executable = FileEntry::new_file("f.txt".into(), 1, 0o755);
        executable.set_mtime(1_600_000_000, 0);
        let flipped = ctx.itemize_existing_flags(&executable, &path, Some(&meta), 0);
        assert!(
            flipped & ItemFlags::ITEM_REPORT_PERMS != 0,
            "-E reports a change in executability: {flipped:#06x}"
        );

        // Same executability, different other bits: nothing -E would copy.
        let mut same_exec = FileEntry::new_file("f.txt".into(), 1, 0o640);
        same_exec.set_mtime(1_600_000_000, 0);
        let unchanged = ctx.itemize_existing_flags(&same_exec, &path, Some(&meta), 0);
        assert!(
            unchanged & ItemFlags::ITEM_REPORT_PERMS == 0,
            "-E must ignore mode bits it does not copy: {unchanged:#06x}"
        );

        // -p takes the other branch and compares all CHMOD_BITS.
        let mut with_p = itemize_client_config();
        with_p.flags.times = true;
        with_p.flags.perms = true;
        let ctx = ReceiverContext::new_for_test(&hs, with_p);
        let full = ctx.itemize_existing_flags(&same_exec, &path, Some(&meta), 0);
        assert!(
            full & ItemFlags::ITEM_REPORT_PERMS != 0,
            "-p compares every CHMOD bit: {full:#06x}"
        );

        // Neither flag: no permission comparison at all.
        let mut neither = itemize_client_config();
        neither.flags.times = true;
        let ctx = ReceiverContext::new_for_test(&hs, neither);
        let off = ctx.itemize_existing_flags(&executable, &path, Some(&meta), 0);
        assert!(
            off & ItemFlags::ITEM_REPORT_PERMS == 0,
            "without -p or -E the `p` column stays clear: {off:#06x}"
        );
    }

    /// upstream: generator.c:555-556 + uidlist.c:284. FLAG_SKIP_GROUP is
    /// stamped on any group a non-root process does not belong to, and the
    /// itemize test is `!(file->flags & FLAG_SKIP_GROUP)`.
    ///
    /// Why it matters: an unprivileged pull of a root:wheel file reported
    /// `.f.....g...` in oc for a group it could never set; rsync 3.4.4 prints
    /// no row. Verified against a real `/usr/share/dict/propernames` (gid 0)
    /// pull with the destination chgrp'd to a group the user does belong to.
    ///
    /// Running as root makes every group settable, so the negative half only
    /// means something unprivileged; it is skipped under root rather than
    /// asserted vacuously.
    #[cfg(unix)]
    #[test]
    fn group_column_skips_a_group_the_process_cannot_set() {
        use crate::generator::ItemFlags;
        use std::os::unix::fs::MetadataExt;

        let dir = test_support::create_tempdir();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"x").unwrap();
        let meta = std::fs::symlink_metadata(&path).unwrap();

        let hs = handshake();
        let mut config = itemize_client_config();
        config.flags.times = true;
        config.flags.group = true;
        let ctx = ReceiverContext::new_for_test(&hs, config);

        // A gid the process can set: its own, made to differ from the
        // destination's by picking a different member group when possible.
        let dest_gid = meta.gid();
        let mut settable = FileEntry::new_file("f.txt".into(), 1, 0o644);
        settable.set_mtime(1_600_000_000, 0);
        settable.set_gid(dest_gid.wrapping_add(1));
        if metadata::group_is_settable(dest_gid.wrapping_add(1)) {
            let flags = ctx.itemize_existing_flags(&settable, &path, Some(&meta), 0);
            assert!(
                flags & ItemFlags::ITEM_REPORT_GROUP != 0,
                "a settable, differing group reports `g`: {flags:#06x}"
            );
        }

        if metadata::am_root() {
            return;
        }
        // gid 0 (wheel/root) is not a supplementary group of an ordinary user.
        if !metadata::group_is_settable(0) && dest_gid != 0 {
            let mut unsettable = FileEntry::new_file("f.txt".into(), 1, 0o644);
            unsettable.set_mtime(1_600_000_000, 0);
            unsettable.set_gid(0);
            let flags = ctx.itemize_existing_flags(&unsettable, &path, Some(&meta), 0);
            assert!(
                flags & ItemFlags::ITEM_REPORT_GROUP == 0,
                "FLAG_SKIP_GROUP suppresses `g` for a group the process cannot \
                 set: {flags:#06x}"
            );
        }
    }

    /// upstream: generator.c:513-517. `keep_time` is type-gated, not simply
    /// `preserve_mtimes`: under `--omit-dir-times` a directory and under
    /// `--omit-link-times` a symlink drop to the `!keep_time` branch, because
    /// the receiver is not going to set that type's mtime at all.
    ///
    /// Why it matters: oc compared the mtime for every type, so `-tO` printed
    /// `.d..t...... sub/` for a directory whose mtime differs while rsync 3.4.4
    /// prints nothing.
    #[test]
    fn keep_time_is_gated_per_type_by_omit_dir_and_link_times() {
        use crate::generator::ItemFlags;

        let dir = test_support::create_tempdir();
        let path = dir.path().join("sub");
        std::fs::create_dir(&path).unwrap();
        filetime::set_file_mtime(&path, filetime::FileTime::from_unix_time(1_600_000_000, 0))
            .unwrap();
        let meta = std::fs::symlink_metadata(&path).unwrap();

        let mut entry = FileEntry::new_directory("sub".into(), 0o755);
        entry.set_mtime(1_700_000_000, 0);

        let hs = handshake();

        let mut plain = itemize_client_config();
        plain.flags.times = true;
        let ctx = ReceiverContext::new_for_test(&hs, plain);
        let reported = ctx.itemize_existing_flags(&entry, &path, Some(&meta), 0);
        assert!(
            reported & ItemFlags::ITEM_REPORT_TIME != 0,
            "a directory mtime difference is reported under plain -t: {reported:#06x}"
        );

        let mut omit_dirs = itemize_client_config();
        omit_dirs.flags.times = true;
        omit_dirs.flags.omit_dir_times = true;
        let ctx = ReceiverContext::new_for_test(&hs, omit_dirs);
        let omitted = ctx.itemize_existing_flags(&entry, &path, Some(&meta), 0);
        assert!(
            omitted & ItemFlags::ITEM_REPORT_TIME == 0,
            "-O drops the directory to the !keep_time branch, and base == 0 \
             there is neither a transfer nor a local change: {omitted:#06x}"
        );

        // --omit-link-times must not affect a directory.
        let mut omit_links = itemize_client_config();
        omit_links.flags.times = true;
        omit_links.flags.omit_link_times = true;
        let ctx = ReceiverContext::new_for_test(&hs, omit_links);
        let unaffected = ctx.itemize_existing_flags(&entry, &path, Some(&meta), 0);
        assert!(
            unaffected & ItemFlags::ITEM_REPORT_TIME != 0,
            "-J is symlink-only and must leave the directory comparison alone: {unaffected:#06x}"
        );
    }

    /// upstream: generator.c:396-401 + util1.c:1478-1489. `mtime_differs()` is
    /// `!same_time(...)`, so the mtime comparison honours `--modify-window`
    /// (and, for a negative window, nanoseconds). oc used a raw `!=` on whole
    /// seconds and so reported `t` for a difference the user asked it to
    /// tolerate: `-t --modify-window=200` on files two minutes apart printed
    /// `.f..t...... f` where rsync 3.4.4 prints nothing.
    #[test]
    fn mtime_comparison_honours_the_modify_window() {
        use crate::generator::ItemFlags;

        let dir = test_support::create_tempdir();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"x").unwrap();
        filetime::set_file_mtime(
            &path,
            filetime::FileTime::from_unix_time(1_600_000_000, 500),
        )
        .unwrap();
        let meta = std::fs::symlink_metadata(&path).unwrap();

        let mut entry = FileEntry::new_file("f.txt".into(), 1, 0o644);
        entry.set_mtime(1_600_000_120, 900);

        let hs = handshake();
        let mut config = itemize_client_config();
        config.flags.times = true;

        let ctx = ReceiverContext::new_for_test(&hs, config.clone());
        let outside = ctx.itemize_existing_flags(&entry, &path, Some(&meta), 0);
        assert!(
            outside & ItemFlags::ITEM_REPORT_TIME != 0,
            "120s apart with a zero window is a difference: {outside:#06x}"
        );

        config.file_selection.modify_window = metadata::ModifyWindow::from_secs(200);
        let ctx = ReceiverContext::new_for_test(&hs, config.clone());
        let inside = ctx.itemize_existing_flags(&entry, &path, Some(&meta), 0);
        assert!(
            inside & ItemFlags::ITEM_REPORT_TIME == 0,
            "a 200s window tolerates a 120s difference: {inside:#06x}"
        );

        // upstream: util1.c:1482 - a negative window compares nanoseconds too,
        // which a whole-second `!=` could never express.
        entry.set_mtime(1_600_000_000, 900);
        config.file_selection.modify_window = metadata::ModifyWindow::from_secs(-1);
        let ctx = ReceiverContext::new_for_test(&hs, config);
        let nsec = ctx.itemize_existing_flags(&entry, &path, Some(&meta), 0);
        assert!(
            nsec & ItemFlags::ITEM_REPORT_TIME != 0,
            "a negative window makes the nsec difference significant: {nsec:#06x}"
        );
    }

    /// upstream: generator.c:535-540. Under `--crtimes` the row reports the
    /// destination's creation time against the sender's through `same_time()`.
    ///
    /// Why it matters: the `n` glyph (log.c:725) was rendered but never
    /// produced on a network receive, so `-N -i` printed `.f.........` where
    /// upstream prints `.f......n..`. The column is gated on `--crtimes`: a run
    /// without it must never surface a birth-time difference.
    #[test]
    fn itemize_existing_flags_reports_crtime_only_under_dash_n() {
        use crate::generator::ItemFlags;

        let dir = test_support::create_tempdir();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"x").unwrap();
        let meta = std::fs::symlink_metadata(&path).unwrap();
        let Ok(created) = meta.created() else {
            // No birth time on this filesystem; upstream's get_create_time
            // would return 0 here too, so there is nothing to compare.
            return;
        };
        let created_secs = created
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);

        let mut entry = FileEntry::new_file("f.txt".into(), 1, 0o644);
        entry.set_mtime(1_600_000_000, 0);
        // Mirror the destination's mtime so only the crtime can differ.
        let hs = handshake();
        let mut with_n = itemize_client_config();
        with_n.flags.times = false;
        with_n.flags.crtimes = true;

        entry.set_crtime(created_secs + 86_400);
        let ctx = ReceiverContext::new_for_test(&hs, with_n.clone());
        let differs = ctx.itemize_existing_flags(&entry, &path, Some(&meta), 0);
        assert!(
            differs & ItemFlags::ITEM_REPORT_CRTIME != 0,
            "a differing crtime must report `n` under --crtimes: {differs:#06x}"
        );

        entry.set_crtime(created_secs);
        let same = ctx.itemize_existing_flags(&entry, &path, Some(&meta), 0);
        assert!(
            same & ItemFlags::ITEM_REPORT_CRTIME == 0,
            "an equal crtime must leave `n` clear: {same:#06x}"
        );

        entry.set_crtime(created_secs + 86_400);
        let mut without_n = itemize_client_config();
        without_n.flags.crtimes = false;
        let ctx = ReceiverContext::new_for_test(&hs, without_n);
        let off = ctx.itemize_existing_flags(&entry, &path, Some(&meta), 0);
        assert!(
            off & ItemFlags::ITEM_REPORT_CRTIME == 0,
            "the `n` column is gated on --crtimes: {off:#06x}"
        );
    }

    /// upstream: generator.c:1940-1942. `--checksum` seeds every transfer with
    /// `ITEM_REPORT_CHANGE`, because the checksum - not the mtime - is what
    /// decided the file changed, so `rsync -rtci` prints `>fc........`.
    ///
    /// The live pass and the `--dry-run` planner share this seed, so `-i` and
    /// `-ni` cannot disagree about the `c` glyph.
    #[test]
    fn transfer_seed_carries_change_only_under_checksum() {
        use crate::generator::ItemFlags;

        assert_eq!(
            transfer_seed(true),
            ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_REPORT_CHANGE
        );
        assert_eq!(transfer_seed(false), ItemFlags::ITEM_TRANSFER);
    }

    /// upstream: generator.c:572-576. The `statret < 0` leg runs
    /// `xattr_diff(file, NULL, 1)` before adding ITEM_IS_NEW, and a NULL `sxp`
    /// means an empty receiver list (xattrs.c:555-561). A sender entry with no
    /// xattrs must therefore leave the `x` column clear - otherwise every
    /// brand-new file under `-X` would print `x` instead of `+`.
    #[test]
    fn itemize_existing_flags_absent_dest_reports_xattr_only_when_the_sender_has_some() {
        use crate::generator::ItemFlags;

        let dir = test_support::create_tempdir();
        let path = dir.path().join("absent.txt");
        let entry = FileEntry::new_file("absent.txt".into(), 1, 0o644);

        let hs = handshake();
        let mut with_x = itemize_client_config();
        with_x.flags.xattrs = true;
        let ctx = ReceiverContext::new_for_test(&hs, with_x);

        let flags = ctx.itemize_existing_flags(&entry, &path, None, ItemFlags::ITEM_TRANSFER);
        assert_eq!(
            flags,
            ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW,
            "an absent destination with no sender xattrs is ITEM_IS_NEW alone: {flags:#06x}"
        );
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

    /// The wire plan as `(flist index, iflags)` pairs.
    type PlanFlags = Vec<(usize, u32)>;
    /// The recorded itemize rows as `(flist index, rendered line)` pairs.
    type Rows = Vec<(usize, String)>;

    /// Runs the dry-run candidate + reporting passes exactly as both receiver
    /// drivers do, and returns the wire plan alongside the recorded rows.
    fn dry_run_pass(ctx: &ReceiverContext, dest: &Path) -> (PlanFlags, Rows) {
        let mut writer = crate::writer::ServerWriter::new_plain(Vec::new());
        let mut stats = TransferStats::default();
        let mut metadata_errors = Vec::new();
        let candidates = ctx.build_files_to_transfer(
            &mut writer,
            dest,
            &metadata::MetadataOptions::default(),
            None,
            &mut metadata_errors,
            &mut stats,
            None,
            None,
        );
        let plan = ctx.plan_dry_run(dest, &candidates);
        let rows = ctx
            .itemize_rows
            .borrow()
            .iter()
            .map(|(idx, lines)| (*idx, lines[0].clone()))
            .collect();
        (
            plan.into_iter()
                .map(|(idx, _, iflags)| (idx, iflags))
                .collect(),
            rows,
        )
    }

    /// A `--dry-run` receive must itemize every changing file-list entry, count
    /// every entry it would create, and put the same NDX + iflags on the wire a
    /// real run would - all without touching the destination.
    ///
    /// Upstream gates only the mutations: `do_mkdir` is `if (dry_run) return 0;`
    /// (syscall.c:1010-1016) and `set_file_attrs()` returns early (rsync.c:498),
    /// while `itemize()` (generator.c:1480-1483) and `stats.created_files++`
    /// (receiver.c:732-746) run unconditionally. oc's receive paths early-return
    /// out of the directory-creation, symlink, and candidate passes under
    /// `skip_dest_writes()`, so this shared pass is the only producer of all
    /// three - and it is shared precisely so the two drivers cannot answer
    /// differently.
    ///
    /// The `iflags` assertion is the load-bearing one for a PUSH: those exact
    /// bits are what the peer sender renders. Sending a bare `ITEM_TRANSFER`
    /// (what shipped) turned `>f+++++++++` into `<f.........` and left the
    /// peer's created-file tally at zero.
    #[test]
    fn dry_run_plan_reports_rows_counts_and_wire_flags_without_writing() {
        use crate::generator::ItemFlags;

        let dir = test_support::create_tempdir();
        let dest = dir.path();

        let hs = handshake();
        let mut config = itemize_client_config();
        config.flags.dry_run = true;
        let mut ctx = ReceiverContext::new_for_test(&hs, config);
        ctx.defer_itemize = true;
        ctx.file_list = vec![
            FileEntry::new_directory("d".into(), 0o755),  // idx 0
            FileEntry::new_file("d/f1".into(), 5, 0o644), // idx 1
            FileEntry::new_symlink("d/lnk".into(), "target".into()), // idx 2
        ];

        // Read-only pass against an empty destination: every entry is new.
        let (plan, rows) = dry_run_pass(&ctx, dest);

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

        assert_eq!(
            plan,
            vec![
                (0, ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW),
                (1, ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW),
                // upstream: generator.c:1608-1609 - the symlink create path
                // passes ITEM_LOCAL_CHANGE|ITEM_REPORT_CHANGE to itemize(), so
                // the wire bits carry REPORT_CHANGE even when ITEM_IS_NEW makes
                // the rendered row all `+`.
                (
                    2,
                    ItemFlags::ITEM_LOCAL_CHANGE
                        | ItemFlags::ITEM_REPORT_CHANGE
                        | ItemFlags::ITEM_IS_NEW,
                ),
            ],
            "the wire plan must carry the directory and symlink too, each with \
             the itemize flags a real run would have sent"
        );

        // upstream: receiver.c:732-746 - created_files counts every ITEM_IS_NEW
        // entry; reg is the derived remainder (3 - 1 dir - 1 link = 1).
        let created = ctx.created_stats.get();
        assert_eq!(created.files, 3, "created_files");
        assert_eq!(created.dirs, 1, "created_dirs");
        assert_eq!(created.symlinks, 1, "created_symlinks");

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

    /// Without `-i` the pass records no row - the bare `-v` name path handles
    /// verbose output instead - but it must still count what it would create
    /// and still plan the wire request.
    ///
    /// Upstream's `stats.created_files++` sits outside the itemize gate
    /// (receiver.c:733 runs for every `ITEM_IS_NEW`, whether or not
    /// `stdout_format_has_i`), so `-n --stats` without `-i` reports the same
    /// "Number of created files" as `-ni --stats`. Folding the tally into the
    /// `-i` gate is the easy mistake this pins.
    #[test]
    fn dry_run_plan_counts_creations_without_the_itemize_flag() {
        let dir = test_support::create_tempdir();
        let dest = dir.path();

        let hs = handshake();
        let mut config = itemize_client_config();
        config.flags.info_flags.itemize = false;
        config.flags.dry_run = true;
        let mut ctx = ReceiverContext::new_for_test(&hs, config);
        ctx.defer_itemize = true;
        ctx.file_list = vec![FileEntry::new_file("f".into(), 1, 0o644)];

        let (plan, rows) = dry_run_pass(&ctx, dest);

        assert!(rows.is_empty(), "no itemize rows recorded without -i");
        assert_eq!(plan.len(), 1, "the file is still requested over the wire");
        assert_eq!(
            ctx.created_stats.get().files,
            1,
            "the created-file tally is independent of the -i gate"
        );
    }

    /// A server-mode receiver (the remote end of a PUSH) records no local row -
    /// upstream's `log.c:822` gates the `FCLIENT` write on `!am_server` and the
    /// client's sender prints instead - but it must still classify every entry,
    /// because those classifications ARE the push's output once they cross the
    /// wire as iflags.
    #[test]
    fn dry_run_plan_on_a_server_receiver_still_classifies_for_the_wire() {
        use crate::generator::ItemFlags;

        let dir = test_support::create_tempdir();
        let dest = dir.path();

        let hs = handshake();
        let mut config = itemize_client_config();
        config.flags.dry_run = true;
        config.connection.client_mode = false;
        let mut ctx = ReceiverContext::new_for_test(&hs, config);
        ctx.defer_itemize = true;
        ctx.file_list = vec![
            FileEntry::new_directory("d".into(), 0o755),
            FileEntry::new_file("d/f1".into(), 5, 0o644),
        ];

        let (plan, rows) = dry_run_pass(&ctx, dest);

        assert!(rows.is_empty(), "a server receiver prints no row itself");
        assert_eq!(
            plan,
            vec![
                (0, ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW),
                (1, ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW),
            ],
            "the client sender renders `cd+++++++++` / `<f+++++++++` from exactly \
             these bits, so they must survive the server-mode row suppression"
        );
    }

    /// #241/#248 dry-run leg: a destination symlink pointing elsewhere must
    /// classify as a CHANGE (`ITEM_LOCAL_CHANGE|ITEM_REPORT_CHANGE`, statret
    /// kept at 0 - generator.c:1604-1610), never ITEM_IS_NEW, and must not
    /// bump the created tally (receiver.c:733-741 counts only ITEM_IS_NEW).
    #[test]
    #[cfg(unix)]
    fn dry_run_plan_classifies_repointed_symlink_as_change() {
        use crate::generator::ItemFlags;

        let dir = test_support::create_tempdir();
        let dest = dir.path();
        std::os::unix::fs::symlink("oldtgt", dest.join("lnk")).expect("seed dest symlink");

        let hs = handshake();
        let mut config = itemize_client_config();
        config.flags.dry_run = true;
        config.flags.links = true;
        // With `-t` and a same-whole-second mtime the `t` column stays dark,
        // leaving exactly the base change bits on the wire.
        config.flags.times = true;
        let mut ctx = ReceiverContext::new_for_test(&hs, config);
        ctx.defer_itemize = true;
        let mut entry = FileEntry::new_symlink("lnk".into(), "newtgt".into());
        {
            use std::os::unix::fs::MetadataExt;
            let old_meta = std::fs::symlink_metadata(dest.join("lnk")).expect("lstat");
            entry.set_mtime(old_meta.mtime(), 0);
        }
        ctx.file_list = vec![entry];

        let (plan, rows) = dry_run_pass(&ctx, dest);

        assert_eq!(
            plan,
            vec![(
                0,
                ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_REPORT_CHANGE
            )],
            "a re-pointed symlink keeps statret == 0: change bits, no ITEM_IS_NEW"
        );
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].1.starts_with("cLc"),
            "dry-run row is a change row, not `cL+++++++++`: {:?}",
            rows[0].1
        );
        assert_eq!(
            ctx.created_stats.get().symlinks,
            0,
            "a replaced symlink is not a creation"
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
