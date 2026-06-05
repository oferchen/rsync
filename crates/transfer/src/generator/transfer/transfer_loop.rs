//! Main NDX-driven file transfer loop for the generator role.
//!
//! Contains `run_transfer_loop` which processes per-file NDX requests from the
//! receiver, generates deltas, and streams data over the wire across phases.
//!
//! # Upstream Reference
//!
//! - `sender.c:send_files()` - Main transfer loop (lines 210-462)

use std::io::{self, Read, Write};

use logging::debug_log;
use protocol::codec::{
    MonotonicNdxWriter, NDX_DEL_STATS, NDX_DONE, NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec,
    create_ndx_codec,
};
use protocol::stats::DeleteStats;

use super::super::delta::{
    create_token_encoder, script_to_wire_delta, stream_whole_file_transfer,
    write_delta_with_inline_checksum,
};
use super::super::item_flags::ItemFlags;
use super::super::{
    GeneratorContext, SegmentScheduler, TransferLoopResult, flush_with_count, is_early_close_error,
};
use crate::delta_config::DeltaGeneratorConfig;
use crate::receiver::SumHead;

impl GeneratorContext {
    /// Runs the main file transfer loop, reading NDX requests from receiver.
    ///
    /// This method processes file transfer requests in phases until all phases complete.
    /// For each file index received, it reads signatures, generates deltas, and sends data.
    ///
    /// # Upstream Reference
    ///
    /// - `sender.c:send_files()` - Main send loop (lines 210-462)
    /// - `io.c:read_ndx/write_ndx` - NDX protocol encoding
    pub(super) fn run_transfer_loop<R: Read, W: Write>(
        &mut self,
        reader: &mut R,
        writer: &mut super::super::super::writer::ServerWriter<W>,
        progress: &mut Option<&mut dyn super::super::super::TransferProgressCallback>,
        itemize: &mut Option<&mut dyn super::super::super::ItemizeCallback>,
    ) -> io::Result<TransferLoopResult> {
        use super::super::super::shared::TransferDeadline;
        use super::super::delta::generate_delta_from_signature;
        use super::super::protocol_io::read_signature_blocks;

        // upstream: sender.c:217-218 - rprintf(FINFO, "send_files starting\n")
        debug_log!(Send, 1, "send_files starting");

        // Phase handling: upstream sender.c line 210: max_phase = protocol_version >= 29 ? 2 : 1
        let mut phase: i32 = 0;
        let max_phase: i32 = if self.protocol.supports_iflags() {
            2
        } else {
            1
        };

        let deadline = TransferDeadline::from_system_time(self.config.stop_at);

        let mut files_transferred = 0;
        let mut bytes_sent = 0u64;
        // upstream: io.c IO_BUFFER_SIZE (32KB)
        let mut stream_buf = Vec::with_capacity(32 * 1024);

        // upstream: token.c creates a single compression context for the entire
        // transfer session. For zstd, the CCtx must persist across file boundaries
        // (one continuous stream). Create once here, reuse across all files.
        let negotiated_compression = self.negotiated_algorithms.map(|n| n.compression);
        let compression_threads = self.config.connection.compression_threads;
        let mut token_encoder = negotiated_compression
            .map(|algo| create_token_encoder(algo, compression_threads))
            .transpose()?
            .flatten();

        // upstream: io.c:2244-2245 - separate read/write NDX state
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());
        let mut ndx_write_codec = MonotonicNdxWriter::new(self.protocol.as_u8());

