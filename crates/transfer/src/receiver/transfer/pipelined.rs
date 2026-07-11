//! Pipelined receiver transfer driving loop.
//!
//! Drives the two-phase pipelined receive path: phase 1 with the short
//! signature length, phase 2 redo with the full signature length for any file
//! whose phase-1 strong checksum failed. Delegates the per-file pipeline body
//! to `run_pipeline_loop_decoupled` (see `pipeline.rs`).

use std::io::{self, Read, Write};
use std::path::PathBuf;

use logging::{PhaseTimer, debug_log, info_log};
use protocol::flist::FileEntry;
use protocol::stats::DeleteStats;

use crate::pipeline::PipelineConfig;
use crate::receiver::stats::TransferStats;
use crate::receiver::{REDO_CHECKSUM_LENGTH, ReceiverContext};

impl ReceiverContext {
    /// Runs the pipelined receiver transfer loop.
    ///
    /// Creates directories and symlinks, optionally deletes extraneous files,
    /// builds the candidate list, runs the pipeline loop, handles redo pass,
    /// and finalizes the transfer.
    ///
    /// Phase 1 uses `SHORT_SUM_LENGTH` (2 bytes) for reduced signature overhead.
    /// Phase 2 redo uses `SUM_LENGTH` (16 bytes) for full collision resistance.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:720` - `recv_files()` main loop
    /// - `generator.c:2157-2163` - phase 1 vs phase 2 checksum length
    pub fn run_pipelined<R: Read, W: Write + crate::writer::MsgInfoSender + ?Sized>(
        &mut self,
        reader: crate::reader::ServerReader<R>,
        writer: &mut W,
        pipeline_config: PipelineConfig,
    ) -> io::Result<TransferStats> {
        let _t = PhaseTimer::new("receiver-transfer-pipelined");
        let (mut reader, file_count, mut setup) = self.setup_transfer(reader, writer)?;
        let reader = &mut reader;

        // upstream: generator.c:1317-1326 - make_path() for relative_paths
        self.ensure_relative_parents(&setup.dest_dir);
        let mut metadata_errors = self.create_directories(
            &setup.dest_dir,
            &setup.metadata_opts,
            setup.acl_cache.as_deref(),
            setup.acl_id_map.as_deref(),
            writer,
            #[cfg(unix)]
            setup.sandbox.as_deref(),
        )?;
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

        // upstream: receiver.c:653-654 DEBUG_GTE(RECV, 1)
        debug_log!(Recv, 1, "recv_files({}) starting", file_count);

        let mut delete_stats = DeleteStats::new();
        let mut delete_limit_exceeded = false;
        let mut delete_io_error: i32 = 0;
        if self.config.flags.delete {
            let (ds, exceeded, io_bits) = self.delete_extraneous_files(
                &setup.dest_dir,
                #[cfg(unix)]
                setup.sandbox.as_ref(),
                writer,
            )?;
            delete_stats = ds;
            delete_limit_exceeded = exceeded;
            delete_io_error = io_bits;
            // Carry the per-type counters into the receiver context so the
            // goodbye handshake can emit NDX_DEL_STATS to the peer sender.
            // upstream: generator.c:2393-2398 - early write_del_stats() emission.
            self.pending_del_stats = delete_stats;
        }

        let mut stats = TransferStats {
            files_listed: file_count,
            entries_received: file_count as u64,
            io_error: self.flist_reader_cache.as_ref().map_or(0, |r| r.io_error())
                | self.flist_io_error
                | delete_io_error,
            ..Default::default()
        };
        let files_to_transfer = self.build_files_to_transfer(
            writer,
            &setup.dest_dir,
            &setup.metadata_opts,
            None,
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
        // list_file_entry() and sends NO per-file NDX request. Only the existing
        // post-loop phase markers (NDX_DONE) and the goodbye handshake cross the
        // wire. This branch must precede the dry_run check: list-only is not
        // dry-run (dry-run streams an NDX request per file).
        if self.config.flags.list_only {
            stats.list_only_entries = self.collect_list_only_entries();
            writer.flush()?;
        } else if self.config.flags.dry_run {
            self.run_dry_run_loop(reader, writer, &files_to_transfer)?;
        } else {
            let total_files = files_to_transfer.len();
            let redo_config = pipeline_config.clone();
            let mut no_progress: Option<&mut dyn crate::TransferProgressCallback> = None;
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
                &mut no_progress,
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
                        &mut no_progress,
                    )?;

                files_transferred += redo_transferred;
                bytes_received += redo_bytes;
                literal_data += redo_literal;
                matched_data += redo_matched;
                all_delayed_updates.extend(redo_delayed);
            }
        }

        if self.config.flags.verbose && self.config.connection.client_mode {
            for file_entry in &self.file_list {
                if file_entry.is_dir() {
                    let relative_path = file_entry.path();
                    if relative_path.as_os_str() == "." {
                        info_log!(Name, 1, "./");
                    } else {
                        info_log!(Name, 1, "{}/", relative_path.display());
                    }
                }
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

        // upstream: generator.c:2080-2133 - touch_up_dirs() re-applies
        // directory mtimes after file writes clobber them.
        self.touch_up_dirs(&setup.dest_dir);

        self.finalize_transfer(reader, writer)?;

        let total_source_bytes: u64 = self.file_list.iter().map(|e| e.size()).sum();

        stats.files_transferred = files_transferred;
        stats.bytes_received = bytes_received;
        stats.literal_data = literal_data;
        stats.matched_data = matched_data;
        stats.total_source_bytes = total_source_bytes;
        if !metadata_errors.is_empty() {
            stats.io_error |= crate::generator::io_error_flags::IOERR_GENERAL;
        }
        stats.metadata_errors = metadata_errors;
        stats.delete_stats = delete_stats;
        stats.delete_limit_exceeded = delete_limit_exceeded;
        stats.redo_count = redo_count;

        Ok(stats)
    }
}
