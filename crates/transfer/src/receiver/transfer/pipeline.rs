//! Pipelined transfer loop with decoupled network and disk I/O.
//!
//! Implements the core pipeline that fills a request window, computes
//! signatures (parallel when batch is large enough), sends requests
//! sequentially, and processes responses with a background disk commit
//! thread. Used by both `run_pipelined` and `run_pipelined_incremental`.
//!
//! # Upstream Reference
//!
//! - `receiver.c:720` - `recv_files()` main reception loop
//! - `generator.c:2157-2163` - phase 1 vs phase 2 checksum length selection
//! - `io.c:perform_io()` - upstream bidirectional I/O batching via `select()`

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use logging::info_log;
use protocol::codec::create_ndx_codec;
use protocol::flist::FileEntry;

use crate::delta_apply::ChecksumVerifier;
use crate::pipeline::{PipelineConfig, PipelineState};
use crate::receiver::basis::find_basis_file_with_config;
use crate::receiver::{PARALLEL_STAT_THRESHOLD, PipelineSetup, ReceiverContext};
use crate::transfer_ops::{
    RequestConfig, ResponseContext, process_file_response_streaming, send_file_request,
};

impl ReceiverContext {
    /// Pipelined transfer loop with decoupled network/disk I/O.
    ///
    /// Fills a sliding window of file requests, computes signatures in parallel
    /// when the batch exceeds `PARALLEL_STAT_THRESHOLD`, then processes responses
    /// with a background disk commit thread. Returns `(files_transferred, bytes, redo_indices)`.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::receiver) fn run_pipeline_loop_decoupled<
        R: Read,
        W: Write + crate::writer::MsgInfoSender + ?Sized,
    >(
        &self,
        reader: &mut crate::reader::ServerReader<R>,
        writer: &mut W,
        pipeline_config: PipelineConfig,
        setup: &PipelineSetup,
        files_to_transfer: Vec<(usize, &FileEntry, PathBuf)>,
        metadata_errors: &mut Vec<(PathBuf, String)>,
        is_redo_pass: bool,
        total_files: usize,
        progress: &mut Option<&mut dyn crate::TransferProgressCallback>,
    ) -> io::Result<(usize, u64, Vec<usize>)> {
        use crate::disk_commit::{BackupConfig, DiskCommitConfig};
        use crate::pipeline::receiver::PipelinedReceiver;
        use crate::shared::TransferDeadline;

        // Early return when there is nothing to transfer - avoids spawning
        // the disk-commit thread, creating codecs, and pipeline state.
        // Flush buffered itemize messages from build_files_to_transfer()
        // so the generator sees them before the NDX_DONE handshake.
        // upstream: generator.c sends itemize immediately per-file via rwrite()
        if files_to_transfer.is_empty() {
            writer.flush()?;
            return Ok((0, 0, Vec::new()));
        }

        let deadline = TransferDeadline::from_system_time(self.config.stop_at);

        let mut ndx_write_codec = create_ndx_codec(self.protocol.as_u8());
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        let request_config = RequestConfig {
            protocol: self.protocol,
            write_iflags: self.protocol.supports_iflags(),
            checksum_length: setup.checksum_length,
            checksum_algorithm: setup.checksum_algorithm,
            negotiated_algorithms: self.negotiated_algorithms.as_ref(),
            compat_flags: self.compat_flags.as_ref(),
            checksum_seed: self.checksum_seed,
            use_sparse: self.config.flags.sparse,
            do_fsync: self.config.write.fsync,
            temp_dir: self.config.temp_dir.as_deref(),
            write_devices: self.config.write.write_devices,
            inplace: self.config.write.inplace,
            inplace_partial: self.config.write.inplace_partial,
            io_uring_policy: self.config.write.io_uring_policy,
            preserve_xattrs: self.config.flags.xattrs,
            want_xattr_optim: self.compat_flags.is_some_and(|f| {
                f.contains(protocol::CompatibilityFlags::AVOID_XATTR_OPTIMIZATION)
            }),
            append: self.config.flags.append,
        };

        // Create the token reader once for the entire transfer session.
        // upstream: token.c uses a single compression context across all files.
        // For zstd, the DCtx must persist across file boundaries (continuous stream).
        let mut token_reader = request_config.create_token_reader();

        let mut pipeline = PipelineState::new(pipeline_config);
        let mut file_iter = files_to_transfer.into_iter();
        let mut pending_files_info: VecDeque<(usize, PathBuf, &FileEntry, bool)> =
            VecDeque::with_capacity(pipeline.window_size());
        let mut files_transferred = 0usize;
        let mut bytes_received = 0u64;

        let mut checksum_verifier = ChecksumVerifier::new(
            self.negotiated_algorithms.as_ref(),
            self.protocol,
            self.checksum_seed,
            self.compat_flags.as_ref(),
        );
        let backup = if self.config.flags.backup {
            Some(BackupConfig {
                dest_dir: setup.dest_dir.clone(),
                backup_dir: self.config.backup_dir.as_ref().map(PathBuf::from),
                suffix: self.config.backup_suffix.as_deref().unwrap_or("~").into(),
            })
        } else {
            None
        };
        let file_list_arc = Arc::new(self.file_list.clone());
        let disk_config = DiskCommitConfig {
            do_fsync: self.config.write.fsync,
            use_sparse: self.config.flags.sparse,
            temp_dir: self.config.temp_dir.as_ref().map(PathBuf::from),
            file_list: Some(file_list_arc),
            metadata_opts: Some(setup.metadata_opts.clone()),
            backup,
            acl_cache: setup.acl_cache.clone(),
            ..DiskCommitConfig::default()
        };
        let mut pipelined_receiver = PipelinedReceiver::new(disk_config);
        if is_redo_pass {
            let _ = pipelined_receiver.take_redo_indices();
        }

        let result = (|| -> io::Result<(usize, u64, Vec<usize>)> {
            // Track how many requests the sender has received (flushed) but
            // not yet responded to. We only flush the write buffer when this
            // drops to zero - otherwise the sender already has queued requests.
            //
            // upstream: io.c perform_io() uses select() for bidirectional I/O,
            // naturally batching writes until the output buffer is full.
            let mut flushed_pending: usize = 0;

            loop {
                if let Some(ref dl) = deadline {
                    if dl.is_reached() {
                        break;
                    }
                }

                // Fill the pipeline with requests.
                // Collect a batch of files, compute signatures (potentially in
                // parallel for incremental sync), then send requests sequentially.
                {
                    use rayon::prelude::*;

                    let batch: Vec<_> = file_iter
                        .by_ref()
                        .take(pipeline.available_slots())
                        .collect();

                    if !batch.is_empty() && !is_redo_pass {
                        // Compute signatures - parallel when batch is large enough.
                        let sig_results: Vec<_> = if batch.len() >= PARALLEL_STAT_THRESHOLD {
                            batch
                                .par_iter()
                                .map(|(_, file_entry, file_path)| {
                                    let basis_config = self.build_basis_file_config(
                                        file_path,
                                        &setup.dest_dir,
                                        file_entry.path(),
                                        file_entry.size(),
                                        setup.checksum_length,
                                        setup.checksum_algorithm,
                                    );
                                    find_basis_file_with_config(&basis_config)
                                })
                                .collect()
                        } else {
                            batch
                                .iter()
                                .map(|(_, file_entry, file_path)| {
                                    let basis_config = self.build_basis_file_config(
                                        file_path,
                                        &setup.dest_dir,
                                        file_entry.path(),
                                        file_entry.size(),
                                        setup.checksum_length,
                                        setup.checksum_algorithm,
                                    );
                                    find_basis_file_with_config(&basis_config)
                                })
                                .collect()
                        };

                        // Send requests sequentially (wire order matters).
                        for ((file_idx, file_entry, file_path), basis_result) in
                            batch.into_iter().zip(sig_results)
                        {
                            if self.config.flags.verbose && self.config.connection.client_mode {
                                info_log!(Name, 1, "{}", file_entry.path().display());
                            }

                            let is_new_file = basis_result.is_empty();
                            let pending = send_file_request(
                                writer,
                                &mut ndx_write_codec,
                                self.flat_to_wire_ndx(file_idx),
                                file_path.clone(),
                                basis_result.signature,
                                basis_result.basis_path,
                                file_entry.size(),
                                &request_config,
                            )?;

                            pipeline.push(pending);
                            pending_files_info.push_back((
                                file_idx,
                                file_path,
                                file_entry,
                                is_new_file,
                            ));
                        }
                    } else {
                        // Redo pass or empty batch: no basis files, skip signatures.
                        for (file_idx, file_entry, file_path) in batch {
                            if self.config.flags.verbose && self.config.connection.client_mode {
                                info_log!(Name, 1, "{}", file_entry.path().display());
                            }

                            let pending = send_file_request(
                                writer,
                                &mut ndx_write_codec,
                                self.flat_to_wire_ndx(file_idx),
                                file_path.clone(),
                                None,
                                None,
                                file_entry.size(),
                                &request_config,
                            )?;

                            pipeline.push(pending);
                            pending_files_info.push_back((
                                file_idx, file_path, file_entry,
                                true, // is_new_file: no basis in redo pass
                            ));
                        }
                    }
                }

                if pipeline.is_empty() {
                    break;
                }

                // Flush only when the sender has no queued requests left.
                if flushed_pending == 0 {
                    writer.flush()?;
                    flushed_pending = pipeline.outstanding();
                }

                // Process one response from a previously flushed request.
                let pending = pipeline.pop().expect("pipeline not empty");
                flushed_pending = flushed_pending.saturating_sub(1);
                let (file_idx, file_path, file_entry, is_new_file) =
                    pending_files_info.pop_front().expect("pipeline not empty");

                let response_ctx = ResponseContext {
                    config: &request_config,
                };

                let xattr_list = self.resolve_xattr_list(file_entry);
                let is_device_target = self.config.write.write_devices && file_entry.is_device();
                let result = process_file_response_streaming(
                    reader,
                    &mut ndx_read_codec,
                    pending,
                    &response_ctx,
                    &mut checksum_verifier,
                    pipelined_receiver.file_sender(),
                    pipelined_receiver.buf_return_rx(),
                    file_idx,
                    is_device_target,
                    xattr_list,
                    &mut token_reader,
                )?;

                pipelined_receiver.note_commit_sent(
                    result.expected_checksum,
                    result.checksum_len,
                    file_path,
                    file_idx,
                );

                // Non-blocking: collect any ready disk results
                let (disk_bytes, disk_meta_errors) = pipelined_receiver.drain_ready_results()?;
                bytes_received += disk_bytes;
                metadata_errors.extend(disk_meta_errors);

                bytes_received += result.total_bytes;
                files_transferred += 1;

                // upstream: receiver.c:950 - log_item() after successful file transfer
                {
                    use crate::generator::ItemFlags;
                    let raw = ItemFlags::ITEM_TRANSFER
                        | if is_new_file {
                            ItemFlags::ITEM_IS_NEW
                        } else {
                            0
                        };
                    let iflags = ItemFlags::from_raw(raw);
                    let _ = self.emit_itemize(writer, &iflags, file_entry);
                }

                if let Some(cb) = progress.as_mut() {
                    let event = crate::TransferProgressEvent {
                        path: file_entry.path(),
                        file_bytes: result.total_bytes,
                        total_file_bytes: Some(file_entry.size()),
                        files_done: files_transferred,
                        total_files,
                    };
                    cb.on_file_transferred(&event);
                }
            }

            // Drain all remaining disk results
            let (disk_bytes, disk_meta_errors) = pipelined_receiver.drain_all_results()?;
            bytes_received += disk_bytes;
            metadata_errors.extend(disk_meta_errors);

            let redo_indices = pipelined_receiver.take_redo_indices();

            Ok((files_transferred, bytes_received, redo_indices))
        })();

        // Graceful shutdown regardless of success or failure.
        let _ = pipelined_receiver.shutdown();

        result
    }
}