        // INC_RECURSE: create scheduler and wire encoding state for lazy sub-list sending.
        // upstream: sender.c:227,261 - interleaves sub-list sending with file transfers.
        let inc_recurse = self.inc_recurse();
        let mut scheduler =
            SegmentScheduler::new(std::mem::take(&mut self.incremental.pending_segments));
        let mut flist_writer = self
            .incremental
            .flist_writer_cache
            .take()
            .unwrap_or_else(|| self.build_flist_writer());
        let mut flist_ndx_codec = create_ndx_codec(self.protocol.as_u8());
        let mut segments_sent: usize = 0;
        // upstream: sender.c:242-250 - tracks remaining flist-free NDX_DONEs.
        // With INC_RECURSE, the client sends one NDX_DONE per completed flist
        // (initial + sub-lists). The sender echoes these without phase change
        // until all flists are freed, then falls through to the normal phase
        // transition. This counter tracks how many flist-free echoes remain.
        let mut flist_done_remaining: usize = 0;

        // upstream: flist.c:104,2160 - file_total tracks entries the receiver
        // knows about (initial segment + dispatched sub-lists). Used for the
        // MIN_FILECNT_LOOKAHEAD comparison in send_extra_file_list().
        // Unlike self.file_list.len() which includes undispatched segments,
        // this only counts entries already sent to the receiver.
        let mut dispatched_entry_count: usize = self
            .incremental
            .initial_segment_count
            .unwrap_or(self.file_list.len());

        // upstream: sender.c - dry-run skips data transfer; daemon may close early
        let tolerant = self.config.flags.dry_run;

