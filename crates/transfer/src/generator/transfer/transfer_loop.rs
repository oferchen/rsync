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
    whole_stream_compression_level, write_delta_with_inline_checksum,
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
            updating_basis_file,
        };
        use super::super::protocol_io::{read_signature_blocks_keepalive, signature_read_lull_mod};

        // upstream: sender.c:217-218 - rprintf(FINFO, "send_files starting\n")
        debug_log!(Send, 1, "send_files starting");

        // upstream: sender.c:218 - int save_io_error = io_error;
        // Baseline io_error already conveyed with the file list. Any bits set
        // beyond this during the send loop (vanished/unreadable source files)
        // must be reported to the receiver so its exit code reflects them.
        let save_io_error = self.io_error;

        // Phase handling: upstream sender.c line 210: max_phase = protocol_version >= 29 ? 2 : 1
        let mut phase: i32 = 0;
        let max_phase: i32 = if self.protocol.supports_iflags() {
            2
        } else {
            1
        };

        let deadline = TransferDeadline::from_system_time(self.config.stop_at);

        let mut files_transferred = 0;
        // upstream: sender.c:343 - stats.total_transferred_size += F_LENGTH(file),
        // accumulated at the same point as xferred_files (dry-run included, since
        // the increment precedes the `if (!do_xfers)` continue at sender.c:346).
        let mut transferred_file_size = 0u64;
        // upstream: sender.c:319-335,480 - FLAG_FILE_SENT per file. A resend of
        // an already-sent entry (the redo pass) is a full-content transfer, not
        // another append delta, so track which entries have been sent once.
        let mut sent_files = SentFileTracker::default();
        let mut bytes_sent = 0u64;
        // upstream: match.c stats.matched_data / stats.literal_data accumulated
        // per token as the sender emits the delta stream.
        let mut matched_data = 0u64;
        let mut literal_data = 0u64;
        // upstream: sender.c:295-308,333-334 - the sender reconstructs
        // stats.created_* from the ITEM_IS_NEW iflags the receiver's generator
        // sends per file, keyed by the entry's mode. Never crosses the wire.
        let mut created_stats = protocol::stats::CreatedStats::new();
        // upstream: io.c IO_BUFFER_SIZE (32KB)
        let mut stream_buf = Vec::with_capacity(32 * 1024);

        // upstream: token.c creates a single compression context for the entire
        // transfer session. For zstd, the CCtx must persist across file boundaries
        // (one continuous stream). Create once here, reuse across all files.
        let negotiated_compression = self.negotiated_algorithms.map(|n| n.compression);
        let compression_threads = self.config.connection.compression_threads;
        // upstream: token.c inits the compressor with do_compression_level (the
        // negotiated --compress-level). Absent an explicit level, each codec
        // substitutes its own default via CompressionLevel::Default.
        let configured_level = self
            .config
            .connection
            .compression_level
            .unwrap_or(compress::zlib::CompressionLevel::Default);
        // upstream: token.c:206-211 - a daemon module's `dont compress = *`
        // stores the whole zlib stream (level 0); zstd/lz4 are unaffected.
        let dont_compress_match_all = self.config.connection.dont_compress_match_all;
        let mut token_encoder = negotiated_compression
            .map(|algo| {
                let level =
                    whole_stream_compression_level(dont_compress_match_all, algo, configured_level);
                create_token_encoder(algo, level, compression_threads)
            })
            .transpose()?
            .flatten();

        // upstream: token.c:1065 send_token() / token.c:1097 recv_token() dispatch
        // purely on the global `do_compression` codec, and token.c:225
        // set_compression()'s per-file suffix lookup is compiled out under `#if 0`
        // ("No compression algorithms currently allow mid-stream changing of the
        // level."). So once a codec is negotiated (`-z`), EVERY file is framed with
        // that codec on the wire; `--skip-compress`/`dont compress` suffix lists
        // never switch framing per file. A bare `*` is the only live effect and is
        // handled session-wide above via `dont_compress_match_all` (whole zlib
        // stream stored at level 0, still deflated framing). The framing decision is
        // therefore one session-level constant, not a per-file boolean.
        let use_compression = token_encoder.is_some();

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

            // upstream: generator.c:2138-2144 - during the send loop, emit a
            // keepalive once the I/O lull has elapsed so the receiver's timeout
            // does not fire while the sender is sifting files without writing.
            // A no-op unless --timeout is set (maybe_send_keepalive gates on the
            // configured allowed_lull), keeping the default path wire-identical.
            writer.maybe_send_keepalive()?;

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
                // upstream: sender.c:293-309 - a non-transfer item that is new
                // (ITEM_IS_NEW) bumps stats.created_files plus the per-type
                // counter for its mode. This is how a pushed new directory,
                // symlink, device, or FIFO reaches the "Number of created files"
                // breakdown even though it carries no file data.
                if iflags.raw() & ItemFlags::ITEM_IS_NEW != 0 && ndx < self.file_list.len() {
                    created_stats.record(self.file_list[ndx].mode());
                }
                // upstream: sender.c:286-292 - non-transfer items still echo
                // NDX + iflags + (optional xattr response) via write_ndx_and_attrs
                // so the receiver can pair the response with its outstanding
                // request and apply xattr-only updates. Without this echo the
                // wire stream stalls when ITEM_REPORT_XATTR is the only delta.
                self.emit_client_item(writer, &iflags, ndx, xname.as_deref(), itemize, false)?;
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

            // upstream: sender.c:312-317 - a valid in-range transfer request must
            // never arrive once the sender has advanced into phase 2, the terminal
            // phase where the sender has already emitted its own "phase done" and
            // is only draining the receiver's end-of-phase NDX_DONE acknowledgements.
            // Upstream prints `got transfer request in phase 2 [who_am_i]` to FERROR
            // and aborts with exit_cleanup(RERR_PROTOCOL) (exit 2). oc mirrors that
            // abort by returning a `ProtocolViolation`-tagged `InvalidData` error so
            // the loop fails loud with the same exit code instead of hanging or
            // silently servicing the request. The wire text is unchanged.
            if phase == 2 {
                return Err(protocol::protocol_violation(format!(
                    "got transfer request in phase 2 [sender] {}{}",
                    crate::role_trailer::error_location!(),
                    crate::role_trailer::sender()
                )));
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
                // upstream: sender.c:332-334 - the created_files++ for a new
                // transfer item sits in the else (first-send) branch BEFORE the
                // `if (!do_xfers)` dry-run continue, so a dry run counts created
                // files too. A dry run never redoes a file, so every entry here
                // is a first send.
                if iflags.raw() & ItemFlags::ITEM_IS_NEW != 0 {
                    created_stats.record(file_entry.mode());
                }
                // upstream: sender.c:341-344 - a dry run logs the transfer item
                // via log_item(FCLIENT) without sending data: the `-i` itemize
                // row or, under plain `-v`, the bare `%n%L` name line.
                self.emit_client_item(writer, &iflags, ndx, xname.as_deref(), itemize, true)?;
                files_transferred += 1;
                transferred_file_size += file_entry.size();
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

            // upstream: sender.c:319-335 - when a file arrives again on the redo
            // pass (`file->flags & FLAG_FILE_SENT`), the sender negates
            // append_mode and make_backups so the resend is a full-content
            // transfer. The receiver's generator has already restored
            // `csum_length = SUM_LENGTH` and negated append_mode for that redo
            // (generator.c:2178-2216), so it now sends a full block signature.
            // Honouring append here would skip reading those block sums and
            // desync the wire.
            let is_resend = sent_files.is_resend(ndx);

            // upstream: sender.c:327-334 - the created_files++ lives in the
            // `else` (first-send, not FLAG_FILE_SENT) branch, so a redo-pass
            // resend never double-counts. A transferred file is always a
            // regular file, so `record` classifies it as the derived reg count.
            if !is_resend && iflags.raw() & ItemFlags::ITEM_IS_NEW != 0 {
                created_stats.record(file_entry.mode());
            }

            // upstream: sender.c:89-95 receive_sums() - in append mode the
            // receiver's generator writes only the sum_head, not the block
            // checksums (generator.c:786 `if (append_mode > 0 && f_copy < 0)
            // return 0`). Reading blocks here would block forever, so derive
            // flength from the header and take the append literal path. A resend
            // (redo pass) clears append_mode (sender.c:324), so the signature
            // blocks are present and must be read like any full transfer.
            let is_append = self.config.flags.append && !is_resend;
            let sig_blocks = if is_append {
                Vec::new()
            } else {
                // upstream: sender.c:76 receive_sums() - on protocols below 31 the
                // sender pokes a keepalive every `allowed_lull * 5` blocks so a
                // large/slow checksum read does not trip the peer's --timeout.
                // Newer protocols multiplex the stream and set lull_mod = 0.
                let lull_mod = signature_read_lull_mod(self.protocol, writer.allowed_lull());
                let blocks =
                    read_signature_blocks_keepalive(&mut *reader, &sum_head, lull_mod, || {
                        writer.maybe_send_keepalive().map(|_| ())
                    })?;
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

            // upstream: sender.c:421-429 - in append mode, refuse to send a
            // source that has shrunk below the length recorded when the file
            // list was built (`st.st_size < F_LENGTH(file)`). Appending only
            // ever extends a file, so a now-shorter source would corrupt the
            // destination's preserved prefix. Skip it with the "skipped
            // diminished file" warning and MSG_NO_SEND (no io_error bit). A
            // stat failure (e.g. the source vanished) is left to the per-branch
            // open, which routes it through record_open_failure.
            if is_append && source_diminished_below_flist(&source_path, file_size) {
                self.record_diminished_skip(&mut *writer, wire_ndx, &source_path_display)?;
                continue;
            }

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
                        self.protocol,
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

                // upstream: sender.c:337 - the per-file updating_basis_file flag
                // gates match.c:211's backward-Copy suppression. On a redo-pass
                // resend upstream negates make_backups (sender.c:323,329) so the
                // inplace send skips the duplicate backup; mirror that by
                // clearing the backup flag for a resend (proto < 29 path).
                let updating_basis_file = updating_basis_file(
                    self.config.write.inplace,
                    self.config.write.inplace_partial,
                    self.config.flags.backup && !is_resend,
                    self.protocol,
                    fnamecmp_type,
                );

                let config = DeltaGeneratorConfig {
                    block_length,
                    sig_blocks,
                    strong_sum_length,
                    protocol: self.protocol,
                    negotiated_algorithms: self.negotiated_algorithms.as_ref(),
                    compat_flags: self.compat_flags.as_ref(),
                    checksum_seed: self.checksum_seed,
                    updating_basis_file,
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
                        self.protocol,
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
                        self.protocol,
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
            transferred_file_size += file_size;
            // upstream: sender.c:480 - `file->flags |= FLAG_FILE_SENT` once the
            // entry has actually been transferred, so a later redo request for
            // it clears append_mode/make_backups above. Skipped items
            // (diminished, open failure, non-regular) `continue` before here,
            // matching upstream which sets the flag only after a real send.
            sent_files.mark_sent(ndx);

            // upstream: sender.c:131-182 successful_send() - the source unlink is
            // DEFERRED, never run inline at send time. Upstream waits for the
            // receiver/generator to confirm the commit with MSG_SUCCESS(ndx)
            // (io.c:1623-1637) and only then unlinks in successful_send().
            // Recording this entry as pending - instead of unlinking now - is
            // what makes --remove-source-files crash-safe: an interrupted,
            // failed, or redone transfer never deletes a source that did not
            // safely land at the destination. The re-stat and changed-file
            // guards still run later, at confirmation time, inside
            // confirm_source_removal() -> remove_source_file_if_requested().
            if self.config.flags.remove_source_files && !self.config.flags.dry_run {
                self.pending_source_removals.mark_pending(ndx);
            }

            // upstream: sender.c:445-446
            // rprintf(FINFO, "sender finished %s%s%s\n", path,slash,fname)
            debug_log!(Send, 1, "sender finished {}", file_entry.path().display());

            // upstream: sender.c:430 - log_item(log_code, file, iflags, xname)
            self.emit_client_item(writer, &iflags, ndx, xname.as_deref(), itemize, true)?;

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

        // upstream: sender.c:485-486 - if (io_error != save_io_error &&
        // protocol_version >= 30) send_msg_int(MSG_IO_ERROR, io_error);
        // Emitted immediately before NDX_DONE so a remote receiver learns of
        // vanished/unreadable source files and reports exit 24/23. MSG_NO_SEND
        // only skips the file; it does not carry the exit-code bits.
        if self.io_error != save_io_error && self.protocol.supports_generator_messages() {
            let io_error = self.io_error;
            if let Err(e) = writer.send_io_error(io_error) {
                if !(tolerant && is_early_close_error(&e)) {
                    return Err(e);
                }
            }
        }

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
            transferred_file_size,
            bytes_sent,
            matched_data,
            literal_data,
            created_stats,
            ndx_read_codec,
            ndx_write_codec,
        })
    }

    /// Unlinks the sender-side source file when `--remove-source-files` is
    /// active, applying upstream's `successful_send` safety guards first and
    /// returning the `io_error` bits the caller must OR into the transfer's
    /// accumulated error state.
    ///
    /// Mirrors upstream `successful_send()` (sender.c:131-182): the source is
    /// re-stat'd (`do_stat` under `--copy-links`, else `do_lstat`) and is only
    /// unlinked when it still matches the size and modification time recorded in
    /// the file list. A vanished source (`ENOENT`) is the benign "already
    /// removed" notice (`FINFO`); a re-stat failure, a source that changed since
    /// it entered the file list, or an unlink failure is an `FERROR_XFER`, which
    /// upstream turns into `got_xfer_error` -> exit 23 (`RERR_PARTIAL`) without
    /// aborting the run. We mirror the exit code by returning
    /// [`IOERR_GENERAL`](super::super::io_error_flags::IOERR_GENERAL); the send
    /// loop reports it via `MSG_IO_ERROR` after the final file.
    ///
    /// The dev/ino "destination file" guard (sender.c:155-160) is gated on
    /// `local_server` upstream. The network generator is never `local_server`
    /// (local transfers use the engine copy path), so that guard lives only on
    /// the local-copy side.
    ///
    /// # Upstream Reference
    ///
    /// - `sender.c:131-182` `successful_send()`
    /// - `options.c:765` `remove_source_files` global
    #[must_use]
    fn remove_source_file_if_requested(
        &self,
        source_path: &Path,
        recorded: RecordedSourceIdentity,
    ) -> i32 {
        use super::super::io_error_flags::IOERR_GENERAL;

        // upstream: sender.c:139-140 - bail before any FS calls when the flag is off.
        if !self.config.flags.remove_source_files {
            return 0;
        }
        // upstream: sender.c:131-138 - successful_send() is a no-op when
        // do_xfers is false (dry-run). Mirror that early return so --dry-run
        // never touches the filesystem.
        if self.config.flags.dry_run {
            return 0;
        }

        // upstream: sender.c:150 - re-stat the source (do_stat under
        // --copy-links, else do_lstat) before removing it, so a source that
        // vanished or changed since it entered the file list is never unlinked.
        let restat = if self.config.flags.copy_links {
            std::fs::metadata(source_path)
        } else {
            std::fs::symlink_metadata(source_path)
        };
        let current = match restat {
            Ok(meta) => meta,
            // upstream: sender.c:174-175 - ENOENT is the benign FINFO notice.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                info_log!(
                    Remove,
                    1,
                    "sender file already removed: {}",
                    source_path.display()
                );
                return 0;
            }
            // upstream: sender.c:151-153,176-177 - any other re-lstat failure is
            // rsyserr(FERROR_XFER, ...), setting got_xfer_error -> exit 23.
            Err(error) => {
                eprintln!(
                    "rsync: [sender] sender failed to re-lstat \"{}\": {}",
                    source_path.display(),
                    engine::local_copy::upstream_io_error(&error),
                );
                return IOERR_GENERAL;
            }
        };

        // upstream: sender.c:162-169 - refuse to remove a source that changed
        // size or modification time since it entered the file list.
        let (size, mtime, mtime_nsec) = stat_identity(&current);
        if source_changed_since_flist(recorded, size, mtime, mtime_nsec) {
            eprintln!(
                "ERROR: Skipping sender remove for changed file: {}",
                source_path.display()
            );
            return IOERR_GENERAL;
        }

        // upstream: sender.c:171 - do_unlink(fname) once every guard passed.
        match std::fs::remove_file(source_path) {
            Ok(()) => {
                // upstream: sender.c:179-180 - INFO_GTE(REMOVE,1) success notice.
                info_log!(Remove, 1, "removing source {}", source_path.display());
                0
            }
            // upstream: sender.c:174-175 - ENOENT after the guards is still benign.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                info_log!(
                    Remove,
                    1,
                    "sender file already removed: {}",
                    source_path.display()
                );
                0
            }
            // upstream: sender.c:172-173,176-177 - rsyserr(FERROR_XFER, ...) on
            // unlink failure sets got_xfer_error -> exit 23.
            Err(error) => {
                eprintln!(
                    "rsync: [sender] sender failed to remove \"{}\": {}",
                    source_path.display(),
                    engine::local_copy::upstream_io_error(&error),
                );
                IOERR_GENERAL
            }
        }
    }

    /// Runs the deferred `--remove-source-files` unlink for a file the peer has
    /// confirmed committed via `MSG_SUCCESS(wire_ndx)`.
    ///
    /// This is the sender-side reaction to a received `MSG_SUCCESS`, mirroring
    /// upstream's `successful_send()` being invoked from the message handler
    /// (`io.c:1637`). The wire index is mapped back to its flat file-list entry
    /// and the source is unlinked only if this sender actually deferred a
    /// removal for it - an index the sender never marked pending (a duplicate
    /// confirmation, or an up-to-date entry the sender never transmitted) is
    /// ignored, so a stray `MSG_SUCCESS` can never trigger a spurious deletion.
    /// The re-stat and changed-file guards in
    /// [`remove_source_file_if_requested`](Self::remove_source_file_if_requested)
    /// still gate the unlink, so a source that vanished or changed since it
    /// entered the file list is never removed. Returns the `io_error` bits the
    /// caller must OR into the transfer's accumulated error state.
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:1623-1637` - `MSG_SUCCESS` receipt drives `successful_send(val)`.
    /// - `sender.c:131-182` - `successful_send()` unlink + guards.
    #[must_use]
    pub(crate) fn confirm_source_removal(&mut self, wire_ndx: i32) -> i32 {
        if wire_ndx < 0 {
            return 0;
        }
        let flat_ndx = self.wire_to_flat_ndx(wire_ndx);
        if flat_ndx >= self.file_list.len() {
            return 0;
        }
        if !self.pending_source_removals.confirm(flat_ndx) {
            return 0;
        }
        let source_path = self.reconstruct_source_path(flat_ndx);
        let entry = &self.file_list[flat_ndx];
        let recorded = RecordedSourceIdentity {
            size: entry.size(),
            mtime: entry.mtime(),
            mtime_nsec: entry.mtime_nsec(),
        };
        self.remove_source_file_if_requested(&source_path, recorded)
    }
}

/// Per-index record of which file-list entries the sender has already
/// transferred, mirroring upstream's per-file `FLAG_FILE_SENT` bit.
///
/// On a redo pass the receiver's generator re-requests a file it already
/// consumed, this time sending a full-length block signature instead of the
/// append short-circuit: `check_for_finished_files()` restores
/// `csum_length = SUM_LENGTH` and negates `append_mode`/`make_backups` around
/// the redo `recv_generator` call (generator.c:2178-2216). The sender mirrors
/// this from its side by keying off `FLAG_FILE_SENT`: a resend is a
/// full-content transfer, not another append delta (sender.c:319-335,482-483).
/// Without it the sender would take the no-signature append path and leave the
/// block sums the receiver just sent unread on the wire, desyncing the stream
/// against a real upstream peer.
#[derive(Default)]
struct SentFileTracker {
    /// `sent[ndx]` becomes true once entry `ndx` has been transferred at least
    /// once. Indexed by the flat file-list index, so a redo request for the
    /// same entry reads back its prior send.
    sent: Vec<bool>,
}

impl SentFileTracker {
    /// Returns true when `ndx` was already transferred, i.e. this request is a
    /// redo-pass resend (upstream `file->flags & FLAG_FILE_SENT`, sender.c:319).
    fn is_resend(&self, ndx: usize) -> bool {
        self.sent.get(ndx).copied().unwrap_or(false)
    }

    /// Records that `ndx` has now been transferred, so any later request for it
    /// is a resend (upstream `file->flags |= FLAG_FILE_SENT`, sender.c:480).
    fn mark_sent(&mut self, ndx: usize) {
        if ndx >= self.sent.len() {
            self.sent.resize(ndx + 1, false);
        }
        self.sent[ndx] = true;
    }
}

/// Source-file identity recorded in the file list, compared against a fresh
/// re-stat before `--remove-source-files` unlinks the source.
///
/// upstream: `sender.c:162` compares `st.st_size` / `st.st_mtime` /
/// `ST_MTIME_NSEC` against the file-list `F_LENGTH` / `modtime` / `F_MOD_NSEC`.
#[derive(Clone, Copy)]
struct RecordedSourceIdentity {
    size: u64,
    mtime: i64,
    mtime_nsec: u32,
}

/// Returns true when a freshly re-stat'd source no longer matches its recorded
/// file-list identity, mirroring the changed-file guard in upstream
/// `successful_send`: size, whole-second mtime, and sub-second mtime compared
/// only when the recorded timestamp carried nanoseconds (upstream gates the
/// nsec compare on `NSEC_BUMP`, i.e. a transmitted `FLAG_MOD_NSEC`).
///
/// upstream: sender.c:162-169
const fn source_changed_since_flist(
    recorded: RecordedSourceIdentity,
    current_size: u64,
    current_mtime: i64,
    current_mtime_nsec: u32,
) -> bool {
    recorded.size != current_size
        || recorded.mtime != current_mtime
        || (recorded.mtime_nsec != 0 && recorded.mtime_nsec != current_mtime_nsec)
}

/// Returns true when an append-mode source has shrunk below the length recorded
/// when the file list was built, so the sender must skip it instead of
/// appending. Appending only ever extends a file: a source now shorter than its
/// recorded `F_LENGTH` would leave the destination's preserved prefix
/// referencing bytes the source can no longer supply, corrupting the result.
///
/// The current on-disk length is read with a fresh `stat` (following symlinks,
/// matching the sender's `do_open_checklinks`). A `stat` failure - most
/// commonly a source that vanished since enumeration - returns false so the
/// per-branch open reports it through `record_open_failure` with the correct
/// vanished/general distinction, exactly as upstream reaches `map_file` only
/// after a successful `fstat`.
///
/// upstream: sender.c:421 - `if (append_mode > 0 && st.st_size < F_LENGTH(file))`
fn source_diminished_below_flist(source_path: &Path, flist_len: u64) -> bool {
    std::fs::metadata(source_path).is_ok_and(|meta| meta.len() < flist_len)
}

/// Extracts `(size, mtime_seconds, mtime_nanoseconds)` from a re-stat result in
/// the same representation the file list records, so the changed-file guard can
/// compare like-for-like across platforms.
fn stat_identity(metadata: &std::fs::Metadata) -> (u64, i64, u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec() as u32,
        )
    }
    #[cfg(not(unix))]
    {
        let (secs, nsec) = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or((0, 0), |d| (d.as_secs() as i64, d.subsec_nanos()));
        (metadata.len(), secs, nsec)
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

#[cfg(test)]
mod sender_remove_guard_tests {
    use super::{RecordedSourceIdentity, source_changed_since_flist, stat_identity};

    fn recorded(size: u64, mtime: i64, mtime_nsec: u32) -> RecordedSourceIdentity {
        RecordedSourceIdentity {
            size,
            mtime,
            mtime_nsec,
        }
    }

    #[test]
    fn unchanged_source_is_removable() {
        // Data safety: a source that still matches its file-list identity is the
        // one we transferred, so upstream successful_send unlinks it.
        let r = recorded(1024, 1_700_000_000, 500);
        assert!(!source_changed_since_flist(r, 1024, 1_700_000_000, 500));
    }

    #[test]
    fn grown_source_is_not_removed() {
        // Data safety: the user appended to the file after it entered the flist;
        // removing it now would destroy data we never sent (sender.c:162).
        let r = recorded(1024, 1_700_000_000, 0);
        assert!(source_changed_since_flist(r, 2048, 1_700_000_000, 0));
    }

    #[test]
    fn retouched_source_is_not_removed() {
        // Data safety: same size but a newer mtime means the file was rewritten
        // in place; upstream refuses the remove (sender.c:162 st_mtime compare).
        let r = recorded(1024, 1_700_000_000, 0);
        assert!(source_changed_since_flist(r, 1024, 1_700_000_500, 0));
    }

    #[test]
    fn subsecond_change_is_detected_when_flist_carried_nsec() {
        // Upstream compares nanoseconds only when the flist entry carried them
        // (NSEC_BUMP); a nonzero recorded nsec makes the compare active.
        let r = recorded(1024, 1_700_000_000, 500);
        assert!(source_changed_since_flist(r, 1024, 1_700_000_000, 999));
    }

    #[test]
    fn subsecond_change_is_ignored_when_flist_lacked_nsec() {
        // With no recorded sub-second component the nsec compare must not fire,
        // or every second-granularity source would be spuriously kept.
        let r = recorded(1024, 1_700_000_000, 0);
        assert!(!source_changed_since_flist(r, 1024, 1_700_000_000, 999));
    }

    #[test]
    fn stat_identity_reads_size_and_mtime() {
        // The guard must read back the very identity it wrote, so a freshly
        // created file compares equal to its own recorded attributes.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("src.bin");
        std::fs::write(&path, b"hello world").expect("write");
        let meta = std::fs::symlink_metadata(&path).expect("stat");
        let (size, mtime, mtime_nsec) = stat_identity(&meta);
        assert_eq!(size, 11);
        let r = recorded(size, mtime, mtime_nsec);
        assert!(!source_changed_since_flist(r, size, mtime, mtime_nsec));
    }
}

#[cfg(test)]
mod sender_diminished_guard_tests {
    use super::source_diminished_below_flist;

    /// A source that shrank below the length recorded in the file list must be
    /// skipped: appending only extends a file, so re-sending a now-shorter
    /// source would corrupt the destination's preserved prefix
    /// (sender.c:421 `st.st_size < F_LENGTH(file)`).
    #[test]
    fn shrunk_source_is_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("src.bin");
        // File list recorded 1024 bytes; the source is now only 512 on disk.
        std::fs::write(&path, vec![0u8; 512]).expect("write");
        assert!(source_diminished_below_flist(&path, 1024));
    }

    /// An unchanged source (on-disk length equals its recorded length) is a
    /// normal append and must proceed - the guard fires strictly below, so
    /// equality never skips (upstream uses `<`, not `<=`).
    #[test]
    fn equal_length_source_proceeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("src.bin");
        std::fs::write(&path, vec![0u8; 1024]).expect("write");
        assert!(!source_diminished_below_flist(&path, 1024));
    }

    /// A source that grew after enumeration still appends normally: it can
    /// supply every recorded byte plus more, so there is nothing to skip.
    #[test]
    fn grown_source_proceeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("src.bin");
        std::fs::write(&path, vec![0u8; 4096]).expect("write");
        assert!(!source_diminished_below_flist(&path, 1024));
    }

    /// A stat failure (here, a vanished source) must NOT skip via the diminished
    /// path: returning false defers to the per-branch open, which reports the
    /// vanished/general distinction through `record_open_failure`, mirroring
    /// upstream reaching the diminished check only after a successful `fstat`.
    #[test]
    fn missing_source_defers_to_open_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("does-not-exist.bin");
        assert!(!source_diminished_below_flist(&path, 1024));
    }
}

#[cfg(test)]
mod phase2_guard_tests {
    //! Terminal-phase abort guard for the sender loop.
    //!
    //! upstream: sender.c:312-317 - once `send_files()` has advanced into phase
    //! 2 (the final phase, where the sender has already emitted its own phase
    //! done and only drains the receiver's end-of-phase `NDX_DONE`
    //! acknowledgements), a valid in-range transfer request is a protocol
    //! violation. Upstream prints `got transfer request in phase 2 [who_am_i]`
    //! and `exit_cleanup(RERR_PROTOCOL)`. These tests pin that the loop aborts
    //! loud (a typed error, not a hang or silent service) while the normal
    //! phase-completion `NDX_DONE` sequence still returns `Ok`.

    use std::ffi::OsString;
    use std::io::{self, Cursor};
    use std::path::PathBuf;

    use protocol::ProtocolVersion;
    use protocol::codec::{MonotonicNdxWriter, NdxCodec};

    use crate::config::ServerConfig;
    use crate::generator::GeneratorContext;
    use crate::handshake::HandshakeResult;
    use crate::role::ServerRole;
    use crate::writer::ServerWriter;

    /// `ITEM_TRANSFER` (0x8000) as its 2-byte little-endian wire encoding, the
    /// shortint iflags the receiver sends for a real file transfer request
    /// (proto >= 29, `item_flags.rs::read`).
    const ITEM_TRANSFER_LE: [u8; 2] = [0x00, 0x80];

    fn test_handshake() -> HandshakeResult {
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

    /// Builds a generator over a single source file so wire NDX 0 is a valid,
    /// in-range transfer request.
    fn generator_with_one_file() -> (tempfile::TempDir, GeneratorContext) {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("only.txt");
        std::fs::write(&file, b"payload").expect("write source");

        let handshake = test_handshake();
        let config = ServerConfig {
            role: ServerRole::Generator,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            args: vec![OsString::from(&file)],
            ..Default::default()
        };
        let mut ctx = GeneratorContext::new_for_test(&handshake, config);
        ctx.build_file_list(&[PathBuf::from(&file)])
            .expect("build file list");
        (dir, ctx)
    }

    /// Drives the sender loop to completion over a crafted receiver wire stream,
    /// returning the loop result and the bytes the sender wrote back.
    fn drive(ctx: &mut GeneratorContext, incoming: Vec<u8>) -> io::Result<()> {
        let mut reader = Cursor::new(incoming);
        let mut writer = ServerWriter::new_plain(Vec::new());
        let mut progress: Option<&mut dyn crate::TransferProgressCallback> = None;
        let mut itemize: Option<&mut dyn crate::ItemizeCallback> = None;
        ctx.run_transfer_loop(&mut reader, &mut writer, &mut progress, &mut itemize)
            .map(|_| ())
    }

    #[test]
    fn transfer_request_in_phase_2_aborts_with_protocol_error() {
        let (_dir, mut ctx) = generator_with_one_file();

        // Two NDX_DONEs advance the sender 0 -> 1 -> 2; the following in-range
        // request (NDX 0 + ITEM_TRANSFER iflags) must never arrive in phase 2.
        let mut ndx = MonotonicNdxWriter::new(32);
        let mut wire = Vec::new();
        ndx.write_ndx_done(&mut wire).unwrap();
        ndx.write_ndx_done(&mut wire).unwrap();
        ndx.write_ndx(&mut wire, 0).unwrap();
        wire.extend_from_slice(&ITEM_TRANSFER_LE);

        let err = drive(&mut ctx, wire).expect_err("phase-2 request must abort");
        // upstream sender.c:316 exit_cleanup(RERR_PROTOCOL) (exit 2). oc tags the
        // InvalidData error as a ProtocolViolation so the core exit-code mapper
        // yields RERR_PROTOCOL(2), not RERR_STREAMIO(12). The wire kind and text
        // stay identical to the receiver's goodbye NDX_DONE guard.
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.get_ref()
                .is_some_and(|e| e.is::<protocol::ProtocolViolation>()),
            "phase-2 abort must be tagged RERR_PROTOCOL"
        );
        assert!(
            err.to_string().contains("got transfer request in phase 2"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn phase_completion_ndx_done_sequence_succeeds() {
        let (_dir, mut ctx) = generator_with_one_file();

        // The normal end-of-transfer sequence: one NDX_DONE per phase boundary
        // (0 -> 1, 1 -> 2, 2 -> break past max_phase). No transfer request ever
        // arrives, so the loop completes without tripping the phase-2 guard.
        let mut ndx = MonotonicNdxWriter::new(32);
        let mut wire = Vec::new();
        ndx.write_ndx_done(&mut wire).unwrap();
        ndx.write_ndx_done(&mut wire).unwrap();
        ndx.write_ndx_done(&mut wire).unwrap();

        drive(&mut ctx, wire).expect("clean phase completion");
    }
}

#[cfg(test)]
mod append_redo_tests {
    //! Redo-pass append desync guard for the sender loop.
    //!
    //! upstream: sender.c:319-338,482-483 - the sender tracks `FLAG_FILE_SENT`
    //! per file. The first request for an entry is sent as an append delta
    //! (`append_mode > 0`, no block signature - receive_sums returns early at
    //! generator.c:786). On a redo request the receiver's generator has already
    //! restored `csum_length = SUM_LENGTH` and negated `append_mode`
    //! (check_for_finished_files, generator.c:2178-2216) and now transmits a
    //! full block signature. The sender must negate `append_mode` too
    //! (sender.c:324) so it reads those block sums and does a full-content
    //! transfer. A static append branch would skip the block-sum read and leave
    //! them on the wire, desyncing every subsequent NDX against a real upstream
    //! peer.

    use std::ffi::OsString;
    use std::io::{self, Cursor};
    use std::path::PathBuf;

    use protocol::ProtocolVersion;
    use protocol::codec::{NdxCodec, create_ndx_codec};

    use crate::config::ServerConfig;
    use crate::flags::ParsedServerFlags;
    use crate::generator::GeneratorContext;
    use crate::handshake::HandshakeResult;
    use crate::receiver::SumHead;
    use crate::role::ServerRole;
    use crate::writer::ServerWriter;

    /// `ITEM_TRANSFER` (0x8000) as its 2-byte little-endian wire encoding.
    const ITEM_TRANSFER_LE: [u8; 2] = [0x00, 0x80];

    fn test_handshake() -> HandshakeResult {
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

    /// Builds an `--append` generator over a single 100-byte source file, so
    /// wire NDX 0 is a valid in-range transfer request.
    fn append_generator() -> (tempfile::TempDir, GeneratorContext) {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("data.bin");
        std::fs::write(&file, vec![0xA5u8; 100]).expect("write source");

        let handshake = test_handshake();
        let config = ServerConfig {
            role: ServerRole::Generator,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            args: vec![OsString::from(&file)],
            flags: ParsedServerFlags {
                append: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut ctx = GeneratorContext::new_for_test(&handshake, config);
        ctx.build_file_list(&[PathBuf::from(&file)])
            .expect("build file list");
        (dir, ctx)
    }

    /// Drives the sender loop over a crafted receiver stream, returning
    /// `(files_transferred, transferred_file_size, bytes_consumed_from_reader)`.
    fn drive(ctx: &mut GeneratorContext, incoming: Vec<u8>) -> io::Result<(usize, u64, u64)> {
        let mut reader = Cursor::new(incoming);
        let mut writer = ServerWriter::new_plain(Vec::new());
        let mut progress: Option<&mut dyn crate::TransferProgressCallback> = None;
        let mut itemize: Option<&mut dyn crate::ItemizeCallback> = None;
        let result =
            ctx.run_transfer_loop(&mut reader, &mut writer, &mut progress, &mut itemize)?;
        Ok((
            result.files_transferred,
            result.transferred_file_size,
            reader.position(),
        ))
    }

    /// A file first sent as an append delta and then re-requested on the redo
    /// pass must, on the resend, read the receiver's full block signature and
    /// perform a full-content transfer - never a second append that skips the
    /// block sums (sender.c:319-335,482-483).
    ///
    /// The redo request carries 5 block sums (100 wire bytes) the receiver's
    /// generator produced after negating `append_mode` for the redo
    /// (generator.c:2178-2216). With the static append branch the sender skips
    /// those 100 bytes; they then remain on the wire and are misread as the
    /// following NDX values, so the reader is left with 100 bytes unconsumed -
    /// the block-sum desync. Honouring `FLAG_FILE_SENT` clears append for the
    /// resend, so the whole crafted stream is consumed and both transfers land.
    #[test]
    fn append_redo_reads_full_signature_without_desync() {
        let (_dir, mut ctx) = append_generator();

        let mut rx = create_ndx_codec(32);
        let mut wire = Vec::new();

        // Phase 1 append request for NDX 0. In append mode the receiver's
        // generator writes only the sum_head (generator.c:786), here describing
        // a 40-byte existing prefix, and NO block sums.
        rx.write_ndx(&mut wire, 0).unwrap();
        wire.extend_from_slice(&ITEM_TRANSFER_LE);
        SumHead::new(2, 20, 0, 0).write(&mut wire).unwrap(); // flength = 40
        // NDX_DONE advances the sender phase 0 -> 1.
        rx.write_ndx_done(&mut wire).unwrap();

        // Redo pass: the SAME NDX 0 is re-requested, this time with a full block
        // signature (generator.c:2178-2216 restored csum_length = SUM_LENGTH and
        // negated append_mode for the redo). 5 blocks of a 16-byte strong sum
        // describe the whole 100-byte file: 5 * (4 rolling + 16 strong) = 100
        // wire bytes the sender MUST consume to stay in sync.
        rx.write_ndx(&mut wire, 0).unwrap();
        wire.extend_from_slice(&ITEM_TRANSFER_LE);
        SumHead::new(5, 20, 16, 0).write(&mut wire).unwrap();
        let block_sum_bytes = 5 * (4 + 16);
        wire.extend(std::iter::repeat_n(0x00u8, block_sum_bytes));

        // Two more NDX_DONEs drain phases 1 -> 2 -> break past max_phase.
        rx.write_ndx_done(&mut wire).unwrap();
        rx.write_ndx_done(&mut wire).unwrap();
        let total = wire.len() as u64;

        let (files, transferred_file_size, consumed) =
            drive(&mut ctx, wire).expect("append redo must not desync");

        // Both the append send and the full-content redo completed.
        assert_eq!(
            files, 2,
            "append send + redo resend both count as transfers"
        );
        // #178: total_transferred_size accumulates F_LENGTH at each transfer
        // point (sender.c:343), in lockstep with files_transferred. Both sends
        // of the 100-byte file count, so the sender-side total is 2 * 100 = 200.
        // A generator that never summed the length reports 0 here, which is what
        // made every remote push print `Total transferred file size: 0`.
        assert_eq!(
            transferred_file_size, 200,
            "sender must sum F_LENGTH for every transfer (append send + redo)"
        );
        // Every crafted byte was consumed: the resend read the full block
        // signature instead of leaving it on the wire. The static-append bug
        // leaves exactly the block-sum bytes unread, so `consumed` falls short.
        assert_eq!(
            consumed, total,
            "resend left the redo block signature unread -> wire desync"
        );
    }
}
