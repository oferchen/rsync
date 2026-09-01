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
use protocol::codec::{MonotonicNdxWriter, NDX_DEL_STATS, NDX_DONE, NdxCodec, create_ndx_codec};
use protocol::stats::DeleteStats;

use super::super::delta::{
    ScanSource, create_token_encoder, poison_file_checksum, script_to_wire_delta,
    stream_append_transfer, stream_whole_file_transfer, whole_stream_compression_level,
    write_delta_with_inline_checksum,
};
use super::super::item_flags::ItemFlags;
use super::super::protocol_io::NdxAttrs;
use super::super::{
    GeneratorContext, SegmentScheduler, SenderFstatError, TransferLoopResult, flush_with_count,
    is_early_close_error,
};
use crate::delta_config::DeltaGeneratorConfig;
use crate::reader::BufferedInputHint;
use crate::receiver::SumHead;
use crate::writer::{BatchRoute, MsgInfoSender};

/// Scoped view of the sender's transfer stream, upstream's `f_xfer`.
///
/// upstream `sender.c:217` picks the destination of a file's sum head, tokens
/// and trailing file checksum once per `send_files()` run:
///
/// ```c
/// int f_xfer = write_batch < 0 ? batch_fd : f_out;
/// ```
///
/// Under `--only-write-batch` that stream goes into the batch file *instead of*
/// the wire, which is what lets the remote receiver run with `dry_run = 1`
/// (`main.c:1839`) and never read a byte of delta. The NDX+attrs header
/// (`sender.c:766`) always stays on the wire, so the divert is scoped to the
/// payload and reverted on drop. Under `--write-batch` (or with no batch at
/// all) the route is unchanged and the recorder keeps teeing (`io.c:2282`).
struct XferSink<'w, W: Write + MsgInfoSender + ?Sized> {
    writer: &'w mut W,
    diverted: bool,
}

impl<'w, W: Write + MsgInfoSender + ?Sized> XferSink<'w, W> {
    /// Borrows `writer`, diverting it into the batch when `divert` is set.
    fn new(writer: &'w mut W, divert: bool) -> Self {
        if divert {
            writer.set_batch_route(BatchRoute::Divert);
        }
        Self {
            writer,
            diverted: divert,
        }
    }
}

impl<W: Write + MsgInfoSender + ?Sized> Write for XferSink<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writer.write(buf)
    }

    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        self.writer.write_vectored(bufs)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

impl<W: Write + MsgInfoSender + ?Sized> Drop for XferSink<'_, W> {
    fn drop(&mut self) {
        if self.diverted {
            self.writer.set_batch_route(BatchRoute::Tee);
        }
    }
}

/// Narrows a payload byte count to the part that actually reached the wire.
///
/// upstream: `io.c:2255-2258` - a `write_buf()` aimed at `batch_fd` takes the
/// `safe_write()` shortcut and returns before `total_data_written += len`, so
/// bytes diverted into the batch never count towards "sent N bytes".
const fn sent_bytes(counted: u64, diverted: bool) -> u64 {
    if diverted { 0 } else { counted }
}

/// Whether `error` is a tagged [`protocol::ProtocolViolation`] - an abort that
/// upstream maps to `exit_cleanup(RERR_PROTOCOL)` - rather than an ordinary
/// per-file open failure that the sender records with `MSG_NO_SEND` and skips.
///
/// Used to route the device-guard abort (`sender.c:407-409`) surfaced by
/// [`GeneratorContext::open_source_unbuffered`] to a fatal return instead of
/// [`GeneratorContext::record_open_failure`].
fn is_protocol_violation(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.is::<protocol::ProtocolViolation>())
}

/// Whether `error` is a tagged [`SenderFstatError`] - a failed `fstat(2)` on the
/// just-opened source fd that upstream aborts with `exit_cleanup(RERR_FILEIO)` -
/// rather than an ordinary per-file open failure the sender records with
/// `MSG_NO_SEND` and skips.
///
/// Used to route the fstat abort surfaced by
/// [`GeneratorContext::open_source_unbuffered`] to a fatal return (mapping to
/// `RERR_FILEIO`, exit 11) instead of [`GeneratorContext::record_open_failure`].
///
/// upstream: `sender.c` `do_fstat` - `if (do_fstat(fd, &st) != 0) { ...
/// exit_cleanup(RERR_FILEIO); }`.
fn is_sender_fstat_error(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.is::<SenderFstatError>())
}

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
fn open_source_mmap(
    path: &std::path::Path,
    source_open: &super::super::open_source::SourceOpen,
) -> io::Result<fast_io::MmapReader> {
    // The mmap reader wraps the fd opened under the sender's symlink-race
    // policy (confined / O_NOFOLLOW), so the parallel-delta scan reads the
    // same protected inode as the sequential path. upstream: sender.c maps
    // the fd returned by secure_relative_open / do_open_checklinks.
    let file = source_open.open(path)?;
    fast_io::MmapReader::from_file(file)
}

/// Formats the sender-side re-lstat/remove failure diagnostic.
///
/// Mirrors upstream `sender.c:459`
/// `rsyserr(FERROR_XFER, errno, "sender failed to %s %s", failed_op, fname)`:
/// the path is emitted with a bare `%s`, never `full_fname()`, so it carries no
/// surrounding quotes.
fn sender_op_failure(op: &str, path: &Path, error: &io::Error) -> String {
    format!(
        "rsync: [sender] sender failed to {} {}: {}",
        op,
        path.display(),
        engine::local_copy::upstream_io_error(error),
    )
}

/// What a `--remove-source-files` removal attempt owes its caller: the
/// `io_error` bits to accumulate, and the `FERROR_XFER` diagnostic to report.
///
/// The two travel together because upstream produces them together: every
/// failing arm of `successful_send()` ends at `rsyserr(FERROR_XFER, ...)` /
/// `rprintf(FERROR_XFER, ...)`, and it is that log code - not an `io_error`
/// bit - that upstream turns into the exit status. `rwrite()` sets
/// `got_xfer_error = 1` on `FERROR_XFER`, and on a server it *also* forwards
/// the text to the client as `MSG_ERROR_XFER`, which sets `got_xfer_error` over
/// there too; `exit_cleanup()` then lifts a zero exit to `RERR_PARTIAL` (23).
/// `successful_send()` sets no `io_error` bit at all, so a sender that only
/// records bits leaves a pulling client at exit 0.
///
/// The `io_error` bit is kept as well because oc's client-side sender drains it
/// into its own exit code; it is redundant with the diagnostic, never a
/// substitute for it.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/sender.c:455-462` - the shared `failed:` label
/// - `rsync-3.5.0/log.c:337-338` - `case FERROR_XFER: got_xfer_error = 1;`
/// - `rsync-3.5.0/log.c:357-367` - `am_server` forwards it as `MSG_ERROR_XFER`
/// - `rsync-3.5.0/cleanup.c:217-218` - `got_xfer_error` -> `RERR_PARTIAL`
#[must_use]
pub(crate) struct SourceRemovalOutcome {
    /// `io_error` bits to OR into the transfer's accumulated state.
    pub(crate) io_error: i32,
    /// Upstream's `FERROR_XFER` text, newline-terminated, when the removal
    /// failed or a guard refused it.
    pub(crate) error_xfer: Option<String>,
}

impl SourceRemovalOutcome {
    /// The removal succeeded, was not requested, or hit upstream's benign
    /// `ENOENT` "already removed" arm.
    const fn clean() -> Self {
        Self {
            io_error: 0,
            error_xfer: None,
        }
    }

    /// The removal failed or a guard refused it: upstream reports `text` at
    /// `FERROR_XFER` and finishes `RERR_PARTIAL` (23).
    fn error_xfer(text: String) -> Self {
        Self {
            io_error: super::super::io_error_flags::IOERR_GENERAL,
            error_xfer: Some(text),
        }
    }
}

impl GeneratorContext {
    /// Writes one file's NDX + iflags (+ optional xattr response) header and the
    /// sum head that follows it.
    ///
    /// The two go to different destinations under `--only-write-batch`: the
    /// header is what the receiver still reads (`receiver.c:811-817` logs the
    /// item and moves on), while the sum head opens the payload stream and
    /// therefore belongs to the batch. `divert` selects that split; when it is
    /// `false` both land on the wire exactly as before.
    ///
    /// # Upstream Reference
    ///
    /// - `sender.c:468-485` - `write_ndx_and_attrs()` body (calls
    ///   `send_xattr_request(fname, file, f_out)` when ITEM_REPORT_XATTR set)
    /// - `sender.c:766-767` - `write_ndx_and_attrs(f_out, ...)` followed by
    ///   `write_sum_head(f_xfer, s)`
    fn write_ndx_attrs_and_sum_head<W: Write + MsgInfoSender>(
        &self,
        writer: &mut W,
        ndx_codec: &mut impl NdxCodec,
        attrs: &NdxAttrs<'_>,
        sum_head: &SumHead,
        xattr_response: Option<&mut protocol::xattr::XattrList>,
        divert: bool,
    ) -> io::Result<()> {
        self.write_ndx_iflags_and_xattr_response(writer, ndx_codec, attrs, xattr_response)?;
        sum_head.write(&mut XferSink::new(writer, divert))
    }