        loop {
            // upstream: sender.c:227 - send extra file lists at top of loop
            if inc_recurse {
                // upstream: flist.c:2104 - file_total - file_old_total < at_least
                // remaining = entries the receiver knows about minus transferred
                let remaining = dispatched_entry_count.saturating_sub(files_transferred);
                while let Some(seg) = scheduler.next_if_needed(remaining) {
                    self.encode_and_send_segment(
                        &mut *writer,
                        seg,
                        &mut flist_writer,
                        &mut flist_ndx_codec,
                    )?;
                    segments_sent += 1;
                    flist_done_remaining += 1;
                    dispatched_entry_count += seg.count;
                }

                // upstream: flist.c:2534-2545 - send NDX_FLIST_EOF when all sub-lists
                // have been dispatched. Must happen inside the loop (not after) because
                // the receiver waits for NDX_FLIST_EOF before sending NDX_DONE.
                if !self.incremental.flist_eof_sent && scheduler.is_exhausted() {
                    self.send_flist_eof(&mut *writer, &mut flist_ndx_codec, segments_sent)?;
                }
            }

            // upstream: io.c perform_io() flushes buffered output while waiting
            // for input via select(). Our Read/Write traits are independent, so
            // we must explicitly flush before blocking on read to prevent deadlock
            // when buffered delta data hasn't reached the receiver yet.
            flush_with_count(writer)?;

            // upstream: sender.c:210-462 - read NDX request from receiver
            let ndx = match ndx_read_codec.read_ndx(&mut *reader) {
                Ok(ndx) => ndx,
                Err(e) if (phase > 0 || tolerant) && is_early_close_error(&e) => {
                    break;
                }
                Err(e) => return Err(e),
            };

            // upstream: io.c:1736-1750, sender.c:236-258 - handle control NDX values
            if ndx < 0 {
                match ndx {
                    NDX_DONE => {
                        // upstream: sender.c:242-257 - INC_RECURSE flist-free path.
                        // With INC_RECURSE, the client sends one NDX_DONE per
                        // completed sub-file-list before the actual phase transitions.
                        // Echo these without incrementing phase, matching upstream's
                        // flist_free(first_flist) loop.
                        if inc_recurse && flist_done_remaining > 0 {
                            flist_done_remaining -= 1;
                            // upstream: sender.c:244 - flist_free(first_flist)
                            // Reclaim heap data from the oldest completed segment
                            // to reduce RSS. Entries stay in place for NDX indexing
                            // but their PathBuf/extras allocations are freed.
                            self.reclaim_oldest_segment();
                            if let Err(e) = ndx_write_codec
                                .write_ndx_done(&mut *writer)
                                .and_then(|()| flush_with_count(&mut *writer))
                            {
                                if tolerant && is_early_close_error(&e) {
                                    break;
                                }
                                return Err(e);
                            }

                            // upstream: sender.c:242-250 - when flist_free() frees
                            // the last flist (first_flist becomes NULL), the sender
                            // falls through to the phase transition immediately.
                            // Without this, empty-dir pushes deadlock: the generator
                            // frees all flists except the last (cur_flist ==
                            // first_flist break), blocks waiting for receiver, which
                            // blocks waiting for our phase-transition NDX_DONE.
                            // Proactively transition when all flists are freed and
                            // no more sub-lists are pending, BUT only when the flist
                            // has no regular files. When files exist, the receiver
                            // will request them and send the normal phase-transition
                            // NDX_DONE afterward - no deadlock occurs.
                            let has_no_files =
                                !self.file_list.as_slice().iter().any(|e| e.is_file());
                            if flist_done_remaining == 0
                                && self.incremental.flist_eof_sent
                                && has_no_files
                            {
                                debug_log!(
                                    Send,
                                    1,
                                    "all flists freed with eof sent, \
                                     proactive phase transition"
                                );
                                phase += 1;
                                if phase > max_phase {
                                    break;
                                }
                                debug_log!(Send, 1, "send_files phase={}", phase);
                                if let Err(e) = ndx_write_codec
                                    .write_ndx_done(&mut *writer)
                                    .and_then(|()| flush_with_count(&mut *writer))
                                {
                                    if tolerant && is_early_close_error(&e) {
                                        break;
                                    }
                                    return Err(e);
                                }
                            }

                            continue;
                        }

                        // upstream: sender.c:252-257 - phase transition.
                        // Increment phase first, break without echo if past max_phase.
                        phase += 1;
                        if phase > max_phase {
                            break;
                        }
                        // upstream: sender.c:254-255
                        // rprintf(FINFO, "send_files phase=%d\n", phase)
                        debug_log!(Send, 1, "send_files phase={}", phase);
                        if let Err(e) = ndx_write_codec
                            .write_ndx_done(&mut *writer)
                            .and_then(|()| flush_with_count(&mut *writer))
                        {
                            if tolerant && is_early_close_error(&e) {
                                break;
                            }
                            return Err(e);
                        }
                        continue;
                    }
                    NDX_FLIST_EOF => {
                        // End of incremental file lists (upstream io.c:1738-1741)
                        debug_log!(Flist, 2, "received NDX_FLIST_EOF, file list complete");
                        continue;
                    }
                    NDX_DEL_STATS => {
                        // Deletion statistics (upstream main.c:238-247).
                        // During dry-run the connection may drop mid-read.
                        let stats = match DeleteStats::read_from(&mut *reader) {
                            Ok(s) => s,
                            Err(e) if tolerant && is_early_close_error(&e) => {
                                break;
                            }
                            Err(e) => return Err(e),
                        };
                        self.accumulate_delete_stats(&stats);
                        debug_log!(
                            Flist,
                            2,
                            "received NDX_DEL_STATS: {} deletions",
                            stats.total()
                        );
                        continue;
                    }
                    _ if ndx <= NDX_FLIST_OFFSET => {
                        // Incremental file list directory index (upstream flist.c)
                        debug_log!(Flist, 2, "received NDX_FLIST_OFFSET {}, not supported", ndx);
                        continue;
                    }
                    _ => {
                        // Unknown negative NDX - log and continue
                        debug_log!(Flist, 1, "received unknown negative NDX value {}", ndx);
                        continue;
                    }
                }
            }

            // upstream: sender.c:263-266 - preserve the original wire NDX for
            // echo-back. When INC_RECURSE is active, the receiver sends "gap
            // NDX" values (ndx_start - 1 per sub-list) that fall below
            // cur_flist->ndx_start. Upstream echoes the original NDX unchanged;
            // converting through wire_to_flat_ndx and back via flat_to_wire_ndx
            // corrupts these gap values (the subtraction wraps to usize::MAX).
            let wire_ndx = ndx;
            // upstream: rsync.c:424 - i = ndx - cur_flist->ndx_start
            let ndx = self.wire_to_flat_ndx(wire_ndx);

            // upstream: rsync.c:227 - read_ndx_and_attrs() reads iflags
            let iflags = ItemFlags::read(&mut *reader, self.protocol.as_u8())?;
            if self.protocol.supports_iflags() {
                self.timing.total_bytes_read += 2;
            }

            let (_fnamecmp_type, xname) = iflags.read_trailing(&mut *reader)?;
            if iflags.has_basis_type() {
                self.timing.total_bytes_read += 1;
            }
            if let Some(ref xname_data) = xname {
                self.timing.total_bytes_read += 4 + xname_data.len() as u64;
            }

            // upstream: sender.c:280-284 - drain the generator's xattr request
            // when preserve_xattrs && ITEM_REPORT_XATTR is set. The generator
            // always emits at least a 0 terminator (varint) under this gate, so
            // skipping it desyncs the subsequent sum_head read and aborts the
            // transfer with errors like "block length must be non-zero" or
            // "Invalid remainder length" - the failure mode reported under
            // `-X --fake-super` where xattr counts differ between sides.
            //
            // Returns the per-file xattr list with XSTATE_TODO entries set on
            // the indices the generator requested, ready for write_ndx_and_attrs
            // to echo the full values back via send_sender_xattr_response.
            let mut pending_xattr_response =
                self.read_generator_xattr_request_if_any(&mut *reader, ndx, &iflags)?;

            // upstream: sender.c:277-278
            // rprintf(FINFO, "send_files(%d, %s%s%s)\n", ndx, path,slash,fname)
            // F_PATHNAME is unset for the in-band file list we build, so the
            // path/slash prefix is empty and we emit just the relative name.
            if ndx < self.file_list.len() {
                let entry_path = self.file_list[ndx].path().display().to_string();
                debug_log!(Send, 1, "send_files({}, {})", wire_ndx, entry_path);
            }

            if !iflags.needs_transfer() {
                // upstream: sender.c:286-292 - non-transfer items still echo
                // NDX + iflags + (optional xattr response) via write_ndx_and_attrs
                // so the receiver can pair the response with its outstanding
                // request and apply xattr-only updates. Without this echo the
                // wire stream stalls when ITEM_REPORT_XATTR is the only delta.
                self.maybe_emit_itemize(writer, &iflags, ndx, itemize)?;
                self.write_ndx_iflags_and_xattr_response(
                    &mut *writer,
                    &mut ndx_write_codec,
                    wire_ndx,
                    &iflags,
                    pending_xattr_response.as_mut(),
                )?;
                continue;
            }

            // upstream: sender.c:341-344 - dry_run (!do_xfers) logs the item and
            // echoes write_ndx_and_attrs() without calling receive_sums(). The
            // echo still carries the xattr response when ITEM_REPORT_XATTR is
            // set so the receiver can pair its outstanding request.
            if self.config.flags.dry_run {
                self.validate_file_index(ndx)?;
                let file_entry = &self.file_list[ndx];
                self.write_ndx_iflags_and_xattr_response(
                    &mut *writer,
                    &mut ndx_write_codec,
                    wire_ndx,
                    &iflags,
                    pending_xattr_response.as_mut(),
                )?;
                // upstream: sender.c:395 - log_item(FCLIENT, file, iflags, NULL)
                if let Some(cb) = itemize {
                    let name = file_entry.path().to_string_lossy();
                    cb.on_itemize(&format!("{name}\n"));
                }
                files_transferred += 1;
                flush_with_count(writer)?;
                continue;
            }

            // upstream: sender.c:120 - receive_sums()
            let sum_head = SumHead::read(&mut *reader)?;
            self.timing.total_bytes_read += 16;

            self.validate_file_index(ndx)?;

            let file_entry = &self.file_list[ndx];
            debug_assert_eq!(
                self.file_list.len(),
                self.full_paths.len(),
                "file_list and full_paths must be kept in sync"
            );
            let source_path = &self.full_paths[ndx];
            let source_path_display = source_path.display().to_string();

            let sig_blocks = read_signature_blocks(&mut *reader, &sum_head)?;
            let bytes_per_block = 4 + sum_head.s2length as u64;
            self.timing.total_bytes_read += sum_head.count as u64 * bytes_per_block;

            let block_length = sum_head.blength;
            let strong_sum_length = sum_head.s2length as u8;
            let has_basis = !sum_head.is_empty();

            if !file_entry.is_file() {
                continue;
            }

            let file_size = file_entry.size();

            if has_basis {
                let source: Box<dyn Read> = match self.open_source_reader(source_path, file_size) {
                    Ok(r) => r,
                    Err(e) => {
                        self.record_open_failure(&mut *writer, wire_ndx, &e, &source_path_display)?;
                        continue;
                    }
                };
                let config = DeltaGeneratorConfig {
                    block_length,
                    sig_blocks,
                    strong_sum_length,
                    protocol: self.protocol,
                    negotiated_algorithms: self.negotiated_algorithms.as_ref(),
                    compat_flags: self.compat_flags.as_ref(),
                    checksum_seed: self.checksum_seed,
                };
                let delta_script = generate_delta_from_signature(source, config)?;

                self.write_ndx_and_attrs(
                    &mut *writer,
                    &mut ndx_write_codec,
                    wire_ndx,
                    &iflags,
                    &sum_head,
                    pending_xattr_response.as_mut(),
                )?;

                let checksum_algorithm = self.get_checksum_algorithm();
                let use_noatime = self.config.write.open_noatime;
                let delta_total_bytes = delta_script.total_bytes();
                let wire_ops = script_to_wire_delta(delta_script, block_length);
                let use_compression = self.file_compression(source_path).is_some();
                let is_zlib = matches!(
                    negotiated_compression,
                    Some(protocol::CompressionAlgorithm::Zlib)
                );
                // upstream: match.c:matched() - compute file checksum inline
                // during the wire-write pass, eliminating the separate
                // compute_file_checksum() call that re-opened and re-read the
                // source file.
                let result = write_delta_with_inline_checksum(
                    &mut *writer,
                    &wire_ops,
                    if use_compression {
                        token_encoder.as_mut()
                    } else {
                        None
                    },
                    is_zlib,
                    source_path,
                    use_noatime,
                    checksum_algorithm,
                )?;
                writer.write_all(&result.checksum_buf[..result.checksum_len])?;
                bytes_sent += delta_total_bytes;
            } else {
                // upstream: sender.c:354-369 - whole-file path; MSG_NO_SEND on open failure
                // Use unbuffered reader: stream_whole_file_transfer manages its
                // own 256 KB staging buffer with read_exact, so a BufReader would
                // only add an extra memcpy per byte through its internal buffer.
                let source: Box<dyn Read> = match self
                    .open_source_unbuffered(source_path, file_size)
                {
                    Ok(r) => r,
                    Err(e) => {
                        self.record_open_failure(&mut *writer, wire_ndx, &e, &source_path_display)?;
                        continue;
                    }
                };

                self.write_ndx_and_attrs(
                    &mut *writer,
                    &mut ndx_write_codec,
                    wire_ndx,
                    &iflags,
                    &sum_head,
                    pending_xattr_response.as_mut(),
                )?;

                let checksum_algorithm = self.get_checksum_algorithm();
                let use_compression = self.file_compression(source_path).is_some();
                let result = stream_whole_file_transfer(
                    &mut *writer,
                    source,
                    file_size,
                    checksum_algorithm,
                    if use_compression {
                        token_encoder.as_mut()
                    } else {
                        None
                    },
                    &mut stream_buf,
                )?;
                writer.write_all(&result.checksum_buf[..result.checksum_len])?;
                bytes_sent += result.total_bytes;
            }
            files_transferred += 1;

            // upstream: sender.c:445-446
            // rprintf(FINFO, "sender finished %s%s%s\n", path,slash,fname)
            debug_log!(Send, 1, "sender finished {}", file_entry.path().display());

            // upstream: sender.c:430 - log_item(log_code, file, iflags, NULL)
            self.maybe_emit_itemize(writer, &iflags, ndx, itemize)?;

            if let Some(cb) = progress.as_mut() {
                // upstream: progress.c:80 - "to" once the final sub-list has been
                // sent, "ir" while more sub-lists are still pending.
                let flist_eof = !inc_recurse || self.incremental.flist_eof_sent;
                let event = super::super::super::TransferProgressEvent {
                    path: file_entry.path(),
                    file_bytes: bytes_sent,
                    total_file_bytes: Some(file_entry.size()),
                    files_done: files_transferred,
                    total_files: self.file_list.len(),
                    flist_eof,
                };
                cb.on_file_transferred(&event);
            }

            // upstream: sender.c:261 - send extra file lists at bottom of loop
            if inc_recurse {
                let remaining = dispatched_entry_count.saturating_sub(files_transferred);
                while let Some(seg) = scheduler.next_if_needed(remaining) {
                    self.encode_and_send_segment(
                        &mut *writer,
                        seg,
                        &mut flist_writer,
                        &mut flist_ndx_codec,
                    )?;
                    segments_sent += 1;
                    flist_done_remaining += 1;
                    dispatched_entry_count += seg.count;
                }
            }

            // Check deadline at file boundary after sending each file.
            // Upstream rsync (io.c:825) hard-exits via exit_cleanup(RERR_TIMEOUT).
            // We return an error to match: the sender cannot gracefully stop because
            // the receiver has already sent pending file requests that expect responses.
            // The error propagates up, causing the connection to close and the remote
            // side to detect the closed pipe and clean up.
            if let Some(ref dl) = deadline {
                if dl.is_reached() {
                    return Err(TransferDeadline::as_io_error());
                }
            }
        }

