//! Main NDX-driven file transfer loop for the generator role.
//!
//! Contains `run_transfer_loop` which processes per-file NDX requests from the
//! receiver, generates deltas, and streams data over the wire across phases.
//!
//! # Upstream Reference
//!
//! - `sender.c:send_files()` - Main transfer loop (lines 210-462)

use std::io::{self, Read, Write};
use std::path::Path;

use logging::{debug_log, info_log};
use protocol::codec::{
    MonotonicNdxWriter, NDX_DEL_STATS, NDX_DONE, NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec,
    create_ndx_codec,
};
use protocol::stats::DeleteStats;

use super::super::delta::{
    create_token_encoder, script_to_wire_delta, stream_append_transfer, stream_whole_file_transfer,
    write_delta_with_inline_checksum,
};
use super::super::item_flags::ItemFlags;
use super::super::protocol_io::NdxAttrs;
use super::super::{
    GeneratorContext, SegmentScheduler, TransferLoopResult, flush_with_count, is_early_close_error,
};
use crate::delta_config::DeltaGeneratorConfig;
use crate::receiver::SumHead;

/// Minimum source size before the opt-in parallel delta scan is considered.
///
/// Below this a single core already scans the file faster than the rayon
/// task-spawn and result-concat overhead would allow, so the gate keeps the
/// sequential streaming path. Matches the "large single file" motivation
/// (e.g. a 50 GB file with a large block size pinning one core).
const PARALLEL_DELTA_MIN_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Minimum bytes per parallel range, mirroring the matching crate's internal
/// `MIN_PARALLEL_CHUNK_BYTES` floor. The effective floor is the larger of this
/// and 64 basis blocks; a source must hold at least two such ranges to split.
const PARALLEL_DELTA_MIN_CHUNK_BYTES: u64 = 1024 * 1024;

/// Upper bound on parallel ranges, matching
/// `rayon::current_num_threads().min(8)` from the design.
const PARALLEL_DELTA_MAX_CHUNKS: usize = 8;

/// Decides whether the opt-in parallel delta scan should engage for a file.
///
/// Requires more than one worker, a source at least
/// [`PARALLEL_DELTA_MIN_FILE_BYTES`] large, and room for at least two ranges of
/// the effective minimum chunk size (the larger of
/// [`PARALLEL_DELTA_MIN_CHUNK_BYTES`] and 64 basis blocks). The duplicate-free
/// eligibility check is applied later, once the signature index is built, so
/// this gate is purely about size and available parallelism.
fn should_parallel_delta(file_size: u64, block_length: u32, cores: usize) -> bool {
    if cores <= 1 || file_size < PARALLEL_DELTA_MIN_FILE_BYTES {
        return false;
    }
    let effective_min_chunk = PARALLEL_DELTA_MIN_CHUNK_BYTES
        .max(u64::from(block_length).saturating_mul(64))
        .max(1);
    file_size / effective_min_chunk >= 2
}

