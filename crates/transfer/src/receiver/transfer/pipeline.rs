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
use protocol::codec::{MonotonicNdxWriter, NdxCodec, create_ndx_codec};
use protocol::flist::FileEntry;

use crate::delta_apply::ChecksumVerifier;
use crate::pipeline::{PipelineConfig, PipelineState};
use crate::receiver::basis::{BasisFileConfig, find_basis_file_with_config};
use crate::receiver::{PipelineSetup, ReceiverContext};
use crate::transfer_ops::{
    RequestConfig, ResponseContext, process_file_response_streaming, send_file_request,
};

/// Result type for the pipelined transfer closure:
/// `(files_transferred, bytes, literal, matched, redo_indices, delayed_updates)`.
type PipelineResult = (usize, u64, u64, u64, Vec<usize>, Vec<(PathBuf, PathBuf)>);

impl ReceiverContext {
    /// Pipelined transfer loop with decoupled network/disk I/O.
    ///
    /// Fills a sliding window of file requests, computes signatures in parallel
    /// when the batch exceeds the configured signature threshold, then processes responses
    /// with a background disk commit thread. Returns
    /// `(files_transferred, bytes, literal, matched, redo_indices, delayed_updates)`.
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
        files_to_transfer: Vec<(usize, &FileEntry, PathBuf, u32)>,
        metadata_errors: &mut Vec<(PathBuf, String)>,
        is_redo_pass: bool,
        total_files: usize,
        progress: &mut Option<&mut dyn crate::TransferProgressCallback>,
    ) -> io::Result<PipelineResult> {
        use crate::disk_commit::{BackupConfig, DiskCommitConfig, PartialMode};
        use crate::pipeline::receiver::PipelinedReceiver;
        use crate::shared::TransferDeadline;

        // Early return when there is nothing to transfer - avoids spawning
        // the disk-commit thread, creating codecs, and pipeline state.
        // Flush buffered itemize messages from build_files_to_transfer()
        // so the generator sees them before the NDX_DONE handshake.
        // upstream: generator.c sends itemize immediately per-file via rwrite()
        if files_to_transfer.is_empty() {
            writer.flush()?;
            return Ok((0, 0, 0, 0, Vec::new(), Vec::new()));
        }

        let deadline = TransferDeadline::from_system_time(self.config.stop_at);

        let mut ndx_write_codec = MonotonicNdxWriter::new(self.protocol.as_u8());
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
            io_uring_depth: self.config.write.io_uring_depth,
            preserve_xattrs: self.config.flags.xattrs,
            want_xattr_optim: self.protocol.as_u8() >= 31
                && self.compat_flags.is_some_and(|f| {
                    !f.contains(protocol::CompatibilityFlags::AVOID_XATTR_OPTIMIZATION)
                }),
            append: self.config.flags.append,
        };

        // upstream: token.c uses a single compression context across all files.
        // For zstd the DCtx must persist across file boundaries (continuous
        // stream), so the reader is built once and reused for the session.
        let mut token_reader = request_config.create_token_reader()?;

        let mut pipeline = PipelineState::new(pipeline_config);
        let mut file_iter = files_to_transfer.into_iter();
        let mut pending_files_info: VecDeque<(usize, PathBuf, &FileEntry, u32)> =
            VecDeque::with_capacity(pipeline.window_size());

        // Basis read-ahead prefetch (default-off, feature-gated). Selecting the
        // prefetcher never changes wire output: `posix_fadvise(WILLNEED)` is a
        // pure page-cache timing hint. Disabled for --inplace / --append (a
        // file's write mutates a later file's basis) and on the redo pass
        // (redo requests carry no basis).
        let prefetcher =
            crate::basis_prefetch::select_prefetcher(crate::basis_prefetch::PrefetchDisableList {
                inplace: request_config.inplace,
                append: request_config.append,
            });
        let mut files_transferred = 0usize;
        let mut bytes_received = 0u64;
        let mut total_literal_bytes = 0u64;
        let mut total_matched_bytes = 0u64;

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
                suffix: self.config.effective_backup_suffix().into(),
            })
        } else {
            None
        };
        let file_list_arc = Arc::new(self.file_list.clone());
        // upstream: cleanup.c - compute partial mode from --partial / --partial-dir flags
        let partial_mode = if let Some(ref dir) = self.config.partial_dir {
            PartialMode::PartialDir(dir.clone())
        } else if self.config.flags.partial {
            PartialMode::Partial
        } else {
            PartialMode::None
        };
        let disk_config = DiskCommitConfig {
            do_fsync: self.config.write.fsync,
            use_sparse: self.config.flags.sparse,
            dest_dir: Some(setup.dest_dir.clone()),
            #[cfg(unix)]
            sandbox: setup.sandbox.clone(),
            temp_dir: self.config.temp_dir.as_ref().map(PathBuf::from),
            file_list: Some(file_list_arc),
            metadata_opts: Some(setup.metadata_opts.clone()),
            backup,
            acl_cache: setup.acl_cache.clone(),
            io_uring_policy: self.config.write.io_uring_policy,
            io_uring_depth: self.config.write.io_uring_depth,
            partial_mode,
            delay_updates: self.config.write.delay_updates,
            ..DiskCommitConfig::default()
        };
        let mut pipelined_receiver = PipelinedReceiver::new(disk_config)?;
        if is_redo_pass {
            let _ = pipelined_receiver.take_redo_indices();
        }

        let result = (|| -> io::Result<PipelineResult> {
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

                // Collect a batch of files, compute signatures (potentially in
                // parallel for incremental sync), then send requests sequentially.
                {
                    use rayon::prelude::*;

                    let batch: Vec<_> = file_iter
                        .by_ref()
                        .take(pipeline.available_slots())
                        .collect();

                    if !batch.is_empty() && !is_redo_pass {
                        // Extract basis config fields for the closure to avoid
                        // capturing &self across rayon worker boundaries.
                        let fuzzy_level = self.config.flags.fuzzy_level;
                        let ref_dirs = &self.config.reference_directories;
                        let protocol = self.protocol;
                        let whole_file = self.config.flags.whole_file;
                        let dest_dir = &setup.dest_dir;
                        let checksum_length = setup.checksum_length;
                        let checksum_algorithm = setup.checksum_algorithm;
                        let sig_threshold = self
                            .parallel_thresholds
                            .for_op(crate::parallel_io::ParallelOp::Signature);

                        // Ordering: wire protocol requires file requests in file-list index order.
                        // Preserved by par_iter().map().collect() + sequential zip/send loop below.
                        // Violation sends signatures for wrong files, corrupting delta transfer.
                        let sig_results: Vec<_> = if batch.len() >= sig_threshold {
                            batch
                                .par_iter()
                                .map(|(_, file_entry, file_path, _)| {
                                    let basis_config = BasisFileConfig {
                                        file_path,
                                        dest_dir,
                                        relative_path: file_entry.path(),
                                        target_size: file_entry.size(),
                                        fuzzy_level,
                                        reference_directories: ref_dirs,
                                        protocol,
                                        checksum_length,
                                        checksum_algorithm,
                                        whole_file,
                                    };
                                    find_basis_file_with_config(&basis_config)
                                })
                                .collect()
                        } else {
                            batch
                                .iter()
                                .map(|(_, file_entry, file_path, _)| {
                                    let basis_config = BasisFileConfig {
                                        file_path,
                                        dest_dir,
                                        relative_path: file_entry.path(),
                                        target_size: file_entry.size(),
                                        fuzzy_level,
                                        reference_directories: ref_dirs,
                                        protocol,
                                        checksum_length,
                                        checksum_algorithm,
                                        whole_file,
                                    };
                                    find_basis_file_with_config(&basis_config)
                                })
                                .collect()
                        };

                        // Basis read-ahead: warm the resolved basis paths for the
                        // look-ahead window (this batch) so the disk-commit stage
                        // finds their pages in cache instead of blocking on serial
                        // reads. Uses BasisFileResult::basis_path (the RESOLVED
                        // path - fuzzy/reference-dir aware), not a naive dest/name.
                        // No-op (NullPrefetcher) on the default build. Bounded to
                        // PREFETCH_DEPTH entries to match the channel depth.
                        for basis_result in sig_results
                            .iter()
                            .take(crate::basis_prefetch::PREFETCH_DEPTH)
                        {
                            if let Some(ref basis_path) = basis_result.basis_path {
                                prefetcher.prefetch(basis_path);
                            }
                        }

                        // Send requests sequentially (wire order matters).
                        for ((file_idx, file_entry, file_path, base_iflags), basis_result) in
                            batch.into_iter().zip(sig_results)
                        {
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
                                base_iflags,
                            ));
                        }
                    } else {
                        // Redo pass or empty batch: no basis files, skip signatures.
                        for (file_idx, file_entry, file_path, base_iflags) in batch {
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
                                file_idx,
                                file_path,
                                file_entry,
                                base_iflags,
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
                let (file_idx, file_path, file_entry, base_iflags) =
                    pending_files_info.pop_front().expect("pipeline not empty");

                let response_ctx = ResponseContext {
                    config: &request_config,
                    #[cfg(unix)]
                    sandbox: setup.sandbox.as_ref(),
                    #[cfg(unix)]
                    dest_dir: Some(setup.dest_dir.as_path()),
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

                // Non-blocking: collect any ready disk results.
                let (_disk_bytes, disk_meta_errors) = pipelined_receiver.drain_ready_results()?;
                metadata_errors.extend(disk_meta_errors);

                // Route accumulated warnings through the multiplexed writer
                // instead of eprintln (which deadlocks in daemon handler threads).
                // Fatal transfer errors ride MSG_ERROR_XFER so the peer sets
                // got_xfer_error (exit 23); everything else is MSG_INFO.
                for (code, warning) in pipelined_receiver.drain_warnings() {
                    let _ = if code == protocol::MessageCode::ErrorXfer {
                        writer.send_msg_error_xfer(warning.as_bytes())
                    } else {
                        writer.send_msg_info(warning.as_bytes())
                    };
                }

                // upstream: io.c:820 stats.total_read only counts bytes read
                // off the wire. Matched-from-basis bytes never traverse the
                // read fd, so exclude them from bytes_received.
                bytes_received += result.literal_bytes;
                total_literal_bytes += result.literal_bytes;
                total_matched_bytes += result.matched_bytes;
                files_transferred += 1;

                // upstream: receiver.c:950 - log_item() after successful file transfer
                {
                    if self.config.flags.verbose && self.config.connection.client_mode {
                        info_log!(Name, 1, "{}", file_entry.path().display());
                    }
                    // upstream: generator.c:1925-1937 - the transfer itemize is
                    // emitted right after the file request. With
                    // log_before_transfer == 0 (`am_server`) the row is logged
                    // after the transfer, so a server-mode receiver emits it
                    // here. Client-mode receivers (log_before_transfer == 1)
                    // already emitted it in the linear candidate pass to keep
                    // the stdout interleaving with skip/unchanged rows.
                    if !self.config.connection.client_mode {
                        use crate::generator::ItemFlags;
                        let iflags = ItemFlags::from_raw(base_iflags);
                        let _ = self.emit_itemize(writer, &iflags, file_entry);
                    }
                }

                if let Some(cb) = progress.as_mut() {
                    let event = crate::TransferProgressEvent {
                        path: file_entry.path(),
                        file_bytes: result.total_bytes,
                        total_file_bytes: Some(file_entry.size()),
                        files_done: files_transferred,
                        total_files,
                        // Receiver-side INC_RECURSE collects every sub-list via
                        // `receive_extra_file_lists` before the pipeline begins,
                        // so the file list is always complete when progress is
                        // emitted. upstream: progress.c:79-82 rprint_progress.
                        flist_eof: true,
                    };
                    cb.on_file_transferred(&event);
                }
            }

            // Drain all remaining disk results
            let (_disk_bytes, disk_meta_errors) = pipelined_receiver.drain_all_results()?;
            metadata_errors.extend(disk_meta_errors);

            // Route accumulated warnings through the multiplexed writer.
            // Fatal transfer errors ride MSG_ERROR_XFER so the peer sets
            // got_xfer_error (exit 23); everything else is MSG_INFO.
            for (code, warning) in pipelined_receiver.drain_warnings() {
                let _ = if code == protocol::MessageCode::ErrorXfer {
                    writer.send_msg_error_xfer(warning.as_bytes())
                } else {
                    writer.send_msg_info(warning.as_bytes())
                };
            }

            let redo_indices = pipelined_receiver.take_redo_indices();
            let delayed = pipelined_receiver.take_delayed_updates();

            Ok((
                files_transferred,
                bytes_received,
                total_literal_bytes,
                total_matched_bytes,
                redo_indices,
                delayed,
            ))
        })();

        // Graceful shutdown regardless of success or failure.
        let _ = pipelined_receiver.shutdown();

        result
    }

    /// Dry-run transfer loop: sends NDX requests without data transfer.
    ///
    /// Mirrors upstream generator.c behavior during `!do_xfers`: sends NDX and
    /// iflags for each file candidate, reads the echoed NDX+iflags from the
    /// sender. No sum head or file data is exchanged. This allows the sender to
    /// log each file name for verbose output.
    ///
    /// upstream: generator.c:1845-1946 - `!do_xfers` path sends write_ndx() then
    /// goto cleanup, skipping write_sum_head(). sender.c:394-399 - `!do_xfers`
    /// logs the item and echoes write_ndx_and_attrs() without receive_sums().
    pub(in crate::receiver) fn run_dry_run_loop<
        R: Read,
        W: Write + crate::writer::MsgInfoSender + ?Sized,
    >(
        &self,
        reader: &mut crate::reader::ServerReader<R>,
        writer: &mut W,
        files_to_transfer: &[(usize, &FileEntry, PathBuf, u32)],
    ) -> io::Result<()> {
        if files_to_transfer.is_empty() {
            writer.flush()?;
            return Ok(());
        }

        let mut ndx_write_codec = MonotonicNdxWriter::new(self.protocol.as_u8());
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());
        let write_iflags = self.protocol.supports_iflags();
        let preserve_xattrs = self.config.flags.xattrs;
        let want_xattr_optim = self.protocol.as_u8() >= 31
            && self.compat_flags.is_some_and(|f| {
                !f.contains(protocol::CompatibilityFlags::AVOID_XATTR_OPTIMIZATION)
            });

        // upstream: io.c perform_io() flushes output via select() while waiting
        // for input. We flush once before blocking on each response read, but
        // only when needed (the multiplex dirty-flag skips redundant syscalls).
        for &(file_idx, file_entry, _, _) in files_to_transfer {
            // upstream: generator.c:1925 - write_ndx(f_out, ndx)
            let wire_ndx = self.flat_to_wire_ndx(file_idx);
            ndx_write_codec.write_ndx(&mut *writer, wire_ndx)?;

            // upstream: generator.c:1926 - iflags with ITEM_TRANSFER
            if write_iflags {
                use crate::receiver::wire::SenderAttrs;
                writer.write_all(&SenderAttrs::ITEM_TRANSFER.to_le_bytes())?;
            }

            // Flush before blocking on the sender's echo. The multiplex
            // writer's dirty-flag optimization skips the syscall when the
            // request fits within the 64KB buffer and no prior data was
            // pending - matching upstream's batched iobuf_out pattern.
            writer.flush()?;

            // upstream: sender.c:394-399 - sender echoes write_ndx_and_attrs back
            let (_echoed_ndx, _sender_attrs) =
                crate::receiver::wire::SenderAttrs::read_with_codec_xattr(
                    reader,
                    &mut ndx_read_codec,
                    preserve_xattrs,
                    want_xattr_optim,
                )?;

            // upstream: rsync.c:672-676 set_file_attrs emits the bare-name
            // notice AFTER the transfer decision is known. In dry-run the
            // sender's echo confirms the file would have been transferred,
            // so emit the "updated" line at the post-decision point to
            // match upstream wire order.
            if self.config.flags.verbose && self.config.connection.client_mode {
                info_log!(Name, 1, "{}", file_entry.path().display());
            }
        }

        writer.flush()?;
        Ok(())
    }
}