    /// Dispatches queued INC_RECURSE sub-lists until the receiver has at least
    /// `MIN_FILECNT_LOOKAHEAD` entries queued ahead of the list it is working
    /// through.
    ///
    /// Upstream calls `send_extra_file_list(f_out, MIN_FILECNT_LOOKAHEAD)` from
    /// two points in the send loop (`sender.c:231` and `sender.c:265`); this is
    /// that one function, so both call sites share a single expression of the
    /// rule. The loop condition lives in [`SegmentScheduler::next_to_send`],
    /// which folds each dispatch into the backlog before returning - so, like
    /// upstream's `while (file_total - file_old_total < at_least)`, the test is
    /// re-evaluated on every iteration and stops the burst as soon as enough
    /// entries are queued.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2124-2139` - `send_extra_file_list()` loop head
    /// - `sender.c:231,265` - the two call sites this method serves
    fn send_extra_file_lists<W: Write>(
        &mut self,
        writer: &mut super::super::super::writer::ServerWriter<W>,
        scheduler: &mut SegmentScheduler,
        flist_writer: &mut protocol::flist::FileListWriter,
        ndx_codec: &mut MonotonicNdxWriter,
        flist_done_remaining: &mut usize,
    ) -> io::Result<()> {
        while let Some(seg) = scheduler.next_to_send() {
            self.encode_and_send_segment(&mut *writer, seg, flist_writer, ndx_codec.inner_mut())?;
            *flist_done_remaining += 1;
        }
        Ok(())
    }

    /// Emits `NDX_FLIST_EOF` once the scheduler has handed out every sub-list.
    ///
    /// Must run at every point inside the send loop that can exhaust the
    /// scheduler, not just after the floor top-up: the receiver will not send
    /// its final `NDX_DONE` until it has seen `NDX_FLIST_EOF`, so a dispatch
    /// that empties the queue and then parks on a read would deadlock.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2848-2849` - `write_ndx(f, NDX_FLIST_EOF); flist_eof = 1`
    fn send_flist_eof_if_exhausted<W: Write>(
        &mut self,
        writer: &mut W,
        scheduler: &SegmentScheduler,
        ndx_codec: &mut protocol::codec::NdxCodecEnum,
    ) -> io::Result<()> {
        if !self.incremental.flist_eof_sent && scheduler.is_exhausted() {
            self.send_flist_eof(writer, ndx_codec, scheduler.dispatched_count())?;
        }
        Ok(())
    }

    /// Queues one further sub-list at the moment the sender is about to block
    /// for receiver input, growing the lookahead window past
    /// `MIN_FILECNT_LOOKAHEAD` towards `MAX_FILECNT_LOOKAHEAD`.
    ///
    /// [`Self::send_extra_file_lists`] only tops the backlog *up to* the floor,
    /// which leaves the window pinned there for the whole transfer. Upstream
    /// does not stop at the floor: while the backlog is under the ceiling it
    /// polls with a zero timeout (`io.c:836-843`), and on a poll that finds no
    /// input ready it sends exactly one more sub-list (`io.c:855`,
    /// `at_least = -1` resolving to `backlog + 1` at `flist.c:2407`). oc has no
    /// poll loop, but `has_buffered_input()` marks the same instant - the read
    /// that is genuinely about to block - so the top-up hangs off that instead.
    ///
    /// One segment per idle turn, matching upstream's `backlog + 1`: this is a
    /// use of time the sender was going to spend waiting, not a spin. The
    /// caller is already committed to blocking on the receiver immediately
    /// afterwards, so nothing here polls, retries, or busy-waits.
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:836-843` - the `MAX_FILECNT_LOOKAHEAD` ceiling on this path
    /// - `io.c:851-857` - the idle poll result that triggers the top-up
    fn grow_lookahead_while_idle<W: Write>(
        &mut self,
        writer: &mut super::super::super::writer::ServerWriter<W>,
        scheduler: &mut SegmentScheduler,
        flist_writer: &mut protocol::flist::FileListWriter,
        ndx_codec: &mut MonotonicNdxWriter,
        flist_done_remaining: &mut usize,
    ) -> io::Result<()> {
        if let Some(seg) = scheduler.next_when_idle() {
            self.encode_and_send_segment(&mut *writer, seg, flist_writer, ndx_codec.inner_mut())?;
            *flist_done_remaining += 1;
        }
        Ok(())
    }

    /// Runs the main file transfer loop, reading NDX requests from receiver.
    ///
    /// This method processes file transfer requests in phases until all phases complete.
    /// For each file index received, it reads signatures, generates deltas, and sends data.
    ///
    /// # Upstream Reference
    ///
    /// - `sender.c:send_files()` - Main send loop (lines 210-462)
    /// - `io.c:read_ndx/write_ndx` - NDX protocol encoding
    pub(super) fn run_transfer_loop<R: Read + BufferedInputHint, W: Write>(
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

        // upstream: sender.c:506-507 - rprintf(FINFO, "send_files starting\n")
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
                create_token_encoder(
                    algo,
                    level,
                    compression_threads,
                    u32::from(self.protocol.as_u8()),
                )
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

        // upstream: sender.c:217 - `int f_xfer = write_batch < 0 ? batch_fd :
        // f_out`. Under `--only-write-batch` the sum head, tokens and file
        // checksum are recorded into the batch INSTEAD of being sent, so the
        // remote receiver - which server_options() put into `dry_run` via the
        // `--only-write-batch=X` placeholder (options.c:2850, main.c:1839) and
        // which therefore reads no payload (receiver.c:811-817) - never has an
        // unread stream backing up behind it. Constant for the whole run,
        // exactly like upstream's single `f_xfer` binding.
        let divert_xfer = self.config.flags.only_write_batch;

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
        // upstream: sender.c:242-250 - tracks remaining flist-free NDX_DONEs.
        // With INC_RECURSE, the client sends one NDX_DONE per completed flist
        // (initial + sub-lists). The sender echoes these without phase change
        // until all flists are freed, then falls through to the normal phase
        // transition. This counter tracks how many flist-free echoes remain.
        let mut flist_done_remaining: usize = 0;

        // upstream: sender.c - dry-run skips data transfer; daemon may close early
        let tolerant = self.config.flags.dry_run;

        loop {
            // upstream: io.c:750 - the sender's I/O loop acts on
            // got_kill_signal only at a frame boundary. Checking before the
            // next NDX read means a shutdown never truncates a delta already
            // being written to the wire.
            crate::shared::check_shutdown()?;
            // upstream: sender.c:227 - send extra file lists at top of loop
            if inc_recurse {
                self.send_extra_file_lists(
                    &mut *writer,
                    &mut scheduler,
                    &mut flist_writer,
                    &mut ndx_write_codec,
                    &mut flist_done_remaining,
                )?;

                self.send_flist_eof_if_exhausted(
                    &mut *writer,
                    &scheduler,
                    ndx_write_codec.inner_mut(),
                )?;
            }

            // upstream: io.c:640-724 perform_io() flushes buffered output only
            // while genuinely waiting for input via select(); when the next
            // request is already buffered (iobuf.in.len >= needed, io.c:643) it
            // returns immediately without draining output. Our Read/Write traits
            // are independent, so we mirror that: flush before a read that would
            // actually block (no demuxed request buffered), but skip it while
            // more requests remain in the reader's frame buffer. Skipping is
            // deadlock-safe precisely because a buffered read cannot block, and
            // it lets the writer coalesce per-file deltas up to its buffer bound
            // - ~24 files per socket write, matching upstream's iobuf.out
            // batching instead of one write() per file.
            if !reader.has_buffered_input() {
                // upstream: io.c:851-857 - a poll that finds nothing to read is
                // the sender's cue to queue one more sub-list before it parks,
                // which is what grows the lookahead past the floor. Emitting it
                // here rather than after the flush keeps it in the same socket
                // write as the deltas already buffered.
                if inc_recurse {
                    self.grow_lookahead_while_idle(
                        &mut *writer,
                        &mut scheduler,
                        &mut flist_writer,
                        &mut ndx_write_codec,
                        &mut flist_done_remaining,
                    )?;
                    self.send_flist_eof_if_exhausted(
                        &mut *writer,
                        &scheduler,
                        ndx_write_codec.inner_mut(),
                    )?;
                }
                flush_with_count(writer)?;
            }

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
                        // upstream: sender.c:246-261 - INC_RECURSE flist-free path.
                        // With INC_RECURSE, the client sends one NDX_DONE per
                        // completed sub-file-list before the actual phase transitions.
                        // Echo these without incrementing phase, matching upstream's
                        // flist_free(first_flist) loop.
                        if inc_recurse && flist_done_remaining > 0 {
                            flist_done_remaining -= 1;
                            // upstream: sender.c:247-248 -
                            // file_old_total -= first_flist->used; flist_free(first_flist).
                            // Reclaim heap data from the oldest completed segment to
                            // reduce RSS. Entries stay in place for NDX indexing but
                            // their PathBuf/extras allocations are freed.
                            self.reclaim_oldest_segment();
                            // upstream: sender.c:251 - `file_old_total =
                            // cur_flist->used`. The freed list drops out of the
                            // lookahead backlog and the next sub-list becomes
                            // the one the receiver is working through, so the
                            // backlog shrinks by that list's entry count and
                            // the throttle admits another segment.
                            scheduler.retire_current_flist();

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

                        // upstream: sender.c:256-261 - phase transition.
                        // Increment phase first, break without echo if past max_phase.
                        phase += 1;
                        if phase > max_phase {
                            break;
                        }
                        // upstream: sender.c:258-259
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
                    // upstream: rsync.c:343-353 - `if (!inc_recurse || am_sender)`
                    // rejects EVERY remaining negative index with
                    // "Invalid file index: %d (%d - %d) [%s]" and
                    // exit_cleanup(RERR_PROTOCOL). The gate sits ABOVE the
                    // NDX_FLIST_EOF branch (:354) and the sub-list branch
                    // (:360), so for a sender those are protocol violations
                    // too, not markers to skip: only a receiver inside the
                    // INC_RECURSE window may grow its list from the stream.
                    // Continuing here instead would let a peer desync this
                    // loop silently.
                    _ => {
                        return Err(crate::receiver::ndx_stream::invalid_file_index(
                            ndx,
                            // Mirrors GoodbyeNdxSink::last_file_ndx (goodbye.rs),
                            // upstream rsync.c:345-348.
                            self.file_list().len() as i32 - 1,
                            crate::receiver::ndx_stream::StreamRole::Sender,
                        ));
                    }
                }
            }

