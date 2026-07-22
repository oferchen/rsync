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

use logging::{debug_log, info_log};
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
/// `(files_transferred, transferred_file_size, bytes, literal, matched, redo_indices,
/// delayed_updates)`. `transferred_file_size` mirrors upstream `receiver.c:784`
/// `stats.total_transferred_size += F_LENGTH(file)`, summed at the same point as
/// `files_transferred`.
type PipelineResult = (
    usize,
    u64,
    u64,
    u64,
    u64,
    Vec<usize>,
    Vec<(PathBuf, PathBuf)>,
);

impl ReceiverContext {
    /// Emits `MSG_SUCCESS(ndx)` to the sender for every file whose commit was
    /// confirmed since the last drain, when `--remove-source-files` is active.
    ///
    /// The sender defers its source unlink until it receives this confirmation,
    /// so this is what lets the sender remove a source only after the file has
    /// safely landed at the destination. When the flag is off the confirmed
    /// indices are drained and discarded, keeping the accumulator bounded.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:1063-1069` - `send_msg_success(fname, ndx)` on `recv_ok == 1`.
    /// - `io.c:1623-1637` - sender-side `MSG_SUCCESS` handler -> `successful_send`.
    fn emit_confirmed_source_removals<W>(
        &self,
        writer: &mut W,
        pipelined_receiver: &mut crate::pipeline::receiver::PipelinedReceiver,
    ) -> io::Result<()>
    where
        W: crate::writer::MsgInfoSender + ?Sized,
    {
        let confirmed = pipelined_receiver.drain_new_success_indices();
        if !self.config.flags.remove_source_files {
            return Ok(());
        }
        for flat_idx in confirmed {
            writer.send_msg_success(self.flat_to_wire_ndx(flat_idx))?;
        }
        Ok(())
    }

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
            return Ok((0, 0, 0, 0, 0, Vec::new(), Vec::new()));
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
            // upstream: receiver.c:761-773 - a phase-2 redo negates append_mode
            // (`append_mode = -append_mode`), so the re-request is a full
            // transfer that overwrites the file rather than appending to a
            // prefix the verify pass already rejected.
            append: self.config.flags.append && !is_redo_pass,
            append_verify: self.config.flags.append_verify && !is_redo_pass,
        };

        // upstream: token.c uses a single compression context across all files.
        // For zstd the DCtx must persist across file boundaries (continuous
        // stream), so the reader is built once and reused for the session.
        let mut token_reader = request_config.create_token_reader()?;

        let mut pipeline = PipelineState::new(pipeline_config);
        let mut file_iter = files_to_transfer.into_iter();
        let mut pending_files_info: VecDeque<(usize, PathBuf, &FileEntry, u32)> =
            VecDeque::with_capacity(pipeline.window_size());
        let mut files_transferred = 0usize;
        // upstream: receiver.c:784 stats.total_transferred_size += F_LENGTH(file),
        // summed at the same point as files_transferred.
        let mut transferred_file_size = 0u64;
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
            preallocate: self.config.flags.preallocate,
            dest_dir: Some(setup.dest_dir.clone()),
            #[cfg(unix)]
            sandbox: setup.sandbox.clone(),
            temp_dir: self.config.temp_dir.as_ref().map(PathBuf::from),
            file_list: Some(file_list_arc),
            metadata_opts: Some(setup.metadata_opts.clone()),
            backup,
            acl_cache: setup.acl_cache.clone(),
            acl_id_map: setup.acl_id_map.clone(),
            xattr_filter: self.xattr_name_filter_arc(),
            io_uring_policy: self.config.write.io_uring_policy,
            io_uring_depth: self.config.write.io_uring_depth,
            partial_mode,
            delay_updates: self.config.write.delay_updates,
            append_verify: self.config.flags.append_verify && !is_redo_pass,
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
                        let partial_dir = self.config.partial_dir.as_deref();
                        let protocol = self.protocol;
                        let compat_flags = self.compat_flags;
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
                                        target_mtime: file_entry.mtime(),
                                        fuzzy_level,
                                        reference_directories: ref_dirs,
                                        partial_dir,
                                        protocol,
                                        checksum_length,
                                        checksum_algorithm,
                                        whole_file,
                                        compat_flags,
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
                                        target_mtime: file_entry.mtime(),
                                        fuzzy_level,
                                        reference_directories: ref_dirs,
                                        partial_dir,
                                        protocol,
                                        checksum_length,
                                        checksum_algorithm,
                                        whole_file,
                                        compat_flags,
                                    };
                                    find_basis_file_with_config(&basis_config)
                                })
                                .collect()
                        };

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
                                basis_result.fnamecmp_type,
                                basis_result.xname.as_deref(),
                                file_entry.size(),
                                base_iflags,
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
                                protocol::FnameCmpType::Fname,
                                None,
                                file_entry.size(),
                                base_iflags,
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

                // upstream: receiver.c:708-709 DEBUG_GTE(RECV, 1)
                debug_log!(Recv, 1, "recv_files({})", file_entry.path().display());

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
                    result.is_inplace,
                );

                // Non-blocking: collect any ready disk results.
                let (_disk_bytes, disk_meta_errors) = pipelined_receiver.drain_ready_results()?;
                metadata_errors.extend(disk_meta_errors);

                // upstream: receiver.c:1063-1069 - a committed file (recv_ok == 1)
                // gets an immediate MSG_SUCCESS so the sender can unlink its
                // --remove-source-files source. Emit for every file the drain
                // just confirmed committed.
                self.emit_confirmed_source_removals(writer, &mut pipelined_receiver)?;

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
                transferred_file_size += file_entry.size();

                // upstream: receiver.c:950 - log_item() after successful file transfer
                {
                    if self.config.flags.verbose && self.config.connection.client_mode {
                        if self.interleave_names && !is_redo_pass {
                            // upstream: receiver.c:1008-1012 - the client prints
                            // each file's name per file (log_before_transfer),
                            // in flist order, interleaved with --progress,
                            // instead of buffering for an end-of-run block. The
                            // phase-2 redo re-transfers already-named files, so
                            // it must not re-emit.
                            if self.progress_active {
                                // The live --progress renderer already prints
                                // this file's name before its bar; only release
                                // the directory names that precede it (the
                                // renderer never sees directory entries).
                                let _ = self.flush_names_through(file_idx);
                            } else {
                                let _ = self.emit_name_in_order(
                                    file_idx,
                                    format!("{}\n", file_entry.path().display()),
                                );
                            }
                        } else if !self.should_emit_itemize() {
                            // Plain `-v`: bare name. Under `-i`/`-vi` the
                            // itemize row already carries the name, so suppress
                            // this to avoid a duplicate line.
                            info_log!(Name, 1, "{}", file_entry.path().display());
                        }
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
                        // Routed through the deferral seam for consistency with
                        // the other emit sites. A server-mode receiver never
                        // produces a client-visible row (record_itemize gates on
                        // client_mode), so this stays the no-op it already was,
                        // whether or not deferral is active.
                        let _ = self.emit_or_record_itemize(writer, file_idx, &iflags, file_entry);
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

            // upstream: receiver.c:1063-1069 - flush MSG_SUCCESS for the final
            // batch of files the blocking drain just confirmed committed, so the
            // sender unlinks their --remove-source-files sources.
            self.emit_confirmed_source_removals(writer, &mut pipelined_receiver)?;

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

            // upstream: generator.c:2169 finish_hard_link() itemizes every
            // follower once the leader completes, before the phase-1 NDX_DONE.
            // Emit through the request-phase NDX diff-state (never the redo
            // pass, which carries no new followers) so a pushing client's
            // sender renders each `hf...` / `=> leader` row.
            if !is_redo_pass {
                self.emit_server_hardlink_follower_itemize(writer, ndx_write_codec.inner_mut())?;
            }

            let redo_indices = pipelined_receiver.take_redo_indices();
            let delayed = pipelined_receiver.take_delayed_updates();

            Ok((
                files_transferred,
                transferred_file_size,
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
        for &(file_idx, file_entry, _, base_iflags) in files_to_transfer {
            // upstream: generator.c:1925 - write_ndx(f_out, ndx)
            let wire_ndx = self.flat_to_wire_ndx(file_idx);
            ndx_write_codec.write_ndx(&mut *writer, wire_ndx)?;

            // upstream: generator.c:1937-1947 - iflags carry the full itemize
            // bits (ITEM_TRANSFER plus the pre-transfer attribute diff, incl.
            // ITEM_IS_NEW for a new dest) so the sender prints the right glyph
            // (e.g. `<f+++++++++` for a new file) even in a dry run.
            if write_iflags {
                use crate::receiver::wire::SenderAttrs;
                let iflags = ((base_iflags & 0xFFFF) as u16) | SenderAttrs::ITEM_TRANSFER;
                writer.write_all(&iflags.to_le_bytes())?;
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
            // match upstream wire order. Under `-i`/`-vi` the itemize row
            // already carries the name, so the bare name is suppressed.
            if self.config.flags.verbose
                && self.config.connection.client_mode
                && !self.should_emit_itemize()
            {
                info_log!(Name, 1, "{}", file_entry.path().display());
            }
        }

        writer.flush()?;
        Ok(())
    }

    /// Server-receiver loop for `--only-write-batch=X` (upstream `write_batch
    /// < 0`).
    ///
    /// Unlike [`run_dry_run_loop`](Self::run_dry_run_loop), the generator sends
    /// REAL block checksums (a full sum head + signature per file), because
    /// upstream forces `dry_run = 1` only after `do_xfers` is computed
    /// (main.c:1839), so `do_xfers` stays 1 and the sender needs the checksums
    /// to build its batch. The push sender writes each delta to its own batch
    /// fd rather than the wire (sender.c:217 `f_xfer = write_batch < 0 ?
    /// batch_fd : f_out`), so this loop reads only the bare NDX+attrs echo the
    /// sender emits via `write_ndx_and_attrs(f_out, ...)` (sender.c:442) and
    /// writes nothing to the destination.
    ///
    /// Requests are sent one file at a time, flushing before each echo read, so
    /// there is no risk of a buffer-fill deadlock against a large signature.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:1839` - `if (write_batch < 0) dry_run = 1` (do_xfers stays 1)
    /// - `sender.c:442-443` - `write_ndx_and_attrs(f_out); write_sum_head(f_xfer)`
    /// - `receiver.c:811-817` - `write_batch < 0` path: log, no dest write
    pub(in crate::receiver) fn run_only_write_batch_loop<
        R: Read,
        W: Write + crate::writer::MsgInfoSender + ?Sized,
    >(
        &self,
        reader: &mut crate::reader::ServerReader<R>,
        writer: &mut W,
        files_to_transfer: &[(usize, &FileEntry, PathBuf, u32)],
        setup: &PipelineSetup,
    ) -> io::Result<()> {
        if files_to_transfer.is_empty() {
            writer.flush()?;
            return Ok(());
        }

        let mut ndx_write_codec = MonotonicNdxWriter::new(self.protocol.as_u8());
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        let preserve_xattrs = self.config.flags.xattrs;
        let want_xattr_optim = self.protocol.as_u8() >= 31
            && self.compat_flags.is_some_and(|f| {
                !f.contains(protocol::CompatibilityFlags::AVOID_XATTR_OPTIMIZATION)
            });

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
            preserve_xattrs,
            want_xattr_optim,
            append: self.config.flags.append,
            append_verify: self.config.flags.append_verify,
        };

        for &(file_idx, file_entry, ref file_path, base_iflags) in files_to_transfer {
            // upstream: generator.c:1961-1969 - compute the basis signature and
            // send a real sum head so the sender can diff against the receiver's
            // basis (empty when no basis exists, driving a whole-file batch).
            let basis_config = BasisFileConfig {
                file_path,
                dest_dir: &setup.dest_dir,
                relative_path: file_entry.path(),
                target_size: file_entry.size(),
                target_mtime: file_entry.mtime(),
                fuzzy_level: self.config.flags.fuzzy_level,
                reference_directories: &self.config.reference_directories,
                partial_dir: self.config.partial_dir.as_deref(),
                protocol: self.protocol,
                checksum_length: setup.checksum_length,
                checksum_algorithm: setup.checksum_algorithm,
                whole_file: self.config.flags.whole_file,
                compat_flags: self.compat_flags,
            };
            let basis = find_basis_file_with_config(&basis_config);

            // upstream: generator.c:1939 write_ndx + write_sum_head(f_out, s).
            let _pending = send_file_request(
                writer,
                &mut ndx_write_codec,
                self.flat_to_wire_ndx(file_idx),
                file_path.clone(),
                basis.signature,
                basis.basis_path,
                basis.fnamecmp_type,
                basis.xname.as_deref(),
                file_entry.size(),
                base_iflags,
                &request_config,
            )?;

            // Flush before blocking on the echo so the sender has the full
            // request. It reads the sum head, writes the delta into its own
            // batch fd, and echoes only NDX+attrs back to us.
            writer.flush()?;

            // upstream: sender.c:442 - write_ndx_and_attrs(f_out, ...) echo.
            // No delta follows (it went to the batch fd), so we stop here and
            // write nothing to the destination.
            let (_echoed_ndx, _sender_attrs) =
                crate::receiver::wire::SenderAttrs::read_with_codec_xattr(
                    reader,
                    &mut ndx_read_codec,
                    preserve_xattrs,
                    want_xattr_optim,
                )?;
        }

        writer.flush()?;
        Ok(())
    }
}
