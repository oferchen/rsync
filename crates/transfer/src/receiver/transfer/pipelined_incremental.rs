//! Pipelined receiver with incremental directory creation.
//!
//! Like `run_pipelined`, but interleaves directory creation with the file-list
//! walk and tracks per-directory failures so descendants of a failed parent are
//! skipped. Emits itemize lines for both new and pre-existing directories,
//! mirroring upstream `generator.c` semantics.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use logging::{PhaseTimer, debug_log, info_log};
use protocol::CompatibilityFlags;
use protocol::codec::{MonotonicNdxWriter, NdxCodecEnum, create_ndx_codec};

use crate::pipeline::PipelineConfig;
use crate::receiver::PipelineSetup;
use crate::receiver::ndx_stream::{NdxFrame, read_marker_aware_ndx};
use crate::receiver::stats::TransferStats;
use crate::receiver::{REDO_CHECKSUM_LENGTH, ReceiverContext};

use super::mode::ReceiverMode;

impl ReceiverContext {
    /// Runs the receiver with incremental directory creation and failed-dir tracking.
    ///
    /// Unlike [`run_pipelined`](Self::run_pipelined), tracks directory creation
    /// failures and skips files whose parent directories could not be created.
    /// Emits per-directory itemize output for both new and existing directories.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1432` - itemize new directory
    /// - `generator.c:2260` - itemize existing directory (metadata only)
    pub fn run_pipelined_incremental<R: Read, W: Write + crate::writer::MsgInfoSender + ?Sized>(
        &mut self,
        reader: crate::reader::ServerReader<R>,
        writer: &mut W,
        pipeline_config: PipelineConfig,
        mut progress: Option<&mut dyn crate::TransferProgressCallback>,
    ) -> io::Result<TransferStats> {
        let _t = PhaseTimer::new("receiver-transfer-incremental");
        // Buffer itemize rows and flush them once in flist-index order before
        // finalization, so a directory row immediately precedes its children
        // (upstream's single flist-index-order walk, generator.c:2329-2344)
        // rather than oc's two-phase "all dirs, then all files" emission.
        // Mirrors run_pipelined; the async incremental path is out of scope.
        self.defer_itemize = true;
        // Interleave plain `-v` (name-only, no `-i`) client-mode file names with
        // --progress in flist order, matching upstream log_before_transfer
        // (receiver.c:1008-1012). Set identically to run_pipelined: the flags
        // are read by the shared run_pipeline_loop_decoupled, so leaving them
        // unset here left the default feature set (this driver) rendering names
        // as an end-of-run block while the shipped binary interleaved them.
        self.interleave_names = self.config.flags.verbose
            && self.config.connection.client_mode
            && !self.should_emit_itemize()
            && matches!(self.select_mode(), ReceiverMode::Transfer);
        self.names_to_stderr = self.config.flags.msgs_to_stderr;
        self.progress_active = progress.is_some();
        let (mut reader, file_count, mut setup) = self.setup_transfer(reader, writer)?;
        let reader = &mut reader;

        // upstream: main.c:1383-1392 - a client handed an empty list skips
        // do_recv() entirely and reports the io_error the end marker carried.
        // Checked before the sub-list fetch below because upstream's own
        // `if (inc_recurse && file_total == 1) recv_additional_file_list()`
        // (main.c:1380-1381) cannot fire with a file_total of 0 either.
        if self.is_empty_client_flist(file_count) {
            return self.finish_empty_client_flist(reader, writer);
        }

        // RS-3b: when INC_RECURSE is negotiated and the terminating
        // NDX_FLIST_EOF has not yet arrived (a genuine multi-segment sub-list
        // stream), consume it lazily one segment at a time instead of draining
        // it up front. The eager drain below deadlocks against the sender's
        // MAX_FILECNT_LOOKAHEAD window on trees larger than the window because it
        // never emits an NDX_DONE mid-walk to free it. Gated to real transfers
        // with no delete pass (delete stays on the batch path until A5a-4). On
        // the live path INC_RECURSE is not negotiated, so `flist_eof` is already
        // set here, this dispatch never fires, and the batch body below runs
        // unchanged - byte-for-byte.
        if self.should_stream_incremental() {
            return self.run_pipelined_incremental_streaming(
                reader,
                writer,
                pipeline_config,
                progress,
                setup,
                file_count,
            );
        }

        // Materialize the INC_RECURSE sub-list segments the setup no longer
        // drains, so the directory walk, delete sweep, and batched candidate
        // build below see the complete list - each walks the whole `file_list`
        // and cannot classify a still-pending entry (see
        // `delete_pass_flist_complete`). Driven by a flat cursor through
        // `ensure_flat_idx`, the same on-demand primitive the synchronous driver
        // walks (`sync.rs`), so this batch driver now carries an index walk for
        // the segment-boundary reclaim to hang on later; today the walk still
        // runs to completion because those passes need the whole list. When
        // `flist_eof` is already set at entry - every non-INC_RECURSE transfer,
        // and the only live pull path today - `ensure_flat_idx` never touches the
        // reader, so this is a pure in-memory bound scan that leaves `file_list`
        // untouched and reads not one wire byte, byte-identical to the
        // `ensure_all_segments_loaded` drain it replaces.
        // upstream: generator.c:2299-2368 fetches sub-lists on demand.
        let mut flist_ndx_codec = create_ndx_codec(self.protocol.as_u8());
        let mut flat_idx = 0usize;
        while self.ensure_flat_idx(flat_idx, reader, &mut flist_ndx_codec)? {
            flat_idx += 1;
        }

        let mut stats = TransferStats {
            files_listed: file_count,
            entries_received: file_count as u64,
            io_error: self.flist_reader_io_error() | self.flist_io_error,
            ..Default::default()
        };
        // upstream: receiver.c:653-654 DEBUG_GTE(RECV, 1)
        debug_log!(Recv, 1, "recv_files({}) starting", file_count);

        let mut failed_dirs = crate::receiver::directory::FailedDirectories::new();
        let mut metadata_errors: Vec<(PathBuf, String)> = Vec::new();

        // Decide the plain-`-v` directory NAME lines from the PRE-transfer
        // state, before the walk below applies metadata or a child mkdir bumps a
        // parent's mtime. Upstream names a directory only when set_file_attrs()
        // changed it (generator.c:1503-1505); a directory absent pre-transfer is
        // treated as newly created and named. The walk itself no longer names
        // them, so an unchanged re-sync is silent here exactly as in
        // run_pipelined - it previously named every directory unconditionally.
        let verbose_dir_lines = if self.config.flags.verbose
            && self.config.connection.client_mode
            && !self.should_emit_itemize()
        {
            self.verbose_dir_name_lines(&setup.dest_dir)
        } else {
            Vec::new()
        };

        // A dry run reports its directories from the shared `plan_dry_run` pass
        // below, exactly like `run_pipelined`. Running this walk too would
        // record every directory row and created-dir tally twice; it creates
        // nothing under `skip_dest_writes()` anyway (#6947). `--list-only` does
        // not set `dry_run`, so its walk is untouched.
        let walk_dirs = !self.config.flags.dry_run;
        for (flist_idx, file_entry) in self.file_list.iter().enumerate() {
            if walk_dirs && file_entry.is_dir() {
                let result = self.create_directory_incremental(
                    &setup.dest_dir,
                    file_entry,
                    &setup.metadata_opts,
                    &mut failed_dirs,
                    setup.acl_cache.as_deref(),
                    setup.acl_id_map.as_deref(),
                    #[cfg(unix)]
                    setup.sandbox.as_deref(),
                )?;
                match result {
                    Some((is_new, iflags_raw)) => {
                        if is_new {
                            stats.directories_created += 1;
                            // upstream: receiver.c:736-738 - a new directory
                            // (ITEM_IS_NEW) bumps stats.created_dirs. Counts the
                            // pre-flight-mkdir'd transfer root too, so the
                            // "Number of created files" dir sub-count matches.
                            self.record_created(file_entry.mode());
                        }
                        // upstream: generator.c:1480-1483 - itemize each dir with
                        // the flags computed against its pre-apply stat. A new dir
                        // carries ITEM_LOCAL_CHANGE|ITEM_IS_NEW; an existing dir
                        // carries the attribute-diff flags (so a differing root
                        // `.` mtime emits `.d..t......`). emit_itemize's gate
                        // drops the row when nothing is significant.
                        let iflags = crate::generator::ItemFlags::from_raw(iflags_raw);
                        // Deferred (defer_itemize) so the dir row lands in
                        // flist-index order immediately before its children at
                        // flush time, matching run_pipelined and upstream.
                        let _ = self.emit_or_record_itemize(writer, flist_idx, &iflags, file_entry);
                        self.record_server_no_transfer_itemize(flist_idx, iflags.raw());
                    }
                    None => {
                        stats.directories_failed += 1;
                    }
                }
            }
        }

        // upstream: generator.c:1718-1725 - make_path() fills in a parent that
        // is still absent after its own file-list entry would have created it.
        // Must follow the directory walk above: running it first would pre-empt
        // the classified, confined mkdir in `create_directory_incremental` and
        // make a real run report an existing directory where its own --dry-run
        // reports a created one.
        self.ensure_relative_parents(
            &setup.dest_dir,
            #[cfg(unix)]
            setup.sandbox.as_deref(),
        );

        #[cfg(unix)]
        self.create_symlinks(&setup.dest_dir, setup.sandbox.as_deref(), writer)?;
        #[cfg(not(unix))]
        self.create_symlinks(&setup.dest_dir, writer)?;
        #[cfg(unix)]
        self.create_specials(&setup.dest_dir, setup.sandbox.as_deref(), writer)?;
        #[cfg(not(unix))]
        self.create_specials(&setup.dest_dir, writer)?;

        // upstream: generator.c:1360-1366 - missing_args == 2 && file->mode == 0
        // deletes the destination path and skips any creation for the sentinel.
        self.process_missing_args_sentinels(
            &setup.dest_dir,
            #[cfg(unix)]
            setup.sandbox.as_deref(),
        )?;

        // Mirror `run_pipelined`: when `--delete` is in effect, sweep the
        // destination for extraneous entries and capture per-type counters.
        // upstream: generator.c:2280-2281 - --delete-before / --delete-during
        // sweep before the per-file loop. --delete-after / --delete-delay defer
        // the sweep until after the transfer (see the late call below) so the
        // destination `.rsync-filter` merge files transferred by this run are
        // present and consulted at delete time.
        if self.delete_pass_is_early() {
            self.run_receiver_delete_pass(
                super::DeletePassPhase::Early,
                &setup.dest_dir,
                #[cfg(unix)]
                setup.sandbox.as_ref(),
                writer,
                &mut stats,
            )?;
        }

        let files_to_transfer = self.build_files_to_transfer(
            writer,
            &setup.dest_dir,
            #[cfg(unix)]
            setup.sandbox.as_deref(),
            &setup.metadata_opts,
            Some(&failed_dirs),
            &mut metadata_errors,
            &mut stats,
            setup.acl_cache.as_deref(),
            setup.acl_id_map.as_deref(),
        );

        // Both assigned by every arm of the mode match below.
        // upstream: receiver.c:784 total_transferred_size, summed with files_transferred.
        let mut files_transferred: usize;
        let mut transferred_file_size: u64;
        let mut bytes_received: u64 = 0;
        let mut literal_data: u64 = 0;
        let mut matched_data: u64 = 0;
        let mut redo_count: usize = 0;
        let mut all_delayed_updates: Vec<(PathBuf, PathBuf)> = Vec::new();

        // Buffer directory `-v` names under their flist index so each is
        // released immediately before its first child is reached in the transfer
        // loop below (upstream emits the directory row as its flist entry is
        // reached, so a directory precedes its children). Trailing directories
        // with no transferred child flush at end of run.
        if self.interleave_names {
            for (idx, name) in &verbose_dir_lines {
                self.buffer_deferred_name(*idx, format!("{name}\n"));
            }
        }

        // One shared decision (see `mode.rs`) with one shared body per
        // non-transfer mode, so this driver and run_pipelined cannot drift
        // again. The match is exhaustive by design: a new ReceiverMode variant
        // is a compile error in every driver that does not handle it.
        match self.select_mode() {
            ReceiverMode::NonTransfer(non_transfer) => {
                (files_transferred, transferred_file_size) = self.run_non_transfer_mode(
                    non_transfer,
                    reader,
                    writer,
                    &setup,
                    &files_to_transfer,
                    &mut stats,
                )?;
            }
            ReceiverMode::Transfer => {
                let total_files = files_to_transfer.len();
                // Stage 0: hand the pipeline the transfer set by flist index;
                // the in-flight window clones each FileEntry as it is pushed
                // (O(window)), so the loop no longer borrows `self.file_list`.
                let redo_config = pipeline_config.clone();
                let redo_indices;
                let delayed;
                // upstream: io.c::write_ndx / read_ndx keep a single
                // connection-wide prev_positive/prev_negative. The phase-2 redo
                // re-requests files (generator.c:2178-2216) through that SAME
                // state, so one codec pair is threaded through both the phase-1
                // and the redo pass. Fresh per-pass codecs would reset the diff
                // base and desync the daemon-sender's NDX decode on the redo.
                let mut ndx_write_codec = MonotonicNdxWriter::new(self.protocol.as_u8());
                let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());
                (
                    files_transferred,
                    transferred_file_size,
                    bytes_received,
                    literal_data,
                    matched_data,
                    redo_indices,
                    delayed,
                ) = self.run_pipeline_loop_decoupled(
                    reader,
                    writer,
                    pipeline_config,
                    &setup,
                    files_to_transfer,
                    &mut metadata_errors,
                    false,
                    total_files,
                    0,
                    true,
                    &mut progress,
                    &mut ndx_write_codec,
                    &mut ndx_read_codec,
                )?;
                all_delayed_updates.extend(delayed);

                // Phase 2: redo pass for files that failed checksum verification.
                redo_count = redo_indices.len();
                if !redo_indices.is_empty() {
                    setup.checksum_length = REDO_CHECKSUM_LENGTH;

                    // upstream: generator.c:2200 - the phase-2 redo re-enters the
                    // ordinary recv_generator() for the redo index, so the retry
                    // is a full re-request: ITEM_TRANSFER (generator.c:1940), a
                    // re-stat of the destination, and a fresh block signature
                    // built from it (generator.c:1967).
                    let redo_files: Vec<(usize, PathBuf, u32)> = redo_indices
                        .iter()
                        .filter_map(|&idx| {
                            self.file_list.get(idx).map(|entry| {
                                let p = entry.path();
                                let file_path = if p.as_os_str() == "." {
                                    setup.dest_dir.clone()
                                } else {
                                    setup.dest_dir.join(p)
                                };
                                (idx, file_path, crate::generator::ItemFlags::ITEM_TRANSFER)
                            })
                        })
                        .collect();

                    let (
                        redo_transferred,
                        redo_transferred_size,
                        redo_bytes,
                        redo_literal,
                        redo_matched,
                        _,
                        redo_delayed,
                    ) = self.run_pipeline_loop_decoupled(
                        reader,
                        writer,
                        redo_config,
                        &setup,
                        redo_files,
                        &mut metadata_errors,
                        true,
                        total_files,
                        0,
                        true,
                        &mut progress,
                        &mut ndx_write_codec,
                        &mut ndx_read_codec,
                    )?;

                    files_transferred += redo_transferred;
                    transferred_file_size += redo_transferred_size;
                    bytes_received += redo_bytes;
                    literal_data += redo_literal;
                    matched_data += redo_matched;
                    all_delayed_updates.extend(redo_delayed);
                }
            }
        }

