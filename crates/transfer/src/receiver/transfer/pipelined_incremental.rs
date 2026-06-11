//! Pipelined receiver with incremental directory creation.
//!
//! Like `run_pipelined`, but interleaves directory creation with the file-list
//! walk and tracks per-directory failures so descendants of a failed parent are
//! skipped. Emits itemize lines for both new and pre-existing directories,
//! mirroring upstream `generator.c` semantics.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use logging::PhaseTimer;
use protocol::flist::FileEntry;
use protocol::stats::DeleteStats;

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
        let (mut reader, file_count, mut setup) = self.setup_transfer(reader)?;
        let reader = &mut reader;

        let mut stats = TransferStats {
            files_listed: file_count,
            entries_received: file_count as u64,
            io_error: self.flist_reader_cache.as_ref().map_or(0, |r| r.io_error())
                | self.flist_io_error,
            ..Default::default()
        };
        let mut failed_dirs = crate::receiver::directory::FailedDirectories::new();
        let mut metadata_errors: Vec<(PathBuf, String)> = Vec::new();

        // upstream: generator.c:1317-1326 - make_path() for relative_paths
        self.ensure_relative_parents(&setup.dest_dir);

        for file_entry in &self.file_list {
            if file_entry.is_dir() {
                let result = self.create_directory_incremental(
                    &setup.dest_dir,
                    file_entry,
                    &setup.metadata_opts,
                    &mut failed_dirs,
                    setup.acl_cache.as_deref(),
                    #[cfg(unix)]
                    setup.sandbox.as_deref(),
                )?;
                match result {
                    Some(true) => {
                        stats.directories_created += 1;
                        // upstream: generator.c:1432 - itemize new directory
                        let iflags = crate::generator::ItemFlags::from_raw(
                            crate::generator::ItemFlags::ITEM_LOCAL_CHANGE
                                | crate::generator::ItemFlags::ITEM_IS_NEW,
                        );
                        let _ = self.emit_itemize(writer, &iflags, file_entry);
                    }
                    Some(false) => {
                        // upstream: generator.c:2260 - existing dir, metadata only
                        let iflags = crate::generator::ItemFlags::from_raw(0);
                        let _ = self.emit_itemize(writer, &iflags, file_entry);
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

        // upstream: generator.c:1348-1354 - missing_args == 2 && file->mode == 0
        // deletes the destination path and skips any creation for the sentinel.
        self.process_missing_args_sentinels(
            &setup.dest_dir,
            #[cfg(unix)]
            setup.sandbox.as_deref(),
        )?;

        // Mirror `run_pipelined`: when `--delete` is in effect, sweep the
        // destination for extraneous entries and capture per-type counters.
        // Upstream accumulates these globally across all delete paths so they
        // can be reported via stats and emitted as NDX_DEL_STATS.
        // upstream: generator.c:do_delete_pass()
        let mut delete_stats = DeleteStats::new();
        let mut delete_limit_exceeded = false;
        if self.config.flags.delete {
            let (ds, exceeded, io_bits) = self.delete_extraneous_files(
                &setup.dest_dir,
                #[cfg(unix)]
                setup.sandbox.as_ref(),
                writer,
            )?;
            delete_stats = ds;
            delete_limit_exceeded = exceeded;
            stats.io_error |= io_bits;
            // Carry the counters into the receiver context so the goodbye
            // handshake can emit NDX_DEL_STATS to the peer sender. URV-6.b
            // landed the on-disk deletion but the counters were never
            // propagated to the wire, so client `--stats` reported
            // "Number of deleted files: 0" on every daemon upload.
            // upstream: generator.c:2393-2398 - early write_del_stats() emission.
            self.pending_del_stats = delete_stats;
        }

        let files_to_transfer = self.build_files_to_transfer(
            writer,
            &setup.dest_dir,
            &setup.metadata_opts,
            Some(&failed_dirs),
            &mut metadata_errors,
            &mut stats,
            setup.acl_cache.as_deref(),
            #[cfg(unix)]
            setup.sandbox.as_deref(),
        );

        let mut files_transferred: usize = 0;
        let mut bytes_received: u64 = 0;
        let mut literal_data: u64 = 0;
        let mut matched_data: u64 = 0;
        let mut redo_count: usize = 0;
        let mut all_delayed_updates: Vec<(PathBuf, PathBuf)> = Vec::new();

        // upstream: generator.c:1845 - dry_run sends NDX requests without data
        if self.config.flags.dry_run {
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

                let redo_files: Vec<(usize, &FileEntry, PathBuf)> = redo_indices
                    .iter()
                    .filter_map(|&idx| {
                        self.file_list.get(idx).map(|entry| {
                            let p = entry.path();
                            let file_path = if p.as_os_str() == "." {
                                setup.dest_dir.clone()
                            } else {
                                setup.dest_dir.join(p)
                            };
                            (idx, entry, file_path)
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
        stats.delete_stats = delete_stats;
        stats.delete_limit_exceeded = delete_limit_exceeded;
        stats.redo_count = redo_count;

        self.finalize_transfer(reader, writer)?;

        Ok(stats)
    }
}