            // upstream: sender.c:267-272 - preserve the original wire NDX for
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

            // upstream: sender.c:286-290 - drain the generator's xattr request
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

            // upstream: sender.c:283-284
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

            // upstream: sender.c:347-350 - dry_run (!do_xfers) logs the item and
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
                // upstream: sender.c:347-350 - a dry run logs the transfer item
                // via log_item(FCLIENT) without sending data: the `-i` itemize
                // row or, under plain `-v`, the bare `%n%L` name line.
                self.emit_client_item(writer, &iflags, ndx, xname.as_deref(), itemize, true)?;
                files_transferred += 1;
                transferred_file_size += file_entry.size();
                flush_with_count(writer)?;
                continue;
            }

            // upstream: sender.c:120 - receive_sums(), which calls
            // io.c:read_sum_head(). That reader rejects an s2length wider than
            // the negotiated transfer digest (`xfer_sum_len`): the block loop
            // below consumes `4 + s2length` bytes per block, so a strong sum
            // wider than the checksum the generator wrote would desync the
            // stream. get_checksum_algorithm() yields the negotiated (or
            // protocol-default) transfer checksum, whose digest_len is upstream's
            // xfer_sum_len (checksum.c:214 csum_len_for_type()).
            let xfer_sum_len = self.get_checksum_algorithm().digest_len() as u32;
            let sum_head = SumHead::read_negotiated(&mut *reader, xfer_sum_len)?;
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

            // The sender's per-connection source-open policy (confinement for
            // a non-chroot daemon, O_NOFOLLOW otherwise). Threaded into the
            // free-function read paths (mmap scan, inline-checksum re-read) so
            // every open of this source applies the same symlink-race defence.
            // upstream: sender.c:359-383.
            let source_open = self.source_open();

            // upstream: sender.c:325-341 - when a file arrives again on the redo
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

            // upstream: sender.c:462-471 - a source read error during
            // match_sums() is not fatal. map_ptr() zeroed the unreadable window
            // and the token stream ran to completion; the file checksum is
            // poisoned (match.c:414-423) so the receiver redoes the file, and
            // unmap_file()'s saved status is reported here, after log_item().
            let mut read_error: Option<io::Error>;

            if is_append && has_basis {
                // upstream: match.c:371-390 - append mode streams only the tail
                // past the existing prefix; the sum_head's count/blength encode
                // that flength. No block matching, no signature blocks.
                let (source, _src_fd, file_size): (Box<dyn Read>, Option<_>, u64) = match self
                    .open_source_unbuffered(&source_path, file_size)
                {
                    Ok(triple) => triple,
                    // upstream: sender.c:407-409 - a device source without
                    // --copy-devices aborts with exit_cleanup(RERR_PROTOCOL).
                    Err(e) if is_protocol_violation(&e) => return Err(e),
                    // upstream: sender.c do_fstat - a failed fstat on the opened
                    // fd aborts fatally with exit_cleanup(RERR_FILEIO), never a
                    // MSG_NO_SEND skip.
                    Err(e) if is_sender_fstat_error(&e) => return Err(e),
                    Err(e) => {
                        self.record_open_failure(&mut *writer, wire_ndx, &e, &source_path_display)?;
                        continue;
                    }
                };

                self.write_ndx_attrs_and_sum_head(
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
                    divert_xfer,
                )?;

                let checksum_algorithm = self.get_checksum_algorithm();
                let flength = sum_head.flength().min(file_size);
                let append_verify = self.config.flags.append_verify;
                let wire_bytes = {
                    let mut cw = crate::writer::CountingWriter::new(XferSink::new(
                        &mut *writer,
                        divert_xfer,
                    ));
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
                    read_error = result.read_error;
                    let mut checksum_buf = result.checksum_buf;
                    if read_error.is_some() {
                        poison_file_checksum(&mut checksum_buf, result.checksum_len);
                    }
                    cw.write_all(&checksum_buf[..result.checksum_len])?;
                    sent_bytes(cw.bytes_written(), divert_xfer)
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
                    open_source_mmap(&source_path, &source_open).ok()
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
                    // upstream: sender.c:109-110 - receive_sums() gives the
                    // LAST basis block this length instead of blength, and only
                    // when it is non-zero. The field is read and range-checked
                    // at io.c:2061-2064, so it is always available here.
                    remainder: sum_head.remainder,
                };
                // Upstream scans and emits tokens over the same map_ptr()
                // window, so a read error is absorbed by the scan too. oc scans
                // in a separate pass, so ScanSource gives that pass the same
                // zero-fill-and-continue behaviour (fileio.c:299-306). The mmap
                // path has no io::Error to catch - a bad page raises SIGBUS -
                // so it is left alone.
                let mut scan_source = source_reader.map(|r| ScanSource::new(r, file_size));
                let delta_script = match source_mmap.as_ref() {
                    Some(mmap) => generate_delta_from_signature_chunked(
                        mmap.as_slice(),
                        config,
                        cores.min(PARALLEL_DELTA_MAX_CHUNKS),
                    )?,
                    None => generate_delta_from_signature(
                        scan_source
                            .as_mut()
                            .expect("sequential reader opened when mmap is absent"),
                        config,
                    )?,
                };
                read_error = scan_source.and_then(ScanSource::into_read_error);

                self.write_ndx_attrs_and_sum_head(
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
                    divert_xfer,
                )?;

                let checksum_algorithm = self.get_checksum_algorithm();
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
                    let mut cw = crate::writer::CountingWriter::new(XferSink::new(
                        &mut *writer,
                        divert_xfer,
                    ));
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
                        &source_open,
                        checksum_algorithm,
                        self.checksum_seed,
                        self.protocol,
                    )?;
                    if read_error.is_none() {
                        read_error = result.read_error;
                    }
                    let mut checksum_buf = result.checksum_buf;
                    if read_error.is_some() {
                        poison_file_checksum(&mut checksum_buf, result.checksum_len);
                    }
                    cw.write_all(&checksum_buf[..result.checksum_len])?;
                    matched_data += result.matched_data;
                    literal_data += result.literal_data;
                    sent_bytes(cw.bytes_written(), divert_xfer)
                };
                bytes_sent += wire_bytes;
            } else {
                // upstream: sender.c:385-400 - whole-file path; MSG_NO_SEND on open failure
                // Use unbuffered reader: stream_whole_file_transfer manages its
                // own 256 KB staging buffer with read_exact, so a BufReader would
                // only add an extra memcpy per byte through its internal buffer.
                let (source, src_fd, file_size): (Box<dyn Read>, Option<_>, u64) = match self
                    .open_source_unbuffered(&source_path, file_size)
                {
                    Ok(triple) => triple,
                    // upstream: sender.c:407-409 - a device source without
                    // --copy-devices aborts with exit_cleanup(RERR_PROTOCOL).
                    Err(e) if is_protocol_violation(&e) => return Err(e),
                    // upstream: sender.c do_fstat - a failed fstat on the opened
                    // fd aborts fatally with exit_cleanup(RERR_FILEIO), never a
                    // MSG_NO_SEND skip.
                    Err(e) if is_sender_fstat_error(&e) => return Err(e),
                    Err(e) => {
                        self.record_open_failure(&mut *writer, wire_ndx, &e, &source_path_display)?;
                        continue;
                    }
                };

                self.write_ndx_attrs_and_sum_head(
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
                    divert_xfer,
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
                    let mut cw = crate::writer::CountingWriter::new(XferSink::new(
                        &mut *writer,
                        divert_xfer,
                    ));
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
                    read_error = result.read_error;
                    let mut checksum_buf = result.checksum_buf;
                    if read_error.is_some() {
                        poison_file_checksum(&mut checksum_buf, result.checksum_len);
                    }
                    cw.write_all(&checksum_buf[..result.checksum_len])?;
                    sent_bytes(cw.bytes_written(), divert_xfer)
                };
                bytes_sent += wire_bytes;
                // Whole-file transfer: the entire body is sent as literal data
                // (no block matches). upstream: match.c accounts the full file
                // as literal_data when whole_file is in effect.
                literal_data += file_size;
            }
            files_transferred += 1;
            transferred_file_size += file_size;
            // upstream: sender.c:804 - `file->flags |= FLAG_FILE_SENT` once the
            // entry has actually been transferred, so a later redo request for
            // it clears append_mode/make_backups above. Skipped items
            // (diminished, open failure, non-regular) `continue` before here,
            // matching upstream which sets the flag only after a real send.
            sent_files.mark_sent(ndx);

            // upstream: sender.c:395 successful_send() - the source unlink is
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

            // upstream: sender.c:461 - log_item(log_code, file, iflags, xname)
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

            // upstream: sender.c:462-471 - once the file has been sent, logged
            // and its progress ended, unmap_file() surfaces the saved read
            // errno: set IOERR_GENERAL (exit 23), report it, and move on to the
            // next file rather than aborting the run.
            if let Some(err) = read_error.take() {
                self.record_read_errors(&mut *writer, &err, &source_path_display)?;
            }

            // upstream: sender.c:261 - send extra file lists at bottom of loop
            if inc_recurse {
                self.send_extra_file_lists(
                    &mut *writer,
                    &mut scheduler,
                    &mut flist_writer,
                    &mut ndx_write_codec,
                    &mut flist_done_remaining,
                )?;
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
        // The lookahead throttle is deliberately bypassed here: the receiver
        // will not send its final NDX_DONE until it has seen NDX_FLIST_EOF, so
        // there is no longer anyone to drain the backlog.
        //
        // No need to track flist_done_remaining here: the EOF flush is the
        // final write loop in this function, so the counter is never read
        // again. Mid-transfer increments in the NDX_DONE arm are still needed
        // for the in-loop accounting.
        if inc_recurse && !self.incremental.flist_eof_sent {
            while let Some(seg) = scheduler.next_forced() {
                self.encode_and_send_segment(
                    &mut *writer,
                    seg,
                    &mut flist_writer,
                    ndx_write_codec.inner_mut(),
                )?;
            }
            self.send_flist_eof(
                &mut *writer,
                ndx_write_codec.inner_mut(),
                scheduler.dispatched_count(),
            )?;
        }

        // Cache flist_writer back for potential reuse (e.g., phase 2).
        self.incremental.flist_writer_cache = Some(flist_writer);

        // upstream: sender.c:488-489
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

        // upstream: sender.c:485-493 - after the transfer loop exits, the sender
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
    /// Mirrors upstream `successful_send()` (sender.c:395): the source is
    /// re-stat'd through a confined parent descriptor (`do_stat_atfd` under
    /// `--copy-links`, else `do_lstat_atfd`) and is only unlinked - through that
    /// same descriptor - when it still matches the size and modification time
    /// recorded in the file list. A vanished source (`ENOENT`) is the benign "already
    /// removed" notice (`FINFO`); a re-stat failure, a source that changed since
    /// it entered the file list, or an unlink failure is an `FERROR_XFER`, which
    /// upstream turns into `got_xfer_error` -> exit 23 (`RERR_PARTIAL`) without
    /// aborting the run. We mirror that by returning the diagnostic in
    /// [`SourceRemovalOutcome::error_xfer`] for the caller to route through
    /// `emit_sender_diagnostic`, which is where the `got_xfer_error` flag and
    /// the `MSG_ERROR_XFER` frame to the client both come from.
    ///
    /// The dev/ino "destination file" guard (sender.c:433-440) is gated on
    /// `local_server` upstream. The network generator is never `local_server`
    /// (local transfers use the engine copy path), so that guard lives only on
    /// the local-copy side.
    ///
    /// # Upstream Reference
    ///
    /// - `sender.c:395` `successful_send()`
    /// - `options.c:765` `remove_source_files` global
    fn remove_source_file_if_requested(
        &self,
        source_path: &Path,
        display_name: &Path,
        recorded: RecordedSourceIdentity,
    ) -> SourceRemovalOutcome {
        // upstream: sender.c:405-406 - bail before any FS calls when the flag is off.
        if !self.config.flags.remove_source_files {
            return SourceRemovalOutcome::clean();
        }
        // upstream: sender.c:405-406 - successful_send() is a no-op when
        // do_xfers is false (dry-run). Mirror that early return so --dry-run
        // never touches the filesystem.
        if self.config.flags.dry_run {
            return SourceRemovalOutcome::clean();
        }

        // upstream: sender.c:408-455 - the parent resolve, the fd-relative
        // re-stat and the unlink are ONE decision about ONE directory entry,
        // so they live together in a free function the guard tests drive
        // directly.
        remove_confirmed_source(
            source_path,
            display_name,
            recorded,
            self.config.flags.copy_links,
        )
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
    /// entered the file list is never removed. Returns the `io_error` bits and
    /// the `FERROR_XFER` diagnostic the caller must drain.
    ///
    /// Diagnostics name the file-list entry, not the reconstructed absolute
    /// path: upstream reports `f_name(file, fname)` after `change_pathname()`
    /// has moved it to the file's own directory root, so the text a daemon
    /// module forwards to its client never carries the server's real prefix.
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:1623-1637` - `MSG_SUCCESS` receipt drives `successful_send(val)`.
    /// - `sender.c:395` - `successful_send()` unlink + guards.
    /// - `sender.c:412-414` - `change_pathname(file, NULL, 0)` then
    ///   `f_name(file, fname)` is the name every diagnostic below prints.
    pub(crate) fn confirm_source_removal(&mut self, wire_ndx: i32) -> SourceRemovalOutcome {
        if wire_ndx < 0 {
            return SourceRemovalOutcome::clean();
        }
        let flat_ndx = self.wire_to_flat_ndx(wire_ndx);
        if flat_ndx >= self.file_list.len() {
            return SourceRemovalOutcome::clean();
        }
        if !self.pending_source_removals.confirm(flat_ndx) {
            return SourceRemovalOutcome::clean();
        }
        let source_path = self.reconstruct_source_path(flat_ndx);
        let entry = &self.file_list[flat_ndx];
        let display_name = entry.path().to_path_buf();
        let recorded = RecordedSourceIdentity {
            size: entry.size(),
            mtime: entry.mtime(),
            mtime_nsec: entry.mtime_nsec(),
        };
        self.remove_source_file_if_requested(&source_path, &display_name, recorded)
    }

    /// Runs the deferred `--remove-source-files` unlink for every confirmation
    /// in `confirmed`, routing each failure through `emit_sender_diagnostic`.
    ///
    /// Returns how many confirmations were drained, which is what lets a test
    /// assert that a given drain phase had real work to do rather than passing
    /// on an empty set.
    ///
    /// Upstream has no batched drain to mirror: `read_a_msg()` calls
    /// `successful_send(val)` the instant a `MSG_SUCCESS` frame is demultiplexed
    /// (`io.c:1793-1807`), so the unlink and its `FERROR_XFER` happen wherever
    /// the sender happened to be doing I/O - during `send_files()` and again
    /// inside `read_final_goodbye()`. Our reader accumulates the indices instead
    /// of dispatching them, so the eagerness upstream gets for free has to be
    /// reproduced by calling this at each point upstream could have reacted.
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:1793-1807` - `MSG_SUCCESS` dispatches `successful_send(val)` inline
    /// - `sender.c:395` - `successful_send()` performs the guarded unlink
    /// - `log.c:337-338`, `log.c:357-367` - `FERROR_XFER` sets `got_xfer_error`
    ///   and, on a server, forwards the text as `MSG_ERROR_XFER`
    pub(crate) fn drain_confirmed_source_removals<W: Write>(
        &mut self,
        writer: &mut crate::writer::ServerWriter<W>,
        confirmed: Vec<i32>,
    ) -> io::Result<usize> {
        let drained = confirmed.len();
        for wire_ndx in confirmed {
            let outcome = self.confirm_source_removal(wire_ndx);
            self.io_error |= outcome.io_error;
            if let Some(text) = outcome.error_xfer {
                self.emit_sender_diagnostic(
                    writer,
                    super::super::protocol_io::SenderDiagnostic::ErrorXfer,
                    &text,
                )?;
            }
        }
        Ok(drained)
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
    /// redo-pass resend (upstream `file->flags & FLAG_FILE_SENT`, sender.c:610).
    fn is_resend(&self, ndx: usize) -> bool {
        self.sent.get(ndx).copied().unwrap_or(false)
    }

    /// Records that `ndx` has now been transferred, so any later request for it
    /// is a resend (upstream `file->flags |= FLAG_FILE_SENT`, sender.c:804).
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
/// upstream: `sender.c:442` compares `st.st_size` / `st.st_mtime` /
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
/// upstream: sender.c:442-451
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
/// upstream: sender.c:745 - `if (append_mode > 0 && st.st_size < F_LENGTH(file))`
fn source_diminished_below_flist(source_path: &Path, flist_len: u64) -> bool {
    std::fs::metadata(source_path).is_ok_and(|meta| meta.len() < flist_len)
}

/// Re-stats a confirmed source through a confined parent descriptor and unlinks
/// it through that same descriptor.
///
/// This is the body of upstream `successful_send()` from its parent resolve
/// down to its unlink, with upstream's three outcomes preserved:
///
/// - the walk resolves a parent (`dfd >= 0`): the re-stat is `fstatat` against
///   that descriptor and the removal is `unlinkat` against it, so the entry
///   confirmed is provably the entry removed - no path component is re-resolved
///   in between, and a directory symlink swapped in over the parent after the
///   stat cannot redirect the unlink;
/// - the walk declines (`dfd < 0` with `errno == 0`, i.e. `insecure links = yes`
///   or `--insecure-links`): the pre-3.5.0 path-based `lstat` + `unlink` pair,
///   which is what the opt-out asks for;
/// - the walk refuses (`dfd < 0` with `errno` set): `failed_op =
///   "secure-open-parent"`, reported like any other failure and never silently
///   downgraded to the path-based removal.
///
/// The re-stat compares exactly what upstream compares - size, whole-second
/// mtime, and the sub-second component when the file list carried one - and
/// nothing else. It is deliberately NOT a `(dev, ino)` comparison: the file
/// list records no inode to compare against, and upstream's one dev/ino test
/// here is the separate `local_server` guard that refuses to delete the
/// *destination* file, which reads the receiver's dev/ino off the wire and
/// lives on the local-copy side.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/sender.c:416` - `dfd = secure_sender_parent_fd(file, fname, &bname)`
/// - `rsync-3.5.0/sender.c:421-425` - the `secure-open-parent` failure arm
/// - `rsync-3.5.0/sender.c:426-428` - `do_stat_atfd` / `do_lstat_atfd` on `dfd`
/// - `rsync-3.5.0/sender.c:442-451` - the size/mtime changed-file guard
/// - `rsync-3.5.0/sender.c:453` - `secure_remove_source_file(dfd, bname)`
/// - `rsync-3.5.0/sender.c:201-203` - that function, `do_unlink_atfd(dfd, bname, 0)`
/// - `rsync-3.5.0/sender.c:455-459` - the shared `failed:` label, where `ENOENT`
///   from ANY of the three operations is the benign "already removed" notice
#[cfg(unix)]
fn remove_confirmed_source(
    source_path: &Path,
    display_name: &Path,
    recorded: RecordedSourceIdentity,
    copy_links: bool,
) -> SourceRemovalOutcome {
    use std::os::fd::AsFd as _;

    // upstream: sender.c:416 - resolve the parent ONCE. Both the re-stat and
    // the unlink below run against this one descriptor.
    let parent = match fast_io::ConfinedFallback::confined().parent_at(source_path) {
        Ok(parent) => parent,
        // upstream: sender.c:421-425 + 455-459 - a refused parent is
        // failed_op = "secure-open-parent", which shares the `failed:` label,
        // and therefore the ENOENT-is-benign arm, with the other two failures.
        Err(error) => return report_removal_failure("secure-open-parent", display_name, &error),
    };

    // upstream: sender.c:426-428 - do_stat_atfd under --copy-links, else
    // do_lstat_atfd; the path-based pair only on the declined arm.
    let restat = match &parent {
        Some((dirfd, leaf)) => {
            let at = if copy_links {
                fast_io::fstatat_follow(dirfd.as_fd(), leaf)
            } else {
                fast_io::fstatat_nofollow(dirfd.as_fd(), leaf)
            };
            at.map(|meta| (meta.size(), meta.mtime(), meta.mtime_nsec() as u32))
        }
        None => if copy_links {
            std::fs::metadata(source_path)
        } else {
            std::fs::symlink_metadata(source_path)
        }
        .map(|meta| stat_identity(&meta)),
    };
    let (size, mtime, mtime_nsec) = match restat {
        Ok(identity) => identity,
        // upstream: sender.c:429-430,455-459 - ENOENT is the benign FINFO
        // notice, anything else is rsyserr(FERROR_XFER) -> exit 23.
        Err(error) => return report_removal_failure("re-lstat", display_name, &error),
    };

    // upstream: sender.c:442-451 - refuse to remove a source that changed size
    // or modification time since it entered the file list. Upstream reports
    // this at FERROR_XFER, exactly like the three failed_op arms.
    if source_changed_since_flist(recorded, size, mtime, mtime_nsec) {
        return SourceRemovalOutcome::error_xfer(format!(
            "ERROR: Skipping sender remove for changed file: {}\n",
            display_name.display()
        ));
    }

    // upstream: sender.c:453 - secure_remove_source_file(dfd, bname) through the
    // very descriptor the re-stat used, or do_unlink(fname) on the declined arm.
    let removal = match &parent {
        Some((dirfd, leaf)) => fast_io::unlinkat(dirfd.as_fd(), leaf, fast_io::UnlinkFlags::File),
        None => fast_io::unlink_path(source_path, fast_io::UnlinkFlags::File),
    };
    match removal {
        Ok(()) => {
            // upstream: sender.c:461-462 - INFO_GTE(REMOVE,1) success notice.
            info_log!(Remove, 1, "removing source {}", display_name.display());
            SourceRemovalOutcome::clean()
        }
        Err(error) => report_removal_failure("remove", display_name, &error),
    }
}

/// Windows has neither the `*at` family nor the ownership walk, so the removal
/// stays the path-based pair upstream itself uses on its declined arm.
#[cfg(not(unix))]
fn remove_confirmed_source(
    source_path: &Path,
    display_name: &Path,
    recorded: RecordedSourceIdentity,
    copy_links: bool,
) -> SourceRemovalOutcome {
    let restat = if copy_links {
        std::fs::metadata(source_path)
    } else {
        std::fs::symlink_metadata(source_path)
    };
    let current = match restat {
        Ok(meta) => meta,
        Err(error) => return report_removal_failure("re-lstat", display_name, &error),
    };

    let (size, mtime, mtime_nsec) = stat_identity(&current);
    if source_changed_since_flist(recorded, size, mtime, mtime_nsec) {
        return SourceRemovalOutcome::error_xfer(format!(
            "ERROR: Skipping sender remove for changed file: {}\n",
            display_name.display()
        ));
    }

    match std::fs::remove_file(source_path) {
        Ok(()) => {
            info_log!(Remove, 1, "removing source {}", display_name.display());
            SourceRemovalOutcome::clean()
        }
        Err(error) => report_removal_failure("remove", display_name, &error),
    }
}

/// Upstream's shared `failed:` label: `ENOENT` from the parent resolve, the
/// re-stat or the unlink alike is the benign "already removed" notice and no
/// error bit; anything else is `rsyserr(FERROR_XFER, ...)`, which upstream
/// turns into `got_xfer_error` -> exit 23.
///
/// upstream: `rsync-3.5.0/sender.c:455-459`
fn report_removal_failure(
    op: &str,
    display_name: &Path,
    error: &io::Error,
) -> SourceRemovalOutcome {
    if error.kind() == io::ErrorKind::NotFound {
        info_log!(
            Remove,
            1,
            "sender file already removed: {}",
            display_name.display()
        );
        return SourceRemovalOutcome::clean();
    }
    SourceRemovalOutcome::error_xfer(format!("{}\n", sender_op_failure(op, display_name, error)))
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
mod sender_fstat_error_routing_tests {
    //! The sender fstats the just-opened source fd; upstream `sender.c`
    //! `do_fstat` exits `exit_cleanup(RERR_FILEIO)` (exit 11) when that fstat
    //! fails - a fatal abort, NOT the per-file `MSG_NO_SEND` skip used for an
    //! *open* failure. These tests pin the classification the transfer loop
    //! relies on to route the abort fatally with the correct exit code.
    use super::{is_protocol_violation, is_sender_fstat_error};
    use crate::generator::sender_fstat_error;
    use std::io;

    #[test]
    fn fstat_failure_is_routed_to_a_fatal_return() {
        // WHY: a regression that dropped the tag would send this error down the
        // `record_open_failure` skip arm (MSG_NO_SEND, at most exit 23) instead
        // of aborting - diverging from upstream's fatal RERR_FILEIO exit.
        let tagged = sender_fstat_error(&io::Error::from_raw_os_error(9)); // EBADF
        assert!(
            is_sender_fstat_error(&tagged),
            "an fstat failure must be recognised so the loop aborts, not skips"
        );
    }

    #[test]
    fn fstat_failure_pins_the_fileio_exit_code() {
        // WHY: observable exit-code fidelity. The tagged error carries
        // ErrorKind::Other and no ProtocolViolation tag, which is exactly what
        // the core exit-code mapper needs to yield RERR_FILEIO (11): the raw
        // errno alone (e.g. EACCES) would otherwise map to RERR_FILESELECT (3),
        // and a ProtocolViolation tag would map to RERR_PROTOCOL (2).
        let tagged = sender_fstat_error(&io::Error::from_raw_os_error(13)); // EACCES
        assert_eq!(tagged.kind(), io::ErrorKind::Other);
        assert!(
            !is_protocol_violation(&tagged),
            "the fstat abort maps to RERR_FILEIO, never RERR_PROTOCOL"
        );
    }

    #[test]
    fn ordinary_open_failure_is_not_treated_as_an_fstat_abort() {
        // WHY: an open() failure (vanished/permission) must keep the
        // MSG_NO_SEND skip semantics; only a genuine fstat failure aborts.
        let open_err = io::Error::from_raw_os_error(2); // ENOENT
        assert!(!is_sender_fstat_error(&open_err));
    }

    #[test]
    fn device_guard_abort_is_not_treated_as_an_fstat_abort() {
        // WHY: the device-guard abort (RERR_PROTOCOL) and the fstat abort
        // (RERR_FILEIO) both return fatally but must stay distinct so each keeps
        // its own exit code; the classifiers must not cross-detect.
        let protocol_err = protocol::protocol_violation("attempt to copy device contents");
        assert!(!is_sender_fstat_error(&protocol_err));
        assert!(is_protocol_violation(&protocol_err));
    }

    #[test]
    fn diagnostic_carries_the_sender_role_trailer() {
        // WHY: the fatal error surfaces to the client; it must mirror upstream's
        // "fstat failed" wording and carry oc's [sender] role trailer.
        let tagged = sender_fstat_error(&io::Error::from_raw_os_error(9));
        let text = tagged.to_string();
        assert!(text.contains("fstat failed"), "unexpected message: {text}");
        assert!(text.contains("[sender"), "missing role trailer: {text}");
    }
}

#[cfg(test)]
mod sender_remove_guard_tests {
    use super::{
        RecordedSourceIdentity, sender_op_failure, source_changed_since_flist, stat_identity,
    };
    use std::io;
    use std::path::Path;

    #[test]
    fn re_lstat_failure_leaves_the_path_unquoted() {
        // Output fidelity: upstream sender.c:459 emits the path with a bare
        // %s, never full_fname(), so the diagnostic carries no surrounding
        // quotes. This fails on the pre-fix code that wrapped the path in `"`.
        let error = io::Error::from_raw_os_error(13);
        let msg = sender_op_failure("re-lstat", Path::new("src/data.bin"), &error);
        assert!(
            msg.starts_with("rsync: [sender] sender failed to re-lstat src/data.bin: "),
            "unexpected diagnostic: {msg}"
        );
        assert!(!msg.contains('"'), "path must not be quoted: {msg}");
    }

    #[test]
    fn remove_failure_leaves_the_path_unquoted() {
        // Same bare-%s contract as re-lstat: the "remove" op word is the only
        // difference upstream, and the path is still emitted unquoted.
        let error = io::Error::from_raw_os_error(13);
        let msg = sender_op_failure("remove", Path::new("src/data.bin"), &error);
        assert!(
            msg.starts_with("rsync: [sender] sender failed to remove src/data.bin: "),
            "unexpected diagnostic: {msg}"
        );
        assert!(!msg.contains('"'), "path must not be quoted: {msg}");
    }

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
        // removing it now would destroy data we never sent (sender.c:442).
        let r = recorded(1024, 1_700_000_000, 0);
        assert!(source_changed_since_flist(r, 2048, 1_700_000_000, 0));
    }

    #[test]
    fn retouched_source_is_not_removed() {
        // Data safety: same size but a newer mtime means the file was rewritten
        // in place; upstream refuses the remove (sender.c:442 st_mtime compare).
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

/// The `--remove-source-files` unlink must land on the entry the re-stat
/// confirmed, and must not be steerable out of the confinement root by a
/// directory symlink planted above the leaf.
///
/// These cells install a process-global confinement session
/// (`fast_io::confinement::install_session`), which is sound only because
/// nextest runs one process per test.
///
/// # Why the fixture uses a symlink the test itself owns
///
/// The ownership walk deliberately FOLLOWS a symlink owned by root or by our
/// own euid - that is the operator's own layout, and refusing it would break
/// every ordinary deployment. So a non-root fixture cannot make the walk refuse
/// on ownership grounds, and a cell built that way would be inert. What it can
/// exercise is the other half of the same walk: the confinement-root judgement,
/// which asks where the leaf LANDS and does not care who owns the link. That is
/// exactly upstream's daemon arm, where `secure_sender_parent_fd()` anchors at
/// `module_dir` so a parent flipped to point outside the module cannot be
/// walked through.
///
/// upstream: `rsync-3.5.0/sender.c:395` `successful_send()`
#[cfg(all(test, unix))]
mod sender_remove_confinement_tests {
    use super::{RecordedSourceIdentity, remove_confirmed_source, stat_identity};
    use fast_io::confinement::{
        Activation, DaemonState, LocalInsecureLinks, Role, install_session,
    };
    use std::path::Path;

    /// Publish `root` as the session's confinement root, the way a daemon
    /// publishes its module root before serving a request.
    fn confine_to(root: &Path) {
        install_session(&Activation {
            role: Role::Receiver,
            daemon: DaemonState::NotDaemon,
            insecure_links: LocalInsecureLinks::default(),
            confine_root: Some(root.to_path_buf()),
        });
    }

    /// The identity the file list would have recorded for `path`.
    fn recorded_for(path: &Path) -> RecordedSourceIdentity {
        let meta = std::fs::symlink_metadata(path).expect("stat the source");
        let (size, mtime, mtime_nsec) = stat_identity(&meta);
        RecordedSourceIdentity {
            size,
            mtime,
            mtime_nsec,
        }
    }

    /// A parent component flipped to point outside the confinement root must
    /// not be walked through, so the file it leads to is NOT the file removed.
    ///
    /// WHY this matters and not merely "an unlink failed": the sender decided
    /// to delete an entry inside the module. Resolving that decision through
    /// `AT_FDCWD` on the path string re-walks every component at unlink time,
    /// so whoever controls a parent directory name controls which inode the
    /// deletion lands on - here, a file the transfer never touched and that
    /// lives outside the served module entirely. The pre-fix code removes it.
    #[test]
    fn a_parent_symlink_out_of_the_root_cannot_steer_the_removal() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("module");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).expect("mkdir module");
        std::fs::create_dir(&outside).expect("mkdir outside");
        let victim = outside.join("data.bin");
        std::fs::write(&victim, b"not ours to delete").expect("write the victim");
        // The parent the sender's path names, pointing out of the module.
        std::os::unix::fs::symlink("../outside", root.join("sub")).expect("plant the symlink");
        let spelled = root.join("sub").join("data.bin");
        // Recorded identity MATCHES, so the changed-file guard cannot be what
        // saves the victim - only the confined resolve can.
        let recorded = recorded_for(&spelled);

        confine_to(&root);
        let outcome = remove_confirmed_source(&spelled, &spelled, recorded, false);

        assert!(
            victim.exists(),
            "the out-of-root file was removed: the unlink followed the planted parent symlink"
        );
        assert_ne!(
            outcome.io_error, 0,
            "a refused parent is upstream's secure-open-parent failure (FERROR_XFER -> exit 23)"
        );
        assert!(
            outcome
                .error_xfer
                .as_deref()
                .is_some_and(|text| text.contains("secure-open-parent")),
            "the refusal must carry upstream's failed_op text so the caller can \
             emit it at FERROR_XFER (sender.c:421-425,455-459)"
        );
    }

    /// The companion that keeps the cell above honest: an ordinary source
    /// inside the root is still removed, and reports no error.
    ///
    /// Without this, "confine everything" and "refuse everything" would look
    /// identical - and refusing every removal would silently turn
    /// `--remove-source-files` into a no-op.
    #[test]
    fn an_ordinary_in_root_source_is_still_removed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("module");
        std::fs::create_dir_all(root.join("sub")).expect("mkdir module/sub");
        let source = root.join("sub").join("data.bin");
        std::fs::write(&source, b"sent and confirmed").expect("write the source");
        let recorded = recorded_for(&source);

        confine_to(&root);
        let outcome = remove_confirmed_source(&source, &source, recorded, false);

        assert_eq!(
            outcome.io_error, 0,
            "an in-root removal must report no error"
        );
        assert!(
            outcome.error_xfer.is_none(),
            "a clean removal must not queue an FERROR_XFER diagnostic"
        );
        assert!(!source.exists(), "the confirmed source was not removed");
    }

    /// The fd-relative re-stat must still be a re-stat: a source rewritten
    /// since it entered the file list is kept, exactly as on the path-based
    /// arm (sender.c:442-451). A stat that compared nothing would delete it.
    #[test]
    fn a_source_that_changed_since_the_flist_is_kept() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("module");
        std::fs::create_dir_all(root.join("sub")).expect("mkdir module/sub");
        let source = root.join("sub").join("data.bin");
        std::fs::write(&source, b"short").expect("write the source");
        let mut recorded = recorded_for(&source);
        // The file list saw a longer file than the one on disk now.
        recorded.size += 4096;

        confine_to(&root);
        let outcome = remove_confirmed_source(&source, &source, recorded, false);

        assert_ne!(
            outcome.io_error, 0,
            "a changed source is FERROR_XFER -> exit 23"
        );
        assert!(
            outcome
                .error_xfer
                .as_deref()
                .is_some_and(|text| text.contains("Skipping sender remove for changed file")),
            "upstream reports the changed-file refusal at FERROR_XFER, not on \
             local stderr (sender.c:442-451)"
        );
        assert!(source.exists(), "a changed source must not be removed");
    }

    /// A source that vanished before the confirmation arrived is upstream's
    /// benign "already removed" notice, not an error bit - the shared `failed:`
    /// label folds `ENOENT` from the parent resolve and the re-stat alike into
    /// the same `FINFO` (sender.c:455-458).
    #[test]
    fn a_vanished_source_is_not_an_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("module");
        std::fs::create_dir_all(root.join("sub")).expect("mkdir module/sub");
        let source = root.join("sub").join("data.bin");
        std::fs::write(&source, b"transient").expect("write the source");
        let recorded = recorded_for(&source);
        std::fs::remove_file(&source).expect("the source vanishes");

        confine_to(&root);

        let outcome = remove_confirmed_source(&source, &source, recorded, false);
        assert_eq!(
            outcome.io_error, 0,
            "ENOENT is the benign FINFO notice, never an error bit"
        );
        assert!(
            outcome.error_xfer.is_none(),
            "the benign already-removed notice is FINFO, never FERROR_XFER"
        );
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

    // The crafted-wire tests feed a plain in-memory cursor, which has no
    // peekable frame buffer: it keeps the conservative pre-read flush (the
    // trait default), so these tests exercise the unchanged flush cadence.
    impl crate::reader::BufferedInputHint for Cursor<Vec<u8>> {}

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

    /// Every negative NDX the sender does not itself define is a protocol
    /// violation, not a marker to skip.
    ///
    /// upstream: rsync.c:343-353 - the `if (!inc_recurse || am_sender)` gate
    /// runs BEFORE the NDX_FLIST_EOF branch (:354) and the sub-list branch
    /// (:360), so a sender rejects all three with
    /// `exit_cleanup(RERR_PROTOCOL)`. Only a receiver inside the INC_RECURSE
    /// window may grow its file list from the stream.
    ///
    /// Each case previously logged at debug level and `continue`d, so a peer
    /// could desync this loop with no diagnostic and no exit code. The whole
    /// suite passed either way, which is why the divergence survived.
    fn assert_sender_rejects_negative_ndx(marker: i32, case: &str) {
        let (_dir, mut ctx) = generator_with_one_file();

        let mut ndx = MonotonicNdxWriter::new(32);
        let mut wire = Vec::new();
        ndx.write_ndx(&mut wire, marker).expect("write marker");

        let err = drive(&mut ctx, wire).expect_err(case);
        let rendered = err.to_string();
        assert!(
            rendered.contains(&format!("Invalid file index: {marker}")),
            "{case}: expected upstream's rejection text, got {rendered}"
        );
    }

    #[test]
    fn sender_rejects_flist_eof_as_protocol_violation() {
        assert_sender_rejects_negative_ndx(
            protocol::codec::NDX_FLIST_EOF,
            "NDX_FLIST_EOF reaches a sender only from a broken peer",
        );
    }

    #[test]
    fn sender_rejects_sublist_index_as_protocol_violation() {
        // A directory sub-list request: upstream's dir_ndx encoding, which only
        // an INC_RECURSE receiver may consume.
        assert_sender_rejects_negative_ndx(
            protocol::codec::NDX_FLIST_OFFSET - 3,
            "a sub-list index reaches a sender only from a broken peer",
        );
    }

    #[test]
    fn sender_rejects_unknown_negative_ndx_as_protocol_violation() {
        assert_sender_rejects_negative_ndx(-42, "unknown negative NDX");
    }

    /// Non-vacuity companion: the rejection must not swallow the one negative
    /// marker upstream DOES handle above the gate (rsync.c:337-342), or the
    /// fix would turn a legitimate frame into a fatal.
    #[test]
    fn sender_still_accepts_del_stats_then_done() {
        let (_dir, mut ctx) = generator_with_one_file();

        let mut ndx = MonotonicNdxWriter::new(32);
        let mut wire = Vec::new();
        ndx.write_ndx(&mut wire, protocol::codec::NDX_DEL_STATS)
            .expect("write del stats marker");
        protocol::DeleteStats {
            files: 2,
            dirs: 1,
            symlinks: 0,
            devices: 0,
            specials: 0,
        }
        .write_to(&mut wire)
        .expect("write del stats");
        ndx.write_ndx_done(&mut wire).expect("write done");
        ndx.write_ndx_done(&mut wire).expect("write done");

        drive(&mut ctx, wire).expect("NDX_DEL_STATS is handled above the gate");
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

#[cfg(test)]
mod inc_recurse_lookahead_tests {
    //! Sub-list pacing guard for the INC_RECURSE sender.
    //!
    //! upstream: `sender.c:231,265` call `send_extra_file_list(f_out,
    //! MIN_FILECNT_LOOKAHEAD)`, whose loop head
    //! (`flist.c:2139 while (file_total - file_old_total < at_least)`) is
    //! re-tested on every iteration. The sender therefore keeps roughly 1000
    //! entries queued ahead of the sub-list the receiver is working through and
    //! stops - it never pushes the whole tree up front.
    //!
    //! The WHY this pins: pacing is invisible in an oc-to-oc run, because both
    //! halves are eager and neither ever blocks the other. It is only observable
    //! against a peer that throttles, where an unbounded up-front burst fills the
    //! socket buffer before the receiver has drained it. So the guard has to be
    //! an ordering assertion - how many sub-lists reached the wire *before* the
    //! sender first blocked for a receiver NDX - not an assertion on the final
    //! wire contents, which are identical either way.

    use std::cell::Cell;
    use std::io::{self, Cursor, Read};
    use std::path::Path;
    use std::rc::Rc;
    use std::sync::Arc;

    use protocol::codec::{MonotonicNdxWriter, NdxCodec};
    use protocol::flist::FileEntry;
    use protocol::{CompatibilityFlags, ProtocolVersion};

    use crate::config::ServerConfig;
    use crate::generator::segments::MIN_FILECNT_LOOKAHEAD;
    use crate::generator::{GeneratorContext, segment_dispatch_totals};
    use crate::handshake::HandshakeResult;
    use crate::role::ServerRole;
    use crate::writer::ServerWriter;

    /// Sub-directories in the fixture; one INC_RECURSE sub-list each.
    const DIRS: usize = 30;
    /// Files per sub-directory, so each sub-list carries exactly this many
    /// entries. Chosen to divide `MIN_FILECNT_LOOKAHEAD` evenly, making the
    /// expected first burst an exact count rather than a range.
    const FILES_PER_DIR: usize = 100;
    /// Sub-lists the sender must send before it first blocks on the receiver:
    /// enough to reach `MIN_FILECNT_LOOKAHEAD` queued entries, and no more.
    const EXPECTED_FIRST_BURST: usize = MIN_FILECNT_LOOKAHEAD / FILES_PER_DIR;

    /// Wire reader that records the sub-list dispatch count at the moment the
    /// sender first blocks for a receiver NDX.
    struct FirstReadProbe {
        wire: Cursor<Vec<u8>>,
        dispatched_at_first_read: Rc<Cell<Option<u64>>>,
        /// What the probe answers to `has_buffered_input()`, i.e. whether the
        /// sender believes its next read can be served without blocking.
        ///
        /// This is the signal the sender reads as "am I idle?" (upstream's
        /// `poll()` returning 0, io.c:851). Setting it decides which of the two
        /// lookahead mechanisms the fixture exercises: `true` models a receiver
        /// keeping pace, so only the `MIN_FILECNT_LOOKAHEAD` floor runs and the
        /// burst is the floor's alone; `false` models a sender with time on its
        /// hands, which is when upstream grows the window past the floor.
        buffered_input: bool,
    }

    impl Read for FirstReadProbe {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.dispatched_at_first_read.get().is_none() {
                self.dispatched_at_first_read
                    .set(Some(segment_dispatch_totals().0));
            }
            self.wire.read(buf)
        }
    }

    impl crate::reader::BufferedInputHint for FirstReadProbe {
        fn has_buffered_input(&self) -> bool {
            self.buffered_input
        }
    }

    /// Runs the sender over the fixture tree and reports
    /// `(sub-lists dispatched before the first receiver read, total dispatched)`.
    fn run_and_count_first_burst(buffered_input: bool) -> (usize, usize) {
        // The receiver acknowledges each completed file-list and then closes out
        // the three phases; it never requests a transfer, so every read the
        // sender performs is a genuine block on receiver progress.
        let mut ndx = MonotonicNdxWriter::new(32);
        let mut wire = Vec::new();
        for _ in 0..(DIRS + 3) {
            ndx.write_ndx_done(&mut wire).unwrap();
        }

        let dispatched_at_first_read = Rc::new(Cell::new(None));
        let mut reader = FirstReadProbe {
            wire: Cursor::new(wire),
            dispatched_at_first_read: Rc::clone(&dispatched_at_first_read),
            buffered_input,
        };
        let mut writer = ServerWriter::new_plain(Vec::new());
        let mut progress: Option<&mut dyn crate::TransferProgressCallback> = None;
        let mut itemize: Option<&mut dyn crate::ItemizeCallback> = None;

        let mut ctx = generator_with_segmented_tree();
        let before = segment_dispatch_totals().0;
        ctx.run_transfer_loop(&mut reader, &mut writer, &mut progress, &mut itemize)
            .expect("sender loop must run to completion");
        let after = segment_dispatch_totals().0;

        let first_burst = dispatched_at_first_read
            .get()
            .expect("sender must block for a receiver NDX")
            - before;
        ((first_burst) as usize, (after - before) as usize)
    }

    /// Builds an INC_RECURSE generator over a `DIRS * FILES_PER_DIR` tree.
    ///
    /// Entries are pushed in the sorted order a real `-r` scan produces
    /// (`.`, `d00`, `d00/f000`..., `d01`, ...) so the partitioner sees the same
    /// shape it does in production.
    fn generator_with_segmented_tree() -> GeneratorContext {
        let handshake = HandshakeResult {
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: true,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: Some(CompatibilityFlags::INC_RECURSE),
            checksum_seed: 0,
        };
        let config = ServerConfig {
            role: ServerRole::Generator,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            ..Default::default()
        };
        let mut ctx = GeneratorContext::new_for_test(&handshake, config);
        assert!(ctx.inc_recurse(), "fixture must negotiate INC_RECURSE");

        let base: Arc<Path> = Arc::from(Path::new(""));
        let push = |ctx: &mut GeneratorContext, entry: FileEntry| {
            ctx.file_list.push(entry);
            ctx.source_bases.push(Arc::clone(&base));
        };
        push(&mut ctx, FileEntry::new_directory(".".into(), 0o755));
        for d in 0..DIRS {
            push(
                &mut ctx,
                FileEntry::new_directory(format!("d{d:02}").into(), 0o755),
            );
            for f in 0..FILES_PER_DIR {
                push(
                    &mut ctx,
                    FileEntry::new_file(format!("d{d:02}/f{f:03}").into(), 0, 0o644),
                );
            }
        }

        ctx.partition_file_list_for_inc_recurse();
        assert_eq!(
            ctx.incremental.pending_segments.len(),
            DIRS,
            "one pending sub-list per sub-directory"
        );
        assert!(
            ctx.incremental
                .pending_segments
                .iter()
                .all(|s| s.count == FILES_PER_DIR),
            "each sub-list must carry exactly {FILES_PER_DIR} entries"
        );
        ctx
    }

    #[test]
    fn sub_lists_are_paced_against_receiver_progress() {
        // Receiver keeping pace: its NDXs are already buffered, so the sender is
        // never idle and the MIN_FILECNT_LOOKAHEAD floor is the only mechanism
        // that runs. This isolates the floor, which is what the burst count
        // below measures.
        let (first_burst, total) = run_and_count_first_burst(true);

        // Pre-fix, the lookahead was computed once outside the dispatch loop, so
        // the very first `while` drained all DIRS sub-lists before the sender
        // ever read a byte back. Upstream re-tests the condition each iteration
        // and stops at MIN_FILECNT_LOOKAHEAD queued entries.
        assert_eq!(
            first_burst, EXPECTED_FIRST_BURST,
            "sender must queue {MIN_FILECNT_LOOKAHEAD} entries ahead and then wait, \
             not push all {DIRS} sub-lists up front"
        );
        assert_eq!(
            total, DIRS,
            "every sub-list must still reach the receiver by end of transfer"
        );
    }

    #[test]
    fn an_idle_sender_grows_the_lookahead_past_the_floor() {
        // upstream io.c:851-857 - when the poll ahead of a read finds nothing
        // ready, the sender spends the wait queueing one more sub-list
        // (`at_least = -1` resolves to `backlog + 1` at flist.c:2407) instead of
        // parking with the window pinned at the floor.
        //
        // The WHY: the floor is a floor, not a target. Stopping at it leaves the
        // sender idle with sub-lists in hand and the receiver with only
        // MIN_FILECNT_LOOKAHEAD entries to work through, however much slack the
        // link has. Measured on a 20 000-file push into an upstream 3.5.0
        // receiver, holding the window at the floor costs ~6-9% wall clock
        // against a window allowed to grow.
        //
        // This asserts the growth is exactly one sub-list per idle turn, which
        // is what upstream's `backlog + 1` buys: a mechanism that drained to the
        // ceiling in a single turn would also push past the floor and must not
        // pass.
        let (first_burst, total) = run_and_count_first_burst(false);

        assert_eq!(
            first_burst,
            EXPECTED_FIRST_BURST + 1,
            "an idle sender must add exactly one sub-list beyond the floor's \
             {EXPECTED_FIRST_BURST} before it parks on the receiver"
        );
        assert_eq!(
            total, DIRS,
            "every sub-list must still reach the receiver by end of transfer"
        );
    }
}

#[cfg(test)]
mod sender_batch_flush_tests {
    //! Daemon-sender write-batching guard (#190).
    //!
    //! upstream: io.c:640-724 `perform_io()` drains `iobuf.out` only while
    //! blocking on input; a request already buffered (io.c:643
    //! `iobuf.in.len >= needed`) returns without a flush, so the sender
    //! coalesces many per-file deltas into one socket write (~24 files/write)
    //! rather than forcing a flush per file. This pins that contract: with every
    //! request already buffered, the sender must NOT flush once per transferred
    //! file. The WHY: the extra flushes are invisible in an oc-to-oc run (both
    //! ends keep pace), and only show up as a syscall-rate regression against a
    //! real peer - so the guard has to assert flush cadence, not wire contents,
    //! which are byte-identical either way.

    use std::cell::Cell;
    use std::ffi::OsString;
    use std::io::{self, Cursor, Read, Write};
    use std::path::PathBuf;
    use std::rc::Rc;

    use protocol::ProtocolVersion;
    use protocol::codec::{MonotonicNdxWriter, NdxCodec};

    use crate::config::ServerConfig;
    use crate::generator::GeneratorContext;
    use crate::handshake::HandshakeResult;
    use crate::reader::BufferedInputHint;
    use crate::receiver::SumHead;
    use crate::role::ServerRole;
    use crate::writer::ServerWriter;

    /// `ITEM_TRANSFER` (0x8000) little-endian, the iflags for a file request.
    const ITEM_TRANSFER_LE: [u8; 2] = [0x00, 0x80];

    /// Reader modelling a peer whose entire request stream is already buffered
    /// in user space: while any wire byte remains unread the next read cannot
    /// block, so `has_buffered_input` is true and the sender may skip its
    /// pre-read flush - exactly upstream's `iobuf.in.len >= needed` fast path.
    struct FullyBufferedWire {
        cur: Cursor<Vec<u8>>,
    }

    impl Read for FullyBufferedWire {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.cur.read(buf)
        }
    }

    impl BufferedInputHint for FullyBufferedWire {
        fn has_buffered_input(&self) -> bool {
            (self.cur.position() as usize) < self.cur.get_ref().len()
        }
    }

    /// Plain sink that counts `flush()` calls. In plain mode `ServerWriter`
    /// delegates `flush` straight through, so the count is the sender's
    /// socket-write cadence.
    struct FlushCountingSink {
        flushes: Rc<Cell<usize>>,
    }

    impl Write for FlushCountingSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes.set(self.flushes.get() + 1);
            Ok(())
        }
    }

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

    /// Builds a generator over `n` small source files, so wire NDX `0..n` are
    /// valid in-range whole-file transfer requests.
    fn generator_with_files(n: usize) -> (tempfile::TempDir, GeneratorContext, Vec<PathBuf>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut paths = Vec::with_capacity(n);
        for i in 0..n {
            let file = dir.path().join(format!("f{i:03}.txt"));
            std::fs::write(&file, format!("payload-{i}")).expect("write source");
            paths.push(file);
        }
        let handshake = test_handshake();
        let config = ServerConfig {
            role: ServerRole::Generator,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            args: paths.iter().map(OsString::from).collect(),
            ..Default::default()
        };
        let mut ctx = GeneratorContext::new_for_test(&handshake, config);
        ctx.build_file_list(&paths).expect("build file list");
        (dir, ctx, paths)
    }

    #[test]
    fn buffered_requests_do_not_flush_per_file() {
        const FILES: usize = 12;
        let (_dir, mut ctx, _paths) = generator_with_files(FILES);

        // Receiver request stream: one whole-file transfer request per file
        // (NDX + ITEM_TRANSFER iflags + empty sum_head), then three NDX_DONEs to
        // drain phases 0 -> 1 -> 2 -> break.
        let mut ndx = MonotonicNdxWriter::new(32);
        let mut wire = Vec::new();
        for i in 0..FILES {
            ndx.write_ndx(&mut wire, i as i32).unwrap();
            wire.extend_from_slice(&ITEM_TRANSFER_LE);
            SumHead::empty().write(&mut wire).unwrap();
        }
        ndx.write_ndx_done(&mut wire).unwrap();
        ndx.write_ndx_done(&mut wire).unwrap();
        ndx.write_ndx_done(&mut wire).unwrap();

        let flushes = Rc::new(Cell::new(0usize));
        let mut reader = FullyBufferedWire {
            cur: Cursor::new(wire),
        };
        let mut writer = ServerWriter::new_plain(FlushCountingSink {
            flushes: Rc::clone(&flushes),
        });
        let mut progress: Option<&mut dyn crate::TransferProgressCallback> = None;
        let mut itemize: Option<&mut dyn crate::ItemizeCallback> = None;

        let result = ctx
            .run_transfer_loop(&mut reader, &mut writer, &mut progress, &mut itemize)
            .expect("sender loop completes with batched writes");

        assert_eq!(
            result.files_transferred, FILES,
            "every requested file must still transfer"
        );
        // Pre-fix the top-of-loop flush ran unconditionally, so the sender
        // flushed once per iteration: FILES transfers plus the phase-boundary
        // NDX_DONE echoes = FILES + 2 flushes. With the buffered-input gate the
        // per-file flush is skipped entirely and only the phase-boundary echoes
        // flush. The strict `< FILES` bound fails the instant a per-file flush
        // returns.
        assert!(
            flushes.get() < FILES,
            "sender flushed {} times for {FILES} files - expected batched writes, \
             not one flush per file",
            flushes.get()
        );
    }
}
