//! Pipelined receiver with incremental directory creation.
//!
//! Like `run_pipelined`, but interleaves directory creation with the file-list
//! walk and tracks per-directory failures so descendants of a failed parent are
//! skipped. Emits itemize lines for both new and pre-existing directories,
//! mirroring upstream `generator.c` semantics.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use logging::{PhaseTimer, debug_log, info_log};
use protocol::codec::{MonotonicNdxWriter, create_ndx_codec};

use crate::pipeline::PipelineConfig;
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

        // Materialize any INC_RECURSE sub-list segments the setup no longer
        // drains. No-op without INC_RECURSE (`flist_eof` already set).
        // upstream: generator.c:2299-2368 fetches sub-lists on demand.
        let mut flist_ndx_codec = create_ndx_codec(self.protocol.as_u8());
        self.ensure_all_segments_loaded(reader, &mut flist_ndx_codec)?;

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
                let files_to_transfer: Vec<(usize, PathBuf, u32)> = files_to_transfer
                    .into_iter()
                    .map(|(idx, _entry, path, iflags)| (idx, path, iflags))
                    .collect();
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
        // upstream: flist.c:2699-2712 - classify the (now fully materialized,
        // including any INC_RECURSE sub-lists) file list into per-type tallies so
        // the `--stats` "Number of files" breakdown matches upstream. Computed
        // after the loop because incremental recursion appends sub-list segments
        // as the transfer proceeds.
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
}