/// Opens the source file (honouring `--open-noatime`) and memory-maps it.
///
/// Returns an error when the file cannot be opened or mapped (NFS, FUSE,
/// procfs, or a zero-length file on some platforms); the caller treats that as
/// a signal to fall back to the streaming sequential reader, so wire output is
/// unaffected.
fn open_source_mmap(path: &std::path::Path, use_noatime: bool) -> io::Result<fast_io::MmapReader> {
    let file = super::super::open_source::open_source_with_noatime(path, use_noatime)?;
    fast_io::MmapReader::from_file(file)
}

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
        use super::super::delta::{
            generate_delta_from_signature, generate_delta_from_signature_chunked,
        };
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
        // upstream: match.c stats.matched_data / stats.literal_data accumulated
        // per token as the sender emits the delta stream.
        let mut matched_data = 0u64;
        let mut literal_data = 0u64;
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
        // INC_RECURSE sub-list NDX writes (NDX_FLIST_OFFSET headers,
        // NDX_FLIST_EOF) MUST share the same wire diff-state as the
        // file-transfer / goodbye writes. Upstream io.c::write_ndx keeps a
        // single connection-wide prev_positive/prev_negative; a separate codec
        // instance for sub-lists would diff-encode negative offsets against an
        // independent prev_negative, desyncing the receiver's unified read
        // state. Route sub-list writes through ndx_write_codec.inner_mut().
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
                        ndx_write_codec.inner_mut(),
                    )?;
                    segments_sent += 1;
                    flist_done_remaining += 1;
                    dispatched_entry_count += seg.count;
                }

                // upstream: flist.c:2534-2545 - send NDX_FLIST_EOF when all sub-lists
                // have been dispatched. Must happen inside the loop (not after) because
                // the receiver waits for NDX_FLIST_EOF before sending NDX_DONE.
                if !self.incremental.flist_eof_sent && scheduler.is_exhausted() {
                    self.send_flist_eof(&mut *writer, ndx_write_codec.inner_mut(), segments_sent)?;
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
                            // upstream: sender.c:243-244 -
                            // file_old_total -= first_flist->used; flist_free(first_flist).
                            // Reclaim heap data from the oldest completed segment to
                            // reduce RSS. Entries stay in place for NDX indexing but
                            // their PathBuf/extras allocations are freed.
                            self.reclaim_oldest_segment();

                            // upstream: sender.c:249-253 - after freeing the oldest
                            // flist, `if (first_flist)` is still true (another flist
                            // remains in the list), so it writes NDX_DONE and continues
                            // WITHOUT advancing phase. Reaching this branch at all means
                            // `flist_done_remaining > 0`, i.e. at least one more flist is
                            // still pending, so the echo is unconditional - exactly
                            // upstream's `first_flist != NULL` test. The receiver sends
                            // one NDX_DONE per flist completion (initial + each sub); the
                            // FINAL flist's NDX_DONE arrives when `flist_done_remaining`
                            // has already reached 0, skips this branch, and falls through
                            // to the phase transition below (upstream's last
                            // `flist_free` leaving `first_flist == NULL`).
                            //
                            // The previous `|| !flist_eof_sent` guard suppressed this
                            // echo once NDX_FLIST_EOF had been sent, folding the
                            // initial-flist completion into the phase path. That left
                            // the daemon-sender one flist-free echo short of upstream on
                            // any multi-flist (subdirectory) pull: the lock-step phase
                            // counter ran one step ahead of the receiver, the goodbye
                            // desynced, and the receiver reported "connection
                            // unexpectedly closed (io.c 232)".
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
            // upstream: rsync.c:424 - i = ndx - cur_flist->ndx_start.
            // resolve_itemize_ndx also handles the INC_RECURSE directory gap
            // NDX (`ndx_start - 1`), mapping it to the parent directory entry so
            // a dir itemize prints `.d.. sub/` rather than a file row for the
            // trailing child of the previous segment (sender.c:267-272).
            let ndx = self.resolve_itemize_ndx(wire_ndx);

            // upstream: rsync.c:227 - read_ndx_and_attrs() reads iflags
            let iflags = ItemFlags::read(&mut *reader, self.protocol.as_u8())?;
            if self.protocol.supports_iflags() {
                self.timing.total_bytes_read += 2;
            }

            let (fnamecmp_type, xname, trailing_bytes) = iflags.read_trailing(&mut *reader)?;
            // upstream: rsync.c:403-418 - the basis-type byte plus the xname
            // vstring (1- or 2-byte prefix + payload). read_trailing reports the
            // exact wire bytes consumed so this count never drifts from the wire.
            self.timing.total_bytes_read += trailing_bytes;

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
                    &NdxAttrs {
                        ndx: wire_ndx,
                        iflags: &iflags,
                        fnamecmp_type,
                        xname: xname.as_deref(),
                    },
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
                    &NdxAttrs {
                        ndx: wire_ndx,
                        iflags: &iflags,
                        fnamecmp_type,
                        xname: xname.as_deref(),
                    },
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
                self.source_bases.len(),
                "file_list and source_bases must be kept in sync"
            );
            let source_path = self.reconstruct_source_path(ndx);
            let source_path_display = source_path.display().to_string();

            // upstream: sender.c:89-95 receive_sums() - in append mode the
            // receiver's generator writes only the sum_head, not the block
            // checksums (generator.c:786 `if (append_mode > 0 && f_copy < 0)
            // return 0`). Reading blocks here would block forever, so derive
            // flength from the header and take the append literal path.
            let is_append = self.config.flags.append;
            let sig_blocks = if is_append {
                Vec::new()
            } else {
                let blocks = read_signature_blocks(&mut *reader, &sum_head)?;
                let bytes_per_block = 4 + sum_head.s2length as u64;
                self.timing.total_bytes_read += sum_head.count as u64 * bytes_per_block;
                blocks
            };

            let block_length = sum_head.blength;
            let strong_sum_length = sum_head.s2length as u8;
            let has_basis = !sum_head.is_empty();

            if !file_entry.is_file() {
                continue;
            }

            let file_size = file_entry.size();

            if is_append && has_basis {
                // upstream: match.c:371-390 - append mode streams only the tail
                // past the existing prefix; the sum_head's count/blength encode
                // that flength. No block matching, no signature blocks.
                let (source, _src_fd): (Box<dyn Read>, Option<_>) = match self
                    .open_source_unbuffered(&source_path, file_size)
                {
                    Ok(pair) => pair,
                    Err(e) => {
                        self.record_open_failure(&mut *writer, wire_ndx, &e, &source_path_display)?;
                        continue;
                    }
                };

                self.write_ndx_and_attrs(
                    &mut *writer,
                    &mut ndx_write_codec,
                    &NdxAttrs {
                        ndx: wire_ndx,
                        iflags: &iflags,
                        fnamecmp_type,
                        xname: xname.as_deref(),
                    },
                    &sum_head,
                    pending_xattr_response.as_mut(),
                )?;

                let checksum_algorithm = self.get_checksum_algorithm();
                let use_compression = self.file_compression(&source_path).is_some();
                let flength = sum_head.flength().min(file_size);
                let append_verify = self.config.flags.append_verify;
                let wire_bytes = {
                    let mut cw = crate::writer::CountingWriter::new(&mut *writer);
                    let result = stream_append_transfer(
                        &mut cw,
                        source,
                        file_size,
                        flength,
                        append_verify,
                        checksum_algorithm,
                        self.checksum_seed,
                        if use_compression {
                            token_encoder.as_mut()
                        } else {
                            None
                        },
                        &mut stream_buf,
                    )?;
                    cw.write_all(&result.checksum_buf[..result.checksum_len])?;
                    cw.bytes_written()
                };
                bytes_sent += wire_bytes;
                literal_data += file_size.saturating_sub(flength);
            } else if has_basis {
                // Opt-in parallel sender-side delta scan: only when the flag is
                // set and the file is large enough to split usefully across
                // cores. The source is memory-mapped rather than read into a
                // Vec, but the mapping spans the whole file and every page is
                // touched during the scan, so peak RSS is proportional to the
                // file size (lazily paged in), not bounded - the gain is CPU
                // parallelism, not memory. On any mmap failure (NFS, FUSE,
                // procfs) fall back to the streaming sequential reader so the
                // wire output is unchanged. The
                // duplicate-free eligibility check lives inside
                // generate_delta_from_signature_chunked, which reverts to the
                // pruned sequential scan for a duplicate-content basis.
                let cores = rayon::current_num_threads();
                let want_parallel = self.config.flags.parallel_delta_scan
                    && should_parallel_delta(file_size, block_length, cores);
                let source_mmap = if want_parallel {
                    open_source_mmap(&source_path, self.config.write.open_noatime).ok()
                } else {
                    None
                };

                // For the sequential path only, open the streaming reader; this
                // borrows `self` mutably, so it must happen before `config`
                // (which borrows `self` immutably) is constructed.
                let source_reader: Option<Box<dyn Read>> = if source_mmap.is_none() {
                    match self.open_source_reader(&source_path, file_size) {
                        Ok(r) => Some(r),
                        Err(e) => {
                            self.record_open_failure(
                                &mut *writer,
                                wire_ndx,
                                &e,
                                &source_path_display,
                            )?;
                            continue;
                        }
                    }
                } else {
                    None
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
                let delta_script = match source_mmap.as_ref() {
                    Some(mmap) => generate_delta_from_signature_chunked(
                        mmap.as_slice(),
                        config,
                        cores.min(PARALLEL_DELTA_MAX_CHUNKS),
                    )?,
                    None => generate_delta_from_signature(
                        source_reader.expect("sequential reader opened when mmap is absent"),
                        config,
                    )?,
                };

                self.write_ndx_and_attrs(
                    &mut *writer,
                    &mut ndx_write_codec,
                    &NdxAttrs {
                        ndx: wire_ndx,
                        iflags: &iflags,
                        fnamecmp_type,
                        xname: xname.as_deref(),
                    },
                    &sum_head,
                    pending_xattr_response.as_mut(),
                )?;

                let checksum_algorithm = self.get_checksum_algorithm();
                let use_noatime = self.config.write.open_noatime;
                let wire_ops = script_to_wire_delta(delta_script, block_length);
                let use_compression = self.file_compression(&source_path).is_some();
                let is_zlib = matches!(
                    negotiated_compression,
                    Some(protocol::CompressionAlgorithm::Zlib)
                );
                // upstream: match.c:matched() - compute file checksum inline
                // during the wire-write pass, eliminating the separate
                // compute_file_checksum() call that re-opened and re-read the
                // source file.
                //
                // upstream: io.c:859 - stats.total_written counts actual wire
                // bytes after each write() syscall, not the reconstructed file
                // size. Wrap the delta+checksum write call in a CountingWriter
                // so summary "sent N bytes" reflects the wire stream the
                // receiver actually saw. Using delta_script.total_bytes()
                // (reconstructed size) trips the testsuite's "delta did not
                // engage" assertion on delta pushes.
                let wire_bytes = {
                    let mut cw = crate::writer::CountingWriter::new(&mut *writer);
                    let result = write_delta_with_inline_checksum(
                        &mut cw,
                        &wire_ops,
                        if use_compression {
                            token_encoder.as_mut()
                        } else {
                            None
                        },
                        is_zlib,
                        &source_path,
                        use_noatime,
                        checksum_algorithm,
                        self.checksum_seed,
                    )?;
                    cw.write_all(&result.checksum_buf[..result.checksum_len])?;
                    matched_data += result.matched_data;
                    literal_data += result.literal_data;
                    cw.bytes_written()
                };
                bytes_sent += wire_bytes;
            } else {
                // upstream: sender.c:354-369 - whole-file path; MSG_NO_SEND on open failure
                // Use unbuffered reader: stream_whole_file_transfer manages its
                // own 256 KB staging buffer with read_exact, so a BufReader would
                // only add an extra memcpy per byte through its internal buffer.
                let (source, src_fd): (Box<dyn Read>, Option<_>) = match self
                    .open_source_unbuffered(&source_path, file_size)
                {
                    Ok(pair) => pair,
                    Err(e) => {
                        self.record_open_failure(&mut *writer, wire_ndx, &e, &source_path_display)?;
                        continue;
                    }
                };

                self.write_ndx_and_attrs(
                    &mut *writer,
                    &mut ndx_write_codec,
                    &NdxAttrs {
                        ndx: wire_ndx,
                        iflags: &iflags,
                        fnamecmp_type,
                        xname: xname.as_deref(),
                    },
                    &sum_head,
                    pending_xattr_response.as_mut(),
                )?;

                let checksum_algorithm = self.get_checksum_algorithm();
                let use_compression = self.file_compression(&source_path).is_some();
                // upstream: io.c:859 - stats.total_written counts actual wire
                // bytes after each write() syscall, not the source file size.
                // Wrap the whole-file stream in a CountingWriter so the summary
                // "sent N bytes" reflects the post-compression wire stream the
                // receiver saw, matching the delta path above. The raw source
                // size would over-report by the compression ratio under -z/-zz
                // and trip the daemon-gzip "did -zz engage?" assertion on
                // whole-file pushes.
                // NSV-1: build the SERVE fd pair. The concrete source `File` fd
                // is surfaced by `open_source_unbuffered`; the destination socket
                // fd is not yet reachable because the daemon erases its
                // `TcpStream` to `dyn Write` before it reaches this crate, so
                // `dst_fd` is `None` for now (wired by a later rung). The pair is
                // plumbed but unused, so the transfer stays byte-for-byte
                // identical.
                #[cfg(unix)]
                let serve_fds = src_fd.map(|fd| super::super::delta::ServeFds {
                    src_fd: fd,
                    dst_fd: None,
                });
                #[cfg(not(unix))]
                let serve_fds = {
                    let _ = src_fd;
                    None::<super::super::delta::ServeFds>
                };
                let wire_bytes = {
                    let mut cw = crate::writer::CountingWriter::new(&mut *writer);
                    let result = stream_whole_file_transfer(
                        &mut cw,
                        source,
                        file_size,
                        checksum_algorithm,
                        self.checksum_seed,
                        if use_compression {
                            token_encoder.as_mut()
                        } else {
                            None
                        },
                        &mut stream_buf,
                        serve_fds,
                    )?;
                    cw.write_all(&result.checksum_buf[..result.checksum_len])?;
                    cw.bytes_written()
                };
                bytes_sent += wire_bytes;
                // Whole-file transfer: the entire body is sent as literal data
                // (no block matches). upstream: match.c accounts the full file
                // as literal_data when whole_file is in effect.
                literal_data += file_size;
            }
            files_transferred += 1;

            // upstream: sender.c:129-178 successful_send() - unlink the source
            // when --remove-source-files is active and the transfer succeeded.
            // Upstream defers the unlink to the MSG_SUCCESS handshake so the
            // receiver's commit is confirmed; we run inline here at the
            // file-transfer boundary because the generator does not exchange
            // an MSG_SUCCESS round-trip with the receiver.
            self.remove_source_file_if_requested(&source_path);

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
                        ndx_write_codec.inner_mut(),
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
                    ndx_write_codec.inner_mut(),
                )?;
                segments_sent += 1;
            }
            self.send_flist_eof(&mut *writer, ndx_write_codec.inner_mut(), segments_sent)?;
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
            matched_data,
            literal_data,
            ndx_read_codec,
            ndx_write_codec,
        })
    }

    /// Unlinks the sender-side source file when `--remove-source-files` is active.
    ///
    /// Mirrors upstream `successful_send()` in sender.c:129-178: dry-run and
    /// directory entries are skipped, vanished sources are tolerated as a
    /// successful no-op, and any other unlink failure is logged but does not
    /// fail the transfer. Upstream emits `FERROR_XFER` for failed unlinks and
    /// `FINFO` for the success notice; we mirror that by routing the success
    /// notice through the `info_log!(Remove, ...)` channel (gated by
    /// `--info=remove` / `-vv`) and the failure path through `debug_log!`.
    ///
    /// # Upstream Reference
    ///
    /// - `sender.c:129-178` `successful_send()`
    /// - `options.c:765` `remove_source_files` global
    fn remove_source_file_if_requested(&self, source_path: &Path) {
        // upstream: sender.c:137-138 - bail before any FS calls when the flag is off.
        if !self.config.flags.remove_source_files {
            return;
        }
        // upstream: sender.c:131-138 - successful_send() is a no-op when
        // do_xfers is false (dry-run). Mirror that early return so --dry-run
        // never touches the filesystem.
        if self.config.flags.dry_run {
            return;
        }
        match std::fs::remove_file(source_path) {
            Ok(()) => {
                // upstream: sender.c:175-176 - rprintf(FINFO, "sender removed %s\n", fname)
                info_log!(Remove, 1, "removing source {}", source_path.display());
            }
            // upstream: sender.c:170-171 - ENOENT is the "already removed" notice.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                info_log!(
                    Remove,
                    1,
                    "sender file already removed: {}",
                    source_path.display()
                );
            }
            Err(error) => {
                // upstream: sender.c:172-173 - rsyserr(FERROR_XFER, ...) on
                // unlink failure. We route to the debug channel instead of
                // failing the transfer so a single permission error does not
                // abort the rest of the run.
                debug_log!(
                    Send,
                    1,
                    "sender failed to remove {}: {}",
                    source_path.display(),
                    error
                );
            }
        }
    }
}