        // When interleaving `-v` names (the plain `-v` client pull), directory
        // names were buffered up front and released in flist order alongside
        // their children in the transfer loop above, so skip this end-of-run
        // block; any trailing directories flush just below via flush_names_all.
        if self.config.flags.verbose
            && self.config.connection.client_mode
            && !self.interleave_names
            && !self.should_emit_itemize()
        {
            for (_idx, name) in &verbose_dir_lines {
                info_log!(Name, 1, "{name}");
            }
        }

        // upstream: receiver.c:694-695 then :551-552 - handle_delayed_updates()
        // renames each delay-updates leader to its final path in phase 2, and
        // only then are followers hard-linked to it. See
        // finalize_delayed_updates_and_hardlinks for the ordering rationale.
        #[cfg(unix)]
        self.finalize_delayed_updates_and_hardlinks(
            &setup.dest_dir,
            setup.sandbox.as_deref(),
            &all_delayed_updates,
            writer,
        )?;
        #[cfg(not(unix))]
        self.finalize_delayed_updates_and_hardlinks(&setup.dest_dir, &all_delayed_updates, writer)?;

        // upstream: io.c:1702-1712 - see the matching drain in `pipelined.rs`.
        // The sender's MSG_IO_ERROR arrives before the phase-1 NDX_DONE
        // (sender.c:809-817), so folding it in here is what lets the late sweep
        // honour `delete_in_dir`'s IOERR_GENERAL guard (generator.c:304-311)
        // instead of deleting entries upstream preserves.
        stats.io_error |= reader.take_io_error();

