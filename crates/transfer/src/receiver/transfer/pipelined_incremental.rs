//! Pipelined receiver with incremental directory creation.
//!
//! Like `run_pipelined`, but interleaves directory creation with the file-list
//! walk and tracks per-directory failures so descendants of a failed parent are
//! skipped. Emits itemize lines for both new and pre-existing directories,
//! mirroring upstream `generator.c` semantics.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use logging::{PhaseTimer, debug_log};
use protocol::codec::create_ndx_codec;
use protocol::flist::FileEntry;

use crate::pipeline::PipelineConfig;
use crate::receiver::stats::TransferStats;
use crate::receiver::{REDO_CHECKSUM_LENGTH, ReceiverContext};

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
        let (mut reader, file_count, mut setup) = self.setup_transfer(reader, writer)?;
        let reader = &mut reader;

        // Materialize any INC_RECURSE sub-list segments the setup no longer
        // drains. No-op without INC_RECURSE (`flist_eof` already set).
        // upstream: generator.c:2299-2368 fetches sub-lists on demand.
        let mut flist_ndx_codec = create_ndx_codec(self.protocol.as_u8());
        self.ensure_all_segments_loaded(reader, &mut flist_ndx_codec)?;

        let mut stats = TransferStats {
            files_listed: file_count,
            entries_received: file_count as u64,
            io_error: self.flist_reader_cache.as_ref().map_or(0, |r| r.io_error())
                | self.flist_io_error,
            ..Default::default()
        };
        // upstream: receiver.c:653-654 DEBUG_GTE(RECV, 1)
        debug_log!(Recv, 1, "recv_files({}) starting", file_count);

        let mut failed_dirs = crate::receiver::directory::FailedDirectories::new();
        let mut metadata_errors: Vec<(PathBuf, String)> = Vec::new();

        // upstream: generator.c:1317-1326 - make_path() for relative_paths
        self.ensure_relative_parents(&setup.dest_dir);

        for (flist_idx, file_entry) in self.file_list.iter().enumerate() {
            if file_entry.is_dir() {
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
                    }
                    None => {
                        stats.directories_failed += 1;
                    }
                }
            }
        }

        #[cfg(unix)]
        self.create_symlinks(&setup.dest_dir, setup.sandbox.as_deref(), writer)?;
        #[cfg(not(unix))]
        self.create_symlinks(&setup.dest_dir, writer)?;
        #[cfg(unix)]
        self.create_specials(&setup.dest_dir, setup.sandbox.as_deref(), writer)?;
        #[cfg(not(unix))]
        self.create_specials(&setup.dest_dir, writer)?;

        // upstream: generator.c:1348-1354 - missing_args == 2 && file->mode == 0
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
            &setup.metadata_opts,
            Some(&failed_dirs),
            &mut metadata_errors,
            &mut stats,
            setup.acl_cache.as_deref(),
            setup.acl_id_map.as_deref(),
        );

        let mut files_transferred: usize = 0;
        let mut bytes_received: u64 = 0;
        let mut literal_data: u64 = 0;
        let mut matched_data: u64 = 0;
        let mut redo_count: usize = 0;
        let mut all_delayed_updates: Vec<(PathBuf, PathBuf)> = Vec::new();

        // upstream: generator.c:1249 - list-only renders every flist entry via
        // list_file_entry() and sends NO per-file NDX request. This branch must
        // precede the dry_run check: list-only is not dry-run.
        if self.config.flags.list_only {
            stats.list_only_entries = self.collect_list_only_entries();
            writer.flush()?;
        } else if self.config.flags.dry_run {
            self.run_dry_run_loop(reader, writer, &files_to_transfer)?;
        } else {
            let total_files = files_to_transfer.len();
            let redo_config = pipeline_config.clone();
            let redo_indices;
            let delayed;
            (
                files_transferred,
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
            )?;
            all_delayed_updates.extend(delayed);

            // Phase 2: redo pass for files that failed checksum verification.
            redo_count = redo_indices.len();
            if !redo_indices.is_empty() {
                setup.checksum_length = REDO_CHECKSUM_LENGTH;

                // upstream: generator.c:1926 - the phase-2 redo re-itemizes with
                // ITEM_TRANSFER; the basis comparison is not re-run for the retry.
                let redo_files: Vec<(usize, &FileEntry, PathBuf, u32)> = redo_indices
                    .iter()
                    .filter_map(|&idx| {
                        self.file_list.get(idx).map(|entry| {
                            let p = entry.path();
                            let file_path = if p.as_os_str() == "." {
                                setup.dest_dir.clone()
                            } else {
                                setup.dest_dir.join(p)
                            };
                            (
                                idx,
                                entry,
                                file_path,
                                crate::generator::ItemFlags::ITEM_TRANSFER,
                            )
                        })
                    })
                    .collect();

                let (redo_transferred, redo_bytes, redo_literal, redo_matched, _, redo_delayed) =
                    self.run_pipeline_loop_decoupled(
                        reader,
                        writer,
                        redo_config,
                        &setup,
                        redo_files,
                        &mut metadata_errors,
                        true,
                        total_files,
                        &mut progress,
                    )?;

                files_transferred += redo_transferred;
                bytes_received += redo_bytes;
                literal_data += redo_literal;
                matched_data += redo_matched;
                all_delayed_updates.extend(redo_delayed);
            }
        }

        #[cfg(unix)]
        self.create_hardlinks(&setup.dest_dir, setup.sandbox.as_deref(), writer)?;
        #[cfg(not(unix))]
        self.create_hardlinks(&setup.dest_dir, writer)?;

        // upstream: receiver.c:584-585 - handle_delayed_updates() at phase 2
        if !all_delayed_updates.is_empty() {
            let backup_cfg = if self.config.flags.backup {
                Some(crate::disk_commit::BackupConfig {
                    dest_dir: setup.dest_dir.clone(),
                    backup_dir: self.config.backup_dir.as_ref().map(PathBuf::from),
                    suffix: self.config.effective_backup_suffix().into(),
                })
            } else {
                None
            };
            super::handle_delayed_updates(&all_delayed_updates, backup_cfg);
        }

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

        // upstream: generator.c:2080-2133 - touch_up_dirs() re-applies
        // directory mtimes after file writes clobber them.
        self.touch_up_dirs(&setup.dest_dir);

        stats.files_transferred = files_transferred;
        stats.bytes_received = bytes_received;
        stats.literal_data = literal_data;
        stats.matched_data = matched_data;
        stats.total_source_bytes = self.file_list.iter().map(|e| e.size()).sum();
        if !metadata_errors.is_empty() || stats.directories_failed > 0 || stats.files_skipped > 0 {
            stats.io_error |= crate::generator::io_error_flags::IOERR_GENERAL;
        }
        stats.metadata_errors = metadata_errors;
        stats.redo_count = redo_count;

        // Drain the deferred itemize rows in flist-index order before the
        // goodbye handshake, matching upstream's single-pass emission ordering.
        self.flush_itemize_rows(writer)?;

        self.finalize_transfer(reader, writer)?;

        // upstream: io.c:1547 - io_error |= val on MSG_IO_ERROR from the sender.
        // The sender emits MSG_IO_ERROR (sender.c:485-486) for source files that
        // vanished or could not be opened during its send loop. Fold those bits
        // into the exit-code io_error so the receiver reports 24/23; MSG_NO_SEND
        // alone only skips the file and carries no exit-code bits.
        stats.io_error |= reader.take_io_error();

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
}
