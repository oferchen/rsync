//! Main transfer loop and goodbye handshake for the generator role.
//!
//! Contains `run_transfer_loop` (NDX-driven file sending with delta/whole-file
//! paths), `handle_goodbye` (protocol 31+ extended goodbye with `NDX_DEL_STATS`),
//! and the top-level `run` orchestrator that ties together file list building,
//! transmission, transfer, and finalization.
//!
//! # Upstream Reference
//!
//! - `sender.c:send_files()` - Main transfer loop (lines 210-462)
//! - `main.c:875-906` - `read_final_goodbye()` with del_stats handling

use std::io::{self, Read, Write};
use std::path::PathBuf;

use logging::{PhaseTimer, debug_log};
use protocol::TransferStats;
use protocol::codec::{
    MonotonicNdxWriter, NDX_DEL_STATS, NDX_DONE, NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec,
    create_ndx_codec,
};
use protocol::stats::DeleteStats;

use super::delta::{
    compute_file_checksum, create_token_encoder, script_to_wire_delta, stream_whole_file_transfer,
    write_delta_with_compression,
};
use super::item_flags::ItemFlags;
use super::protocol_io::calculate_duration_ms;
use super::{
    GeneratorContext, GeneratorStats, SegmentScheduler, TransferLoopResult, is_early_close_error,
};
use crate::delta_config::DeltaGeneratorConfig;
use crate::receiver::SumHead;
use crate::role_trailer::error_location;

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
    fn run_transfer_loop<R: Read, W: Write>(
        &mut self,
        reader: &mut R,
        writer: &mut super::super::writer::ServerWriter<W>,
        progress: &mut Option<&mut dyn super::super::TransferProgressCallback>,
        itemize: &mut Option<&mut dyn super::super::ItemizeCallback>,
    ) -> io::Result<TransferLoopResult> {
        use super::super::shared::TransferDeadline;
        use super::delta::generate_delta_from_signature;
        use super::protocol_io::read_signature_blocks;

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
        let mut token_encoder = negotiated_compression
            .map(create_token_encoder)
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

        // upstream: sender.c - dry-run skips data transfer; daemon may close early
        let tolerant = self.config.flags.dry_run;

        loop {
            // upstream: sender.c:227 - send extra file lists at top of loop
            if inc_recurse {
                let remaining = self.file_list.len().saturating_sub(files_transferred);
                while let Some(seg) = scheduler.next_if_needed(remaining) {
                    self.encode_and_send_segment(
                        &mut *writer,
                        seg,
                        &mut flist_writer,
                        &mut flist_ndx_codec,
                    )?;
                    segments_sent += 1;
                    flist_done_remaining += 1;
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
            writer.flush()?;

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
                            if let Err(e) = ndx_write_codec
                                .write_ndx_done(&mut *writer)
                                .and_then(|()| writer.flush())
                            {
                                if tolerant && is_early_close_error(&e) {
                                    break;
                                }
                                return Err(e);
                            }
                            continue;
                        }

                        // upstream: sender.c:252-257 - phase transition.
                        // Increment phase first, break without echo if past max_phase.
                        phase += 1;
                        if phase > max_phase {
                            break;
                        }
                        if let Err(e) = ndx_write_codec
                            .write_ndx_done(&mut *writer)
                            .and_then(|()| writer.flush())
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

            // upstream: rsync.c:424 - i = ndx - cur_flist->ndx_start
            let ndx = self.wire_to_flat_ndx(ndx);

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

            if !iflags.needs_transfer() {
                // upstream: sender.c:287 - maybe_log_item() for non-transfer items
                self.maybe_emit_itemize(writer, &iflags, ndx, itemize)?;
                continue;
            }

            // upstream: sender.c:394-399 - dry_run (!do_xfers) logs the item and
            // echoes write_ndx_and_attrs() without calling receive_sums().
            if self.config.flags.dry_run {
                self.validate_file_index(ndx)?;
                let file_entry = &self.file_list[ndx];
                let ndx_i32 = self.flat_to_wire_ndx(ndx);
                ndx_write_codec.write_ndx(&mut *writer, ndx_i32)?;
                if self.protocol.supports_iflags() {
                    writer.write_all(&iflags.significant_wire_bits().to_le_bytes())?;
                }
                // upstream: sender.c:395 - log_item(FCLIENT, file, iflags, NULL)
                if let Some(cb) = itemize {
                    let name = file_entry.path().to_string_lossy();
                    cb.on_itemize(&format!("{name}\n"));
                }
                files_transferred += 1;
                writer.flush()?;
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
            // upstream: sender.c:389 - write_ndx(f_out, ndx)
            let ndx_i32 = self.flat_to_wire_ndx(ndx);

            if has_basis {
                let source: Box<dyn Read> = match self.open_source_reader(source_path, file_size) {
                    Ok(r) => r,
                    Err(e) => {
                        self.record_open_failure(&mut *writer, ndx_i32, &e, &source_path_display)?;
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
                    ndx_i32,
                    &iflags,
                    &sum_head,
                )?;

                let checksum_algorithm = self.get_checksum_algorithm();
                let (checksum_buf, checksum_len) = compute_file_checksum(
                    &delta_script,
                    checksum_algorithm,
                    self.checksum_seed,
                    self.compat_flags.as_ref(),
                    source_path,
                    block_length,
                )?;
                let delta_total_bytes = delta_script.total_bytes();
                let wire_ops = script_to_wire_delta(delta_script);
                let use_compression = self.file_compression(source_path).is_some();
                let is_zlib = matches!(
                    negotiated_compression,
                    Some(protocol::CompressionAlgorithm::Zlib)
                );
                write_delta_with_compression(
                    &mut *writer,
                    &wire_ops,
                    if use_compression {
                        token_encoder.as_mut()
                    } else {
                        None
                    },
                    is_zlib,
                    source_path,
                )?;
                writer.write_all(&checksum_buf[..checksum_len])?;
                bytes_sent += delta_total_bytes;
            } else {
                // upstream: sender.c:354-369 - whole-file path; MSG_NO_SEND on open failure
                let source: Box<dyn Read> = match self.open_source_reader(source_path, file_size) {
                    Ok(r) => r,
                    Err(e) => {
                        self.record_open_failure(&mut *writer, ndx_i32, &e, &source_path_display)?;
                        continue;
                    }
                };

                self.write_ndx_and_attrs(
                    &mut *writer,
                    &mut ndx_write_codec,
                    ndx_i32,
                    &iflags,
                    &sum_head,
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

            // upstream: sender.c:430 - log_item(log_code, file, iflags, NULL)
            self.maybe_emit_itemize(writer, &iflags, ndx, itemize)?;

            if let Some(cb) = progress.as_mut() {
                let event = super::super::TransferProgressEvent {
                    path: file_entry.path(),
                    file_bytes: bytes_sent,
                    total_file_bytes: Some(file_entry.size()),
                    files_done: files_transferred,
                    total_files: self.file_list.len(),
                };
                cb.on_file_transferred(&event);
            }

            // upstream: sender.c:261 - send extra file lists at bottom of loop
            if inc_recurse {
                let remaining = self.file_list.len().saturating_sub(files_transferred);
                while let Some(seg) = scheduler.next_if_needed(remaining) {
                    self.encode_and_send_segment(
                        &mut *writer,
                        seg,
                        &mut flist_writer,
                        &mut flist_ndx_codec,
                    )?;
                    segments_sent += 1;
                    flist_done_remaining += 1;
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
        if inc_recurse && !self.incremental.flist_eof_sent {
            for seg in scheduler.remaining() {
                self.encode_and_send_segment(
                    &mut *writer,
                    seg,
                    &mut flist_writer,
                    &mut flist_ndx_codec,
                )?;
                segments_sent += 1;
                flist_done_remaining += 1;
            }
            self.send_flist_eof(&mut *writer, &mut flist_ndx_codec, segments_sent)?;
        }

        // Cache flist_writer back for potential reuse (e.g., phase 2).
        self.incremental.flist_writer_cache = Some(flist_writer);

        // upstream: sender.c:454-462 - after the transfer loop exits, the sender
        // sends io_error (if changed) and a final NDX_DONE. This NDX_DONE is the
        // "goodbye" that tells the client's generator to proceed with its own
        // goodbye handshake. Without it, the client hangs waiting for this marker.
        if let Err(e) = ndx_write_codec
            .write_ndx_done(&mut *writer)
            .and_then(|()| writer.flush())
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

    /// Handles the goodbye handshake at end of transfer.
    ///
    /// For protocol < 29, upstream uses `read_int()` (raw 4-byte LE) to read the
    /// receiver's goodbye NDX_DONE. For protocol >= 29, it uses `read_ndx_and_attrs()`
    /// which for NDX_DONE returns immediately without reading iflags. Both produce
    /// the same wire format, so the legacy NDX codec handles both correctly.
    ///
    /// Protocol 31+ introduces NDX_DEL_STATS during the goodbye phase. The receiver
    /// may send deletion statistics before the final NDX_DONE. This mirrors upstream's
    /// `read_ndx_and_attrs()` which loops over NDX_DEL_STATS, reading 5 varints of
    /// deletion counts before continuing to expect NDX_DONE.
    ///
    /// Deletion statistics are only sent when `--stats` is active (INFO_GTE(STATS, 2))
    /// and follow upstream's early/late timing:
    /// - **Early** (delete_during or delete_before): sent when `do_stats && delete_mode`.
    /// - **Late** (delete_delay or delete_after): sent when `do_stats`.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:875-906` - `read_final_goodbye()`
    /// - `main.c:883` - protocol < 29 uses `read_int(f_in)`
    /// - `main.c:885-886` - protocol >= 29 uses `read_ndx_and_attrs()`
    /// - `rsync.c:337-342` - NDX_DEL_STATS handling in `read_ndx_and_attrs()`
    /// - `main.c:225-238` - `write_del_stats()` format
    /// - `generator.c:2376-2381` - early del_stats path
    /// - `generator.c:2420-2425` - late del_stats path
    pub(super) fn handle_goodbye<R: Read, W: Write>(
        &mut self,
        reader: &mut R,
        writer: &mut W,
        ndx_read_codec: &mut protocol::codec::NdxCodecEnum,
        ndx_write_codec: &mut MonotonicNdxWriter,
    ) -> io::Result<()> {
        if !self.protocol.supports_goodbye_exchange() {
            return Ok(());
        }

        // Read first NDX_DONE from receiver, skipping any NDX_DEL_STATS.
        // upstream: main.c:886 - read_ndx_and_attrs() handles NDX_DEL_STATS internally.
        // Connection may close early in dry-run or when the remote daemon exits before
        // completing the goodbye exchange - treat this as acceptable.
        let ndx = match self.read_ndx_skipping_del_stats(reader, ndx_read_codec) {
            Ok(ndx) => ndx,
            Err(e) if is_early_close_error(&e) => {
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        if ndx != NDX_DONE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "expected goodbye NDX_DONE (-1) from receiver, got {ndx} {}{}",
                    error_location!(),
                    crate::role_trailer::sender()
                ),
            ));
        }

        // For protocol 31+: conditionally send del_stats, echo NDX_DONE, read final NDX_DONE.
        //
        // Upstream gates del_stats sending on INFO_GTE(STATS, 2) (i.e. --stats was passed)
        // and splits it into early vs late paths depending on deletion timing:
        // - Early (generator.c:2376-2381): !(delete_during==2 || delete_after) =>
        //   send del_stats only when (do_stats && (delete_mode || force_delete))
        // - Late (generator.c:2420-2425): (delete_during==2 || delete_after) =>
        //   send del_stats when do_stats
        if self.protocol.supports_extended_goodbye() {
            // Writes during goodbye may fail when the daemon has already closed
            // the connection (common in dry-run mode).
            let write_result = (|| -> io::Result<()> {
                if self.should_send_del_stats() {
                    ndx_write_codec.write_ndx(writer, NDX_DEL_STATS)?;
                    self.delete_stats.write_to(writer)?;
                    debug_log!(
                        Flist,
                        2,
                        "sent NDX_DEL_STATS during goodbye: {} deletions",
                        self.delete_stats.total()
                    );
                }
                ndx_write_codec.write_ndx_done(writer)?;
                writer.flush()
            })();

            if let Err(e) = write_result {
                if is_early_close_error(&e) {
                    return Ok(());
                }
                return Err(e);
            }

            // Read final NDX_DONE - may fail if daemon kills receiver child early
            match self.read_ndx_skipping_del_stats(reader, ndx_read_codec) {
                Ok(final_ndx) => {
                    if final_ndx != NDX_DONE {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "expected final goodbye NDX_DONE (-1) from receiver, got {final_ndx} {}{}",
                                error_location!(),
                                crate::role_trailer::sender()
                            ),
                        ));
                    }
                }
                Err(e) if is_early_close_error(&e) => {
                    // Connection closed during final goodbye - acceptable
                }
                Err(e) => {
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    /// Determines whether del_stats should be sent during the goodbye phase.
    ///
    /// Mirrors upstream's conditional logic for `write_del_stats()` in the
    /// generator goodbye sequence. The conditions differ for early vs late
    /// deletion timing:
    ///
    /// - **Early** (`!late_delete`): `do_stats && flags.delete`
    ///   (upstream: generator.c:2377 - `INFO_GTE(STATS, 2) && (delete_mode || force_delete)`)
    /// - **Late** (`late_delete`): `do_stats`
    ///   (upstream: generator.c:2422 - `INFO_GTE(STATS, 2)`)
    pub(super) fn should_send_del_stats(&self) -> bool {
        if !self.config.do_stats {
            return false;
        }
        if self.config.deletion.late_delete {
            // upstream: generator.c:2422 - INFO_GTE(STATS, 2) (already checked above)
            true
        } else {
            // upstream: generator.c:2377 - INFO_GTE(STATS, 2) && (delete_mode || force_delete)
            self.config.flags.delete
        }
    }

    /// Reads the next NDX value, consuming any NDX_DEL_STATS messages.
    ///
    /// Upstream `read_ndx_and_attrs()` (rsync.c:337-342) loops over NDX_DEL_STATS,
    /// calling `read_del_stats()` which reads 5 varints and accumulates counts.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync.c:337-342` - NDX_DEL_STATS loop in `read_ndx_and_attrs()`
    /// - `main.c:238-247` - `read_del_stats()` accumulates into global counters
    fn read_ndx_skipping_del_stats<R: Read>(
        &mut self,
        reader: &mut R,
        ndx_read_codec: &mut protocol::codec::NdxCodecEnum,
    ) -> io::Result<i32> {
        loop {
            let ndx = ndx_read_codec.read_ndx(reader)?;
            if ndx == NDX_DEL_STATS {
                let stats = DeleteStats::read_from(reader)?;
                self.accumulate_delete_stats(&stats);
                debug_log!(
                    Flist,
                    2,
                    "consumed NDX_DEL_STATS during goodbye: {} deletions",
                    stats.total()
                );
                continue;
            }
            return Ok(ndx);
        }
    }

    /// Accumulates deletion statistics from an NDX_DEL_STATS message.
    /// (upstream: main.c:238-247 - `read_del_stats()` adds to global counters)
    fn accumulate_delete_stats(&mut self, stats: &DeleteStats) {
        self.delete_stats.files = self.delete_stats.files.saturating_add(stats.files);
        self.delete_stats.dirs = self.delete_stats.dirs.saturating_add(stats.dirs);
        self.delete_stats.symlinks = self.delete_stats.symlinks.saturating_add(stats.symlinks);
        self.delete_stats.devices = self.delete_stats.devices.saturating_add(stats.devices);
        self.delete_stats.specials = self.delete_stats.specials.saturating_add(stats.specials);
    }

    /// Sends transfer statistics to the client after the transfer loop completes.
    ///
    /// Only called in server mode (daemon sender). Writes total_read,
    /// total_written, total_size as varlong30 values, plus flist_buildtime
    /// and flist_xfertime for protocol >= 29.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:347-357` - `handle_stats()` server-sender write path
    /// - `main.c:960-962` - `do_server_sender()` calls `handle_stats(f_out)`
    fn send_stats<W: Write>(
        &self,
        writer: &mut W,
        transfer_result: &TransferLoopResult,
        flist_buildtime_ms: u64,
        flist_xfertime_ms: u64,
    ) -> io::Result<()> {
        // upstream: stats.total_size is the sum of all file sizes in the transfer
        let total_size: u64 = self.file_list.iter().map(|e| e.size()).sum();

        let stats = TransferStats::with_bytes(
            self.timing.total_bytes_read,
            transfer_result.bytes_sent,
            total_size,
        )
        .with_flist_times(flist_buildtime_ms, flist_xfertime_ms);

        stats.write_to(writer, self.protocol)?;
        writer.flush()?;
        Ok(())
    }

    /// Runs the generator role to completion.
    ///
    /// Orchestrates the full send operation: build file list, send it, process
    /// NDX requests (receive signatures, generate deltas, send data), and
    /// finalize with the goodbye handshake.
    ///
    /// # Upstream Reference
    ///
    /// - `sender.c:send_files()` - Main transfer loop
    /// - `flist.c:2192` - `send_file_list()` builds and sends file list
    /// - `main.c:875-906` - `read_final_goodbye()` protocol finalization
    pub fn run<R: Read, W: Write>(
        &mut self,
        mut reader: super::super::reader::ServerReader<R>,
        writer: &mut super::super::writer::ServerWriter<W>,
        paths: &[PathBuf],
        mut progress: Option<&mut dyn super::super::TransferProgressCallback>,
        mut itemize: Option<&mut dyn super::super::ItemizeCallback>,
    ) -> io::Result<GeneratorStats> {
        if self.should_activate_input_multiplex() {
            reader = reader.activate_multiplex().map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!(
                        "failed to activate INPUT multiplex: {e} {}{}",
                        error_location!(),
                        crate::role_trailer::sender()
                    ),
                )
            })?;
        }

        // upstream: main.c:1248-1258 - flush pending multiplex output before
        // blocking on recv_filter_list(). Upstream's perform_io() flushes the
        // output buffer while waiting for input via select(), but our separate
        // read/write streams cannot do that. Without this flush, any buffered
        // data (e.g. MSG_IO_TIMEOUT) stays unsent while we block reading the
        // client's filter list, causing a protocol ordering deadlock in daemon
        // pull mode where the client waits for server output before proceeding.
        if !self.config.connection.client_mode {
            writer.flush()?;
        }

        // upstream: main.c:1258 - recv_filter_list() in server mode
        self.receive_filter_list_if_server(&mut reader)?;

        // upstream: flist.c:2240-2264 - resolve --files-from paths if configured
        let files_from_paths = self.resolve_files_from_paths(paths, &mut reader)?;

        let reader = &mut reader;

        // upstream: flist.c:2192 - send_file_list()
        let file_count = {
            let _t = PhaseTimer::new("file-list-build-send");
            if files_from_paths.is_empty() {
                self.build_file_list(paths)?;
            } else {
                // upstream: flist.c:2240-2244 - argv[0] is the base for --files-from
                let base_dir = paths.first().cloned().unwrap_or_else(|| PathBuf::from("."));
                self.build_file_list_with_base(&base_dir, &files_from_paths)?;
            }
            self.partition_file_list_for_inc_recurse();
            self.send_file_list(writer)?
        };

        self.send_id_lists(writer)?;
        self.send_io_error_flag(writer)?;

        // INC_RECURSE sub-lists are sent lazily inside the loop via
        // SegmentScheduler, matching upstream sender.c:227,261 cadence.
        let transfer_result = {
            let _t = PhaseTimer::new("generator-transfer-loop");
            self.run_transfer_loop(reader, writer, &mut progress, &mut itemize)?
        };

        // upstream: main.c:960-962 - do_server_sender() calls io_flush then handle_stats
        // before read_final_goodbye. Server-sender writes transfer stats; client-sender
        // handle_stats(-1) is a no-op (main.c:339-345).
        if !self.config.connection.client_mode {
            let flist_buildtime =
                calculate_duration_ms(self.timing.flist_build_start, self.timing.flist_build_end);
            let flist_xfertime =
                calculate_duration_ms(self.timing.flist_xfer_start, self.timing.flist_xfer_end);
            self.send_stats(writer, &transfer_result, flist_buildtime, flist_xfertime)?;
        }

        let mut ndx_read_codec = transfer_result.ndx_read_codec;
        let mut ndx_write_codec = transfer_result.ndx_write_codec;
        self.handle_goodbye(reader, writer, &mut ndx_read_codec, &mut ndx_write_codec)?;

        // Calculate timing stats for return value
        let flist_buildtime =
            calculate_duration_ms(self.timing.flist_build_start, self.timing.flist_build_end);
        let flist_xfertime =
            calculate_duration_ms(self.timing.flist_xfer_start, self.timing.flist_xfer_end);

        Ok(GeneratorStats {
            files_listed: file_count,
            files_transferred: transfer_result.files_transferred,
            bytes_sent: transfer_result.bytes_sent,
            bytes_read: self.timing.total_bytes_read,
            flist_buildtime_ms: flist_buildtime,
            flist_xfertime_ms: flist_xfertime,
            delete_stats: self.delete_stats,
            io_error: self.io_error,
        })
    }
}