        // upstream: generator.c:2425-2428 - --delete-after / --delete-delay run
        // the sweep only after every file (including each destination
        // `.rsync-filter` and any --delay-updates staged file committed just
        // above) has landed, so per-directory merge protect rules are honoured
        // at delete time. Runs before touch_up_dirs so deletion-induced parent
        // mtime changes are re-tidied (upstream touch_up_dirs at generator.c:2449
        // follows the late delete pass).
        if self.delete_pass_is_late() {
            self.run_receiver_delete_pass(
                super::DeletePassPhase::Late,
                &setup.dest_dir,
                #[cfg(unix)]
                setup.sandbox.as_ref(),
                writer,
                &mut stats,
            )?;
        }

        // upstream: generator.c:2093-2146 - touch_up_dirs() re-applies
        // directory mtimes after file writes clobber them.
        self.touch_up_dirs(&setup.dest_dir, writer);

        stats.files_transferred = files_transferred;
        stats.transferred_file_size = transferred_file_size;
        stats.bytes_received = bytes_received;
        stats.literal_data = literal_data;
        stats.matched_data = matched_data;
        stats.total_source_bytes = self.total_source_size();
        // upstream: flist.c:2993-3006 - the per-type tallies were bumped as
        // each entry (sub-lists included) was read, so they stay exact even
        // after completed segments are reclaimed. Read after the loop because
        // incremental recursion keeps appending sub-list segments until then.
        let (num_dirs, num_symlinks, num_devices, num_specials) = self.file_type_counts();
        stats.num_dirs = num_dirs;
        stats.num_symlinks = num_symlinks;
        stats.num_devices = num_devices;
        stats.num_specials = num_specials;
        if !metadata_errors.is_empty() || stats.directories_failed > 0 || stats.files_skipped > 0 {
            stats.io_error |= crate::generator::io_error_flags::IOERR_GENERAL;
        }
        stats.metadata_errors = metadata_errors;
        stats.redo_count = redo_count;
        // upstream: main.c:803-805 - the pre-flight mkdir of the destination
        // root sets FLAG_DIR_CREATED on flist[0], so the generator itemizes it
        // with ITEM_IS_NEW and receiver.c:736-738 counts created_dirs for it.
        // oc creates the root out-of-band (ensure_dest_root_exists), so the root
        // entry is seen as "existing" in the dir loop above and not counted
        // there; count it here so the created-dir total includes the synthesized
        // root (dir:N+1 for a pull into a fresh directory), matching upstream.
        if self.dest_root_created {
            self.record_created(protocol::flist::FileType::Directory.to_mode_bits());
        }
        // Fold the per-type created tally accumulated across the directory,
        // symlink, special, and file-transfer passes into the returned stats so
        // the client reconstructs the "Number of created files" breakdown.
        // upstream: receiver.c:733-746 - stats.created_* accumulated locally.
        stats.created_stats = self.created_stats.get();
        // Rejoin the make-room deletions with the sweep's tally; upstream counts
        // both into the same `stats.deleted_*` globals (delete.c:241-256).
        stats.delete_stats = self.effective_del_stats();

        // Flush any trailing buffered `-v` directory names (those with no
        // transferred child to release them mid-loop), then drain the deferred
        // itemize rows in flist-index order before the goodbye handshake,
        // matching upstream's single-pass emission ordering.
        self.flush_names_all()?;
        self.flush_itemize_rows(writer)?;

        self.finalize_transfer(reader, writer)?;

        // upstream: io.c:1547 - io_error |= val on MSG_IO_ERROR from the sender.
        // The sender emits MSG_IO_ERROR (sender.c:485-486) for source files that
        // vanished or could not be opened during its send loop. Fold those bits
        // into the exit-code io_error so the receiver reports 24/23; MSG_NO_SEND
        // alone only skips the file and carries no exit-code bits.
        stats.io_error |= reader.take_io_error();

        // upstream: log.c:310-311 - every MSG_ERROR_XFER read off the wire sets
        // got_xfer_error, the only report an ENOENT source argument produces
        // (flist.c:2431 withholds IOERR_GENERAL for it).
        stats.got_xfer_error = reader.xfer_error_count() > 0 || self.got_xfer_error.get();