#[cfg(test)]
mod parallel_delta_gate_tests {
    use super::{
        PARALLEL_DELTA_MIN_CHUNK_BYTES, PARALLEL_DELTA_MIN_FILE_BYTES, should_parallel_delta,
    };

    #[test]
    fn single_core_never_engages() {
        assert!(!should_parallel_delta(1 << 30, 4096, 1));
    }

    #[test]
    fn small_file_never_engages() {
        // One byte below the minimum file size stays sequential even with cores.
        assert!(!should_parallel_delta(
            PARALLEL_DELTA_MIN_FILE_BYTES - 1,
            4096,
            8
        ));
    }

    #[test]
    fn large_file_with_room_for_two_ranges_engages() {
        // 64 MiB with a 4 KiB block leaves the 1 MiB floor, so 64 ranges fit.
        assert!(should_parallel_delta(
            PARALLEL_DELTA_MIN_FILE_BYTES,
            4096,
            4
        ));
    }

    #[test]
    fn huge_block_length_raises_the_floor_and_blocks_the_split() {
        // A block so large that 64 blocks exceed half the file leaves room for
        // fewer than two ranges, so the gate stays closed even past the size
        // minimum. effective_min_chunk = block_length * 64.
        let block_length = (PARALLEL_DELTA_MIN_FILE_BYTES / 64) as u32; // 1 MiB
        // effective_min_chunk = 64 MiB == file_size, so file_size / chunk == 1.
        assert!(!should_parallel_delta(
            PARALLEL_DELTA_MIN_FILE_BYTES,
            block_length,
            8
        ));
    }

    #[test]
    fn min_chunk_floor_is_one_mib() {
        assert_eq!(PARALLEL_DELTA_MIN_CHUNK_BYTES, 1024 * 1024);
    }
}