        // Flush any remaining INC_RECURSE segments and send NDX_FLIST_EOF.
        // No need to track flist_done_remaining here: the EOF flush is the
        // final write loop in this function, so the counter is never read
        // again. Mid-transfer increments at line ~435 are still needed for
        // the in-loop accounting at line ~158.
        if inc_recurse && !self.incremental.flist_eof_sent {
            for seg in scheduler.remaining() {
                self.encode_and_send_segment(
                    &mut *writer,
                    seg,
                    &mut flist_writer,
                    &mut flist_ndx_codec,
                )?;
                segments_sent += 1;
            }
            self.send_flist_eof(&mut *writer, &mut flist_ndx_codec, segments_sent)?;
        }

        // Cache flist_writer back for potential reuse (e.g., phase 2).
        self.incremental.flist_writer_cache = Some(flist_writer);

        // upstream: sender.c:457-458
        // rprintf(FINFO, "send files finished\n")
        debug_log!(Send, 1, "send files finished");

        // upstream: sender.c:454-462 - after the transfer loop exits, the sender
        // sends io_error (if changed) and a final NDX_DONE. This NDX_DONE is the
        // "goodbye" that tells the client's generator to proceed with its own
        // goodbye handshake. Without it, the client hangs waiting for this marker.
        if let Err(e) = ndx_write_codec
            .write_ndx_done(&mut *writer)
            .and_then(|()| flush_with_count(&mut *writer))
        {
            if !(tolerant && is_early_close_error(&e)) {
                return Err(e);
            }
        }

        Ok(TransferLoopResult {
            files_transferred,
            bytes_sent,
            ndx_read_codec,
            ndx_write_codec,
        })
    }
}