        Ok(stats)
    }

    /// True when the pipelined-incremental driver should consume the INC_RECURSE
    /// sub-list stream LAZILY, one segment at a time (RS-3b), instead of draining
    /// it up front.
    ///
    /// All four must hold:
    /// - INC_RECURSE negotiated (a sub-list stream exists at all);
    /// - `!flist_eof` at entry - the terminator has not arrived, so this is a
    ///   genuine multi-segment stream. On the live path INC_RECURSE is not
    ///   negotiated and `flist_eof` is set once the single list is received, so
    ///   this is always false and the batch body runs unchanged;
    /// - no delete pass - the per-directory delete split is A5a-4; until then
    ///   `--delete*` stays on the batch path, whose whole-list keep-set needs
    ///   `first_segment_idx == 0` (`delete_pass_flist_complete`);
    /// - a real transfer - the non-transfer modes (list-only, dry-run,
    ///   `--only-write-batch`) keep the eager drain (their reply reads are not
    ///   marker-aware yet, task #47).
    fn should_stream_incremental(&self) -> bool {
        self.compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::INC_RECURSE))
            && !self.flist_eof
            && !self.config.flags.delete
            && matches!(self.select_mode(), ReceiverMode::Transfer)
    }

    /// Determines the flat-index range `[start, end)` of the segment beginning
    /// at flat index `seg_start`, pulling the NEXT sub-list segment (or the
    /// `NDX_FLIST_EOF` terminator) if needed so the end is known.
    ///
    /// A sub-list is read whole to its own end-of-flist terminator by
    /// `receive_one_extra_segment`, so a segment's end is `ndx_segments[k+1].0`
    /// once the next boundary is recorded, or `file_list.len()` once `flist_eof`
    /// is set. Pulling the next segment here is what keeps the sender producing
    /// (it stays roughly one segment ahead); the mid-walk `NDX_DONE` below frees
    /// its window so this never blocks on a parked sender past the lookahead.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:2360-2368` - `wait_for_receiver()` pulls the next list when
    ///   `!cur_flist->next && !flist_eof`.
    fn segment_end_pulling_next<R: Read>(
        &mut self,
        segment_idx: usize,
        reader: &mut crate::reader::ServerReader<R>,
        ndx_read_codec: &mut NdxCodecEnum,
    ) -> io::Result<usize> {
        loop {
            if segment_idx + 1 < self.ndx_segments.len() {
                return Ok(self.ndx_segments[segment_idx + 1].0);
            }
            if self.flist_eof {
                return Ok(self.file_list.len());
            }
            // Learn this segment's end by pulling the next boundary/EOF. The
            // probe index is one past the current end, so `ensure_flat_idx`
            // reads exactly the next frame (a segment or the terminator). The
            // single connection-wide inbound codec is threaded here (not a
            // separate flist codec): a mid-walk sub-list pull and the transfer
            // echoes share one read_ndx diff-state, matching upstream io.c.
            let probe = self.file_list.len();
            self.ensure_flat_idx(probe, reader, ndx_read_codec)?;
        }
    }

    /// Lazy per-segment consumption of an INC_RECURSE sub-list stream (RS-3b).
    ///
    /// Dispatched from [`run_pipelined_incremental`](Self::run_pipelined_incremental)
    /// only when [`should_stream_incremental`](Self::should_stream_incremental)
    /// holds. For each segment in turn it creates that segment's directories,
    /// builds and transfers its candidate files, then - once the segment is
    /// fully drained - emits a per-segment `NDX_DONE` to free the sender's
    /// window (upstream R13/R17: `generator.c:2219-2239`). The RS-3a marker-aware
    /// reply read absorbs any later segment that interleaves with the transfer
    /// replies, so the sender may stay ahead by its lookahead window without
    /// desyncing this driver.
    ///
    /// Whole-list post-passes (relative parents, symlinks, specials,
    /// missing-args, redo, delayed-updates, touch-up) run once at the end over
    /// the fully materialized list, exactly as the batch driver does. Heap
    /// reclaim of retired segments is left to `exchange_phase_done` (as today);
    /// moving it mid-walk - the O(window) RSS win - is RS-3c, because it requires
    /// making those post-passes per-segment so they no longer read the freed
    /// entries.
    #[allow(clippy::too_many_lines)]
    fn run_pipelined_incremental_streaming<
        R: Read,
        W: Write + crate::writer::MsgInfoSender + ?Sized,
    >(
        &mut self,
        reader: &mut crate::reader::ServerReader<R>,
        writer: &mut W,
        pipeline_config: PipelineConfig,
        mut progress: Option<&mut dyn crate::TransferProgressCallback>,
        mut setup: PipelineSetup,
        file_count: usize,
    ) -> io::Result<TransferStats> {
        let _t = PhaseTimer::new("receiver-transfer-incremental-streaming");

        let mut stats = TransferStats {
            files_listed: file_count,
            entries_received: file_count as u64,
            io_error: self.flist_reader_io_error() | self.flist_io_error,
            ..Default::default()
        };
        // upstream: receiver.c:653-654 DEBUG_GTE(RECV, 1)
        debug_log!(Recv, 1, "recv_files({}) starting", file_count);

        let mut failed_dirs = crate::receiver::directory::FailedDirectories::new();
        let mut metadata_errors: Vec<(PathBuf, String)> = Vec::new();

        // upstream: io.c read_ndx keeps ONE connection-wide read state
        // (prev_positive/prev_negative) for f_in - every value it decodes shares
        // it: the sub-list markers (NDX_FLIST_OFFSET/NDX_FLIST_EOF, negative) and
        // the per-file transfer echoes (positive) are diff-encoded against the
        // same running base. The receiver therefore reads BOTH through a single
        // inbound codec: pulling a sub-list segment mid-walk and reading a
        // transfer echo must not diverge, or the second stream to touch a marker
        // decodes its diff against a stale base and misframes the wire (an
        // interleaved NDX_FLIST_OFFSET read as NDX_FLIST_EOF, then the following
        // bytes as a bogus file index). io.c write_ndx keeps its own single
        // WRITE state, mirrored by `ndx_write_codec`; the phase-2 redo re-requests
        // through that same write state, so both codecs are threaded across every
        // segment and the redo pass.
        let mut ndx_write_codec = MonotonicNdxWriter::new(self.protocol.as_u8());
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        let mut files_transferred = 0usize;
        let mut transferred_file_size = 0u64;
        let mut bytes_received = 0u64;
        let mut literal_data = 0u64;
        let mut matched_data = 0u64;
        let mut all_delayed_updates: Vec<(PathBuf, PathBuf)> = Vec::new();
        let mut all_redo_indices: Vec<usize> = Vec::new();
        // Plain-`-v` directory NAME lines, accumulated per segment (each segment's
        // entries are the only ones resident when it is walked). Mirrors the batch
        // driver's up-front `verbose_dir_name_lines`, split across segments.
        let verbose_dir_names = self.config.flags.verbose
            && self.config.connection.client_mode
            && !self.should_emit_itemize();
        let mut all_verbose_dir_lines: Vec<(usize, String)> = Vec::new();

        // Per-segment walk. `segment_idx` addresses `ndx_segments`; `cur_idx`
        // tracks how many segments have been fully processed, which is what
        // gates the mid-walk NDX_DONE (upstream frees `first_flist` only once
        // `cur_flist` has advanced past it).
        let mut segment_idx = 0usize;
        loop {
            // Make sure segment `segment_idx` exists (pull the next frame while
            // the table has not reached it and the stream has not ended).
            while segment_idx >= self.ndx_segments.len() && !self.flist_eof {
                let probe = self.file_list.len();
                if !self.ensure_flat_idx(probe, reader, &mut ndx_read_codec)? {
                    break;
                }
            }
            if segment_idx >= self.ndx_segments.len() {
                break;
            }
            let seg_start = self.ndx_segments[segment_idx].0;
            let seg_end =
                self.segment_end_pulling_next(segment_idx, reader, &mut ndx_read_codec)?;
            if seg_start >= seg_end {
                // An empty segment (e.g. a sub-list of only tombstones): nothing
                // to create or transfer, but it still counts toward the
                // per-segment NDX_DONE. Release it if it is not the last.
                self.release_completed_segment_if_older(
                    segment_idx,
                    &all_redo_indices,
                    reader,
                    &mut ndx_write_codec,
                    &mut ndx_read_codec,
                    writer,
                )?;
                segment_idx += 1;
                continue;
            }
            let range = seg_start..seg_end;

            // Plain-`-v` directory names for this segment, computed BEFORE its
            // directories are created so each stat is pre-transfer (the same gate
            // the batch driver applies to the whole list up front). When
            // interleaving, buffer each under its flist index so it is released
            // just before its first child in the transfer loop; always accumulate
            // for the non-interleave end-of-run block below.
            if verbose_dir_names {
                let seg_lines =
                    self.verbose_dir_name_lines_in_range(range.clone(), &setup.dest_dir);
                if self.interleave_names {
                    for (idx, name) in &seg_lines {
                        self.buffer_deferred_name(*idx, format!("{name}\n"));
                    }
                }
                all_verbose_dir_lines.extend(seg_lines);
            }

            // Directory-creation pass for this segment (a directory must exist
            // before its children are written). Mirrors the batch dir loop,
            // restricted to the segment's range. upstream: generator.c:1432 /
            // :2260 itemize new / existing directory.
            for flist_idx in range.clone() {
                let file_entry = &self.file_list[flist_idx];
                if !file_entry.is_dir() {
                    continue;
                }
                let result = self.create_directory_incremental(
                    &setup.dest_dir,
                    file_entry,
                    &setup.metadata_opts,
                    &mut failed_dirs,
                    setup.acl_cache.as_deref(),
                    setup.acl_id_map.as_deref(),
                    #[cfg(unix)]
                    setup.sandbox.as_deref(),
                )?;
                match result {
                    Some((is_new, iflags_raw)) => {
                        if is_new {
                            stats.directories_created += 1;
                            self.record_created(file_entry.mode());
                        }
                        let iflags = crate::generator::ItemFlags::from_raw(iflags_raw);
                        let _ = self.emit_or_record_itemize(writer, flist_idx, &iflags, file_entry);
                        self.record_server_no_transfer_itemize(flist_idx, iflags.raw());
                    }
                    None => {
                        stats.directories_failed += 1;
                    }
                }
            }

            // Build and transfer this segment's candidate files. The threaded
            // codec pair keeps the request-stream NDX diff-state connection-wide
            // across every segment. A segment that interleaves during these
            // replies is absorbed by the RS-3a marker-aware reply read.
            let files_to_transfer = self.build_files_to_transfer_in_range(
                range.clone(),
                writer,
                &setup.dest_dir,
                #[cfg(unix)]
                setup.sandbox.as_deref(),
                &setup.metadata_opts,
                Some(&failed_dirs),
                &mut metadata_errors,
                &mut stats,
                setup.acl_cache.as_deref(),
                setup.acl_id_map.as_deref(),
            );
            let total_files = files_to_transfer.len();
            // Progress accounting: `files_transferred` is the running total from
            // prior segments (the offset so `files_done` keeps climbing across
            // segments), and `flist_eof` reports whether the sub-list stream has
            // ended (false while more segments may still arrive - upstream's
            // `ir-chk` phase).
            let files_done_offset = files_transferred;
            let flist_complete = self.flist_eof;
            let (
                seg_transferred,
                seg_size,
                seg_bytes,
                seg_literal,
                seg_matched,
                seg_redo,
                seg_delayed,
            ) = self.run_pipeline_loop_decoupled(
                reader,
                writer,
                pipeline_config.clone(),
                &setup,
                files_to_transfer,
                &mut metadata_errors,
                false,
                total_files,
                files_done_offset,
                flist_complete,
                &mut progress,
                &mut ndx_write_codec,
                &mut ndx_read_codec,
            )?;
            files_transferred += seg_transferred;
            transferred_file_size += seg_size;
            bytes_received += seg_bytes;
            literal_data += seg_literal;
            matched_data += seg_matched;
            all_redo_indices.extend(seg_redo);
            all_delayed_updates.extend(seg_delayed);

            // The segment is now fully drained (no in-progress files). Release
            // the OLDEST not-yet-released segment strictly older than the one
            // just finished (upstream frees `first_flist` only while
            // `cur_flist != first_flist`; the current/last segment is freed by
            // the finalize handshake). R17: a segment with files awaiting the
            // phase-2 redo is pinned - not released here - because its flist
            // must stay resident on the sender for the redo re-request.
            self.release_completed_segment_if_older(
                segment_idx,
                &all_redo_indices,
                reader,
                &mut ndx_write_codec,
                &mut ndx_read_codec,
                writer,
            )?;

            segment_idx += 1;
        }

        // Non-interleave `-v` directory names: emitted as an end-of-run block
        // exactly as the batch driver does (upstream generator.c:1503-1505). On
        // the streaming path (Transfer mode, client pull) interleave_names is
        // set, so these were already released alongside their children above and
        // trailing ones flush via flush_names_all; this block covers the
        // non-interleave configuration for parity with the batch driver.
        if verbose_dir_names && !self.interleave_names {
            for (_idx, name) in &all_verbose_dir_lines {
                info_log!(Name, 1, "{name}");
            }
        }

        // Whole-list post-passes over the now fully materialized list, in the
        // same order and form as the batch driver. Safe because RS-3b does not
        // reclaim segment heap mid-walk (see the method doc); every entry's path
        // is still resident here.
        self.ensure_relative_parents(
            &setup.dest_dir,
            #[cfg(unix)]
            setup.sandbox.as_deref(),
        );
        #[cfg(unix)]
        self.create_symlinks(&setup.dest_dir, setup.sandbox.as_deref(), writer)?;
        #[cfg(not(unix))]
        self.create_symlinks(&setup.dest_dir, writer)?;
        #[cfg(unix)]
        self.create_specials(&setup.dest_dir, setup.sandbox.as_deref(), writer)?;
        #[cfg(not(unix))]
        self.create_specials(&setup.dest_dir, writer)?;
        self.process_missing_args_sentinels(
            &setup.dest_dir,
            #[cfg(unix)]
            setup.sandbox.as_deref(),
        )?;

        // Phase 2: redo pass for files that failed checksum verification,
        // deferred until every segment is materialized so a redo index resolves
        // against the complete list. Mirrors the batch driver's redo block,
        // threading the same codec pair. upstream: generator.c:2178-2216.
        let redo_count = all_redo_indices.len();
        if !all_redo_indices.is_empty() {
            setup.checksum_length = REDO_CHECKSUM_LENGTH;
            let redo_files: Vec<(usize, PathBuf, u32)> = all_redo_indices
                .iter()
                .filter_map(|&idx| {
                    self.file_list.get(idx).map(|entry| {
                        let p = entry.path();
                        let file_path = if p.as_os_str() == "." {
                            setup.dest_dir.clone()
                        } else {
                            setup.dest_dir.join(p)
                        };
                        (idx, file_path, crate::generator::ItemFlags::ITEM_TRANSFER)
                    })
                })
                .collect();
            let redo_total = redo_files.len();
            // The redo pass runs after the whole walk, so the sub-list stream has
            // ended (flist complete) and `files_transferred` is the running total
            // across every segment.
            let redo_files_done_offset = files_transferred;
            let (redo_tx, redo_size, redo_bytes, redo_literal, redo_matched, _, redo_delayed) =
                self.run_pipeline_loop_decoupled(
                    reader,
                    writer,
                    pipeline_config,
                    &setup,
                    redo_files,
                    &mut metadata_errors,
                    true,
                    redo_total,
                    redo_files_done_offset,
                    true,
                    &mut progress,
                    &mut ndx_write_codec,
                    &mut ndx_read_codec,
                )?;
            files_transferred += redo_tx;
            transferred_file_size += redo_size;
            bytes_received += redo_bytes;
            literal_data += redo_literal;
            matched_data += redo_matched;
            all_delayed_updates.extend(redo_delayed);
        }

        // upstream: receiver.c:694-695 then :551-552 - delay-updates rename then
        // follower hard-link, after every transfer.
        #[cfg(unix)]
        self.finalize_delayed_updates_and_hardlinks(
            &setup.dest_dir,
            setup.sandbox.as_deref(),
            &all_delayed_updates,
            writer,
        )?;
        #[cfg(not(unix))]
        self.finalize_delayed_updates_and_hardlinks(&setup.dest_dir, &all_delayed_updates, writer)?;

        stats.io_error |= reader.take_io_error();

        // upstream: generator.c:2093-2146 - touch_up_dirs re-applies directory
        // mtimes after file writes clobber them.
        self.touch_up_dirs(&setup.dest_dir, writer);

        stats.files_transferred = files_transferred;
        stats.transferred_file_size = transferred_file_size;
        stats.bytes_received = bytes_received;
        stats.literal_data = literal_data;
        stats.matched_data = matched_data;
        stats.total_source_bytes = self.total_source_size();
        let (num_dirs, num_symlinks, num_devices, num_specials) = self.file_type_counts();
        stats.num_dirs = num_dirs;
        stats.num_symlinks = num_symlinks;
        stats.num_devices = num_devices;
        stats.num_specials = num_specials;
        if !metadata_errors.is_empty() || stats.directories_failed > 0 || stats.files_skipped > 0 {
            stats.io_error |= crate::generator::io_error_flags::IOERR_GENERAL;
        }
        stats.metadata_errors = metadata_errors;
        stats.redo_count = redo_count;
        stats.segments_released_mid_walk = self.segments_released_mid_walk;
        if self.dest_root_created {
            self.record_created(protocol::flist::FileType::Directory.to_mode_bits());
        }
        stats.created_stats = self.created_stats.get();
        stats.delete_stats = self.effective_del_stats();

        self.flush_names_all()?;
        self.flush_itemize_rows(writer)?;

        self.finalize_transfer(reader, writer)?;

        stats.io_error |= reader.take_io_error();
        stats.got_xfer_error = reader.xfer_error_count() > 0 || self.got_xfer_error.get();

        Ok(stats)
    }

    /// Emits one per-segment `NDX_DONE` for the oldest not-yet-released segment
    /// when `cur_segment_idx` has advanced strictly past it (upstream R13:
    /// `cur_flist != first_flist`), the segment has no files awaiting the
    /// phase-2 redo (R17), and drains the sender's echo. Increments
    /// `segments_released_mid_walk` so `exchange_phase_done` emits the remainder
    /// and the total per-segment `NDX_DONE` count on the wire is unchanged.
    ///
    /// The echo is read through the segment-absorbing marker-aware reader (the
    /// receiver context is the sink), because the sender may have queued further
    /// sub-list segments ahead of its `NDX_DONE` echo; a phase-boundary reader
    /// would reject them.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:2219-2239` - `check_for_finished_files` frees `first_flist`
    ///   and writes `NDX_DONE`; `sender.c:246-261` echoes it.
    fn release_completed_segment_if_older<Rd: Read, W: Write + ?Sized>(
        &mut self,
        cur_segment_idx: usize,
        pending_redo: &[usize],
        reader: &mut Rd,
        ndx_write_codec: &mut MonotonicNdxWriter,
        ndx_read_codec: &mut NdxCodecEnum,
        writer: &mut W,
    ) -> io::Result<()> {
        use protocol::codec::NdxCodec;

        // Only release a segment strictly older than the current one; the
        // current/last segment is retired by the finalize handshake.
        if self.segments_released_mid_walk >= cur_segment_idx {
            return Ok(());
        }
        // R17: upstream check_for_finished_files stops freeing at the first
        // flist whose files still await the phase-2 redo (generator.c:2239:
        // `if (first_flist->to_redo) ... break`). RS-3b defers the redo pass
        // until after the whole walk, so the oldest un-released segment's flist
        // must stay resident on the sender until that redo re-requests it by
        // NDX; freeing it here would strand the re-request. Pin it - and, since
        // releases are strictly in order, pinning the oldest also holds every
        // newer segment, exactly upstream's `break`. The pinned NDX_DONEs are
        // emitted by the finalize handshake, which runs after the redo pass, so
        // the total on-wire NDX_DONE count is unchanged.
        let rel = self.segments_released_mid_walk;
        let seg_lo = self.ndx_segments[rel].0;
        let seg_hi = self.ndx_segments[rel + 1].0;
        if pending_redo
            .iter()
            .any(|&ndx| ndx >= seg_lo && ndx < seg_hi)
        {
            return Ok(());
        }

        // Emit one NDX_DONE (frees the sender's oldest flist) and flush so the
        // sender sees it promptly and refills its window (the pump; upstream
        // maybe_flush_socket at generator.c:2231 gates on backlog < MIN/2, and an
        // unconditional flush here is a safe superset).
        ndx_write_codec.write_ndx_done(&mut *writer)?;
        writer.flush()?;

        // Drain the sender's echo, absorbing any sub-list segments it queued
        // ahead of the echo (RS-3a marker-aware read, receiver as the sink).
        match read_marker_aware_ndx(reader, ndx_read_codec, self)? {
            NdxFrame::Done => {}
            NdxFrame::File(ndx) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "expected NDX_DONE echo for a completed sub-list segment, got {ndx} \
                         {}{}",
                        crate::role_trailer::error_location!(),
                        crate::role_trailer::receiver()
                    ),
                ));
            }
        }

        self.segments_released_mid_walk += 1;
        Ok(())
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
    use crate::receiver::directory::FailedDirectories;
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

    /// The incremental driver's deferred flush must interleave directory and
    /// file itemize rows in flist-index order (a dir row immediately precedes
    /// its children), not batch every directory ahead of every file.
    ///
    /// Upstream itemizes in a single flist-index-order walk: `generate_files`
    /// (generator.c:2329-2344) calls `recv_generator` per `cur_flist->sorted[i]`
    /// in index order, and `recv_generator` (generator.c:1480-1483) itemizes
    /// each directory at its own flist position. For the flist `a/ a/f1 b/ b/f2`
    /// upstream prints `.d a/`, `>f a/f1`, `.d b/`, `>f b/f2`.
    ///
    /// `run_pipelined` was wired to defer itemize in #6560; this asserts the sync
    /// incremental driver's own record sites - the per-directory
    /// `create_directory_incremental` loop plus `build_files_to_transfer` - do the
    /// same. It fails if the batch emission returns: reverting the dir loop to an
    /// immediate `emit_itemize` leaves indices 0 and 2 unbuffered, and recording
    /// without the per-index key would order both directory rows ahead of the
    /// files.
    #[test]
    fn incremental_deferred_itemize_rows_interleave_in_flist_index_order() {
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

        // Directory-creation pass: mirror the incremental driver's inline loop,
        // recording the `.d` rows (flist indices 0 and 2) as it creates each dir.
        let mut failed_dirs = FailedDirectories::new();
        for (flist_idx, file_entry) in ctx.file_list.clone().iter().enumerate() {
            if !file_entry.is_dir() {
                continue;
            }
            let result = ctx
                .create_directory_incremental(
                    dest,
                    file_entry,
                    &opts,
                    &mut failed_dirs,
                    None,
                    None,
                    #[cfg(unix)]
                    None,
                )
                .expect("create_directory_incremental succeeds");
            if let Some((_, iflags_raw)) = result {
                let iflags = crate::generator::ItemFlags::from_raw(iflags_raw);
                let _ = ctx.emit_or_record_itemize(&mut writer, flist_idx, &iflags, file_entry);
            }
        }

        // Candidate pass records the new-file transfer rows (indices 1 and 3).
        let mut metadata_errors = Vec::new();
        let mut stats = TransferStats::default();
        let _ = ctx.build_files_to_transfer(
            &mut writer,
            dest,
            #[cfg(unix)]
            None,
            &opts,
            Some(&failed_dirs),
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

    /// A `--dry-run` receive must report the directory it *would* create without
    /// creating it: no `mkdir` on the destination, but the `cd+++++++++` itemize
    /// row and the created-directory tally are still produced.
    ///
    /// WHY all three assertions: upstream suppresses only the syscall, never the
    /// reporting. `syscall.c:1010-1016 do_mkdir()` is `if (dry_run) return 0;`
    /// and `rsync.c:498-499 set_file_attrs()` returns early, while
    /// `generator.c:1480-1483 itemize()` and the receiver's `created_dirs`
    /// counter (`receiver.c:732-737`) run unconditionally. Asserting only the
    /// absent directory would let a future early-return silence output upstream
    /// prints, trading one fidelity bug for another; asserting only the row
    /// would let the mkdir come back.
    ///
    /// `create_directory_incremental` had no `skip_dest_writes()` guard at all,
    /// unlike its non-incremental sibling `create_directories`, so a plain `-n`
    /// on this driver (`incremental-flist`, the default feature set) mutated the
    /// destination.
    #[test]
    fn dry_run_reports_the_directory_without_creating_it() {
        let dir = test_support::create_tempdir();
        let dest = dir.path();

        let hs = handshake();
        let mut config = itemize_client_config();
        config.flags.dry_run = true;
        let mut ctx = ReceiverContext::new_for_test(&hs, config);
        ctx.defer_itemize = true;
        ctx.file_list = vec![
            FileEntry::new_directory("newdir".into(), 0o755),
            FileEntry::new_file("newdir/f1".into(), 5, 0o644),
        ];

        let opts = metadata::MetadataOptions::default();
        let mut writer = crate::writer::ServerWriter::new_plain(Vec::new());
        let mut stats = TransferStats::default();
        let mut failed_dirs = FailedDirectories::new();

        // Mirror the directory pass of `run_pipelined_incremental` exactly: the
        // itemize row and the created tally are the caller's bookkeeping, so a
        // guard placed too early in the callee is only observable from here.
        for (flist_idx, file_entry) in ctx.file_list.clone().iter().enumerate() {
            if !file_entry.is_dir() {
                continue;
            }
            let result = ctx
                .create_directory_incremental(
                    dest,
                    file_entry,
                    &opts,
                    &mut failed_dirs,
                    None,
                    None,
                    #[cfg(unix)]
                    None,
                )
                .expect("create_directory_incremental succeeds");
            let Some((is_new, iflags_raw)) = result else {
                panic!("a dry run must still classify the directory, not skip it");
            };
            if is_new {
                stats.directories_created += 1;
                ctx.record_created(file_entry.mode());
            }
            let iflags = crate::generator::ItemFlags::from_raw(iflags_raw);
            let _ = ctx.emit_or_record_itemize(&mut writer, flist_idx, &iflags, file_entry);
        }

        assert!(
            !dest.join("newdir").exists(),
            "--dry-run must not create {} on the destination",
            dest.join("newdir").display()
        );

        let rows: Vec<String> = ctx
            .itemize_rows
            .borrow()
            .values()
            .map(|lines| lines[0].clone())
            .collect();
        assert_eq!(rows.len(), 1, "exactly one directory row: {rows:?}");
        assert!(
            rows[0].starts_with("cd+++++++++") && rows[0].contains("newdir"),
            "the -n itemize row for the would-be directory must survive: {:?}",
            rows[0]
        );

        assert_eq!(
            stats.directories_created, 1,
            "--dry-run must still count the directory it would create"
        );
        assert_eq!(
            ctx.created_stats.get().dirs,
            1,
            "the created-directories stat feeding \"Number of created files\" must survive"
        );
    }

    /// RS-3b invariant: the TOTAL per-segment `NDX_DONE`s crossing the wire equal
    /// the segment count, whether emitted mid-walk (streaming) or in the finalize
    /// burst (batch) - so moving the emission never double-emits or drops one.
    ///
    /// Drives the REAL `release_completed_segment_if_older` (mid-walk) and the
    /// REAL `exchange_phase_done` (finalize), not reimplementations, over a
    /// windowed segment table. For an INC_RECURSE proto-32 receiver the finalize
    /// handshake also writes exactly 2 non-per-segment markers (the phase
    /// transition and the final goodbye NDX_DONE), and every `NDX_DONE` written
    /// is paired with one echo read. So serving EXACTLY `num_segments + 2`
    /// `NDX_DONE` echoes and asserting the reader is fully consumed with no error
    /// pins the emitted total at `num_segments + 2`: a mid-walk/finalize
    /// double-count reads past the buffer (EOF error) and a drop leaves an echo
    /// unread (leftover bytes) - either reddens. Runs the single-segment path
    /// (`released == 0`, the byte-identical live/batch case) and a 4-segment
    /// multi-segment path (3 released mid-walk, 1 at finalize).
    #[test]
    fn per_segment_ndx_done_total_invariant_single_and_multi_segment() {
        use std::io::Cursor;

        use protocol::CompatibilityFlags;
        use protocol::codec::{MonotonicNdxWriter, NdxCodec, create_ndx_codec};

        const PROTO: u8 = 32;
        // Finalize handshake's non-per-segment NDX_DONE markers for an
        // INC_RECURSE proto-32 receiver: one phase-transition write + the final
        // goodbye write (see exchange_phase_done).
        const FINALIZE_MARKERS: usize = 2;

        // Runs one case and returns (segments_released_mid_walk, echoes_consumed,
        // echoes_served). The caller asserts the invariant on these.
        fn run_case(num_segments: usize) -> (usize, usize, usize) {
            let mut hs = handshake();
            hs.compat_flags = Some(CompatibilityFlags::INC_RECURSE);
            let config = ServerConfig {
                role: ServerRole::Receiver,
                protocol: ProtocolVersion::try_from(PROTO).unwrap(),
                flags: ParsedServerFlags {
                    recursive: true,
                    ..ParsedServerFlags::default()
                },
                args: vec![OsString::from(".")],
                ..Default::default()
            };
            let mut ctx = ReceiverContext::new_for_test(&hs, config);

            // `num_segments` segments of `per` entries each; the +1 NDX gap
            // between segments mirrors upstream flist.c:2966.
            let per = 3usize;
            ctx.file_list = (0..num_segments * per)
                .map(|i| FileEntry::new_file(format!("f{i}").into(), 1, 0o100644))
                .collect();
            ctx.ndx_segments = (0..num_segments)
                .map(|k| (k * per, (k * (per + 1)) as i32 + 1))
                .collect();
            ctx.flist_eof = true;
            ctx.first_segment_idx = 0;
            ctx.segments_released_mid_walk = 0;

            // Serve EXACTLY num_segments + FINALIZE_MARKERS NDX_DONE echoes.
            let echoes_served = num_segments + FINALIZE_MARKERS;
            let mut echo_buf = Vec::new();
            let mut enc = create_ndx_codec(PROTO);
            for _ in 0..echoes_served {
                enc.write_ndx_done(&mut echo_buf).unwrap();
            }
            let mut reader = Cursor::new(echo_buf);
            let mut sink: Vec<u8> = Vec::new();

            // Mid-walk: release every segment strictly older than the current one
            // as the walk advances cur = 1..num_segments (the streaming driver's
            // per-segment-boundary release).
            let mut mid_write = MonotonicNdxWriter::new(PROTO);
            let mut mid_read = create_ndx_codec(PROTO);
            for cur in 1..num_segments {
                ctx.release_completed_segment_if_older(
                    cur,
                    &[],
                    &mut reader,
                    &mut mid_write,
                    &mut mid_read,
                    &mut sink,
                )
                .expect("mid-walk release must not error");
            }
            let released = ctx.segments_released_mid_walk;

            // Finalize handshake, exactly as finalize_transfer drives it (fresh
            // NDX codecs).
            let mut fin_write = create_ndx_codec(PROTO);
            let mut fin_read = create_ndx_codec(PROTO);
            ctx.exchange_phase_done(&mut reader, &mut sink, &mut fin_write, &mut fin_read)
                .expect("finalize handshake must not error");

            let consumed = reader.position() as usize;
            (released, consumed, reader.get_ref().len())
        }

        // Multi-segment: 4 segments -> 3 released mid-walk, 1 at finalize.
        let (released, consumed, served) = run_case(4);
        assert_eq!(released, 3, "cur advancing 1..4 releases segments 0,1,2");
        assert_eq!(
            consumed, served,
            "multi-segment: receiver must consume exactly the served echoes - a \
             double-emit reads past (EOF) and a drop leaves leftovers"
        );
        // released (3) + finalize per-segment (num_segments - released = 1) == 4.
        assert_eq!(released + (4 - released), 4);

        // Single-segment: released==0 (the byte-identical live/batch path); the
        // finalize burst emits all per-segment NDX_DONEs.
        let (released1, consumed1, served1) = run_case(1);
        assert_eq!(released1, 0, "a single segment releases nothing mid-walk");
        assert_eq!(
            consumed1, served1,
            "single-segment: finalize must emit exactly the segment count of \
             per-segment NDX_DONEs"
        );
    }

    /// RS-3b R17 redo-pin: a segment whose flat range holds a file awaiting the
    /// phase-2 redo must NOT emit its per-segment `NDX_DONE` mid-walk. Upstream
    /// `check_for_finished_files` stops freeing at the first flist with a
    /// `to_redo` file (generator.c:2239), keeping it - and, because releases are
    /// strictly in order, every newer flist - resident on the sender for the
    /// deferred redo re-request. The control run (no redo) frees all three older
    /// segments; the redo run frees only the one before the pin. The pin is what
    /// makes the two diverge, so deleting the R17 check reddens this.
    #[test]
    fn r17_redo_bearing_segment_is_pinned_mid_walk() {
        use std::io::Cursor;

        use protocol::CompatibilityFlags;
        use protocol::codec::{MonotonicNdxWriter, NdxCodec, create_ndx_codec};

        const PROTO: u8 = 32;
        let per = 3usize;
        let num_segments = 4usize;

        // Byte length of one NDX_DONE echo on the wire at this protocol.
        let ndx_done_len = {
            let mut b = Vec::new();
            create_ndx_codec(PROTO).write_ndx_done(&mut b).unwrap();
            b.len()
        };

        // Drives the mid-walk release loop over the segment table with the given
        // redo indices; returns (segments_released_mid_walk, mid-walk NDX_DONEs).
        let run = |pending_redo: &[usize]| -> (usize, usize) {
            let mut hs = handshake();
            hs.compat_flags = Some(CompatibilityFlags::INC_RECURSE);
            let config = ServerConfig {
                role: ServerRole::Receiver,
                protocol: ProtocolVersion::try_from(PROTO).unwrap(),
                flags: ParsedServerFlags {
                    recursive: true,
                    ..ParsedServerFlags::default()
                },
                args: vec![OsString::from(".")],
                ..Default::default()
            };
            let mut ctx = ReceiverContext::new_for_test(&hs, config);
            ctx.file_list = (0..num_segments * per)
                .map(|i| FileEntry::new_file(format!("f{i}").into(), 1, 0o100644))
                .collect();
            ctx.ndx_segments = (0..num_segments)
                .map(|k| (k * per, (k * (per + 1)) as i32 + 1))
                .collect();
            ctx.flist_eof = true;
            ctx.first_segment_idx = 0;
            ctx.segments_released_mid_walk = 0;

            // Plenty of echoes so a wrongly-unpinned release still finds one.
            let mut echo_buf = Vec::new();
            let mut enc = create_ndx_codec(PROTO);
            for _ in 0..num_segments {
                enc.write_ndx_done(&mut echo_buf).unwrap();
            }
            let mut reader = Cursor::new(echo_buf);
            let mut sink: Vec<u8> = Vec::new();
            let mut mid_write = MonotonicNdxWriter::new(PROTO);
            let mut mid_read = create_ndx_codec(PROTO);
            for cur in 1..num_segments {
                ctx.release_completed_segment_if_older(
                    cur,
                    pending_redo,
                    &mut reader,
                    &mut mid_write,
                    &mut mid_read,
                    &mut sink,
                )
                .expect("mid-walk release must not error");
            }
            (ctx.segments_released_mid_walk, sink.len() / ndx_done_len)
        };

        // Control: no redo -> segments 0,1,2 all release mid-walk.
        let (released_none, dones_none) = run(&[]);
        assert_eq!(
            released_none, 3,
            "no redo: cur 1..4 releases segments 0,1,2"
        );
        assert_eq!(
            dones_none, 3,
            "no redo: three mid-walk NDX_DONEs on the wire"
        );

        // A redo file in segment 1's flat range [per, 2*per) pins segment 1,
        // which (by in-order release) also holds segments 2 and 3; only the
        // segment before the pin frees mid-walk.
        let (released_redo, dones_redo) = run(&[per]);
        assert_eq!(
            released_redo, 1,
            "a redo in segment 1 pins it and every newer segment; only segment 0 frees"
        );
        assert_eq!(
            dones_redo, 1,
            "the pinned segments emit no mid-walk NDX_DONE (deferred to finalize)"
        );
    }
}
