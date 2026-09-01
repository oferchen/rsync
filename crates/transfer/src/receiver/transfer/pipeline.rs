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

use logging::{debug_log, info_log};
use protocol::codec::{MonotonicNdxWriter, NdxCodec, NdxCodecEnum, create_ndx_codec};
use protocol::flist::FileEntry;

use crate::delta_apply::ChecksumVerifier;
use crate::pipeline::{PipelineConfig, PipelineState};
use crate::receiver::basis::{BasisFileConfig, find_basis_file_with_config};
use crate::receiver::{PipelineSetup, ReceiverContext};
use crate::transfer_ops::{
    RequestConfig, ResponseContext, process_file_response_streaming, send_file_request,
    send_file_request_xattr,
};

/// Per-file state the response handler needs for an in-flight request: the
/// flat file-list index, destination path, the owned entry (Stage 0 clones it
/// so the loop holds no borrow into `self.file_list`) and the generator's
/// base iflags.
type InFlightFile = (usize, PathBuf, FileEntry, u32);

/// The receiver's in-flight request window - wire state and per-file state
/// held as one unit.
///
/// [`PipelineState`] tracks the outstanding wire requests, while the response
/// handler additionally needs an [`InFlightFile`] per request. Upstream needs
/// no such pairing: its receiver is NDX-ADDRESSED, reading the index off the
/// wire and then looking the file up by it (`rsync.c:322-431`), so the two can
/// never disagree. oc's pipelined driver is instead FIFO-POSITIONAL, which
/// makes two parallel queues a standing hazard - and a live one, because an
/// entry can be dropped out of band when the sender declines a file
/// (`MSG_NO_SEND`). A single missed drop does not fail loudly; it silently
/// pairs every later response with the wrong file.
///
/// Owning push, pop and eviction here is what makes that unrepresentable at a
/// call site, rather than a rule each of the three sites has to remember.
struct InFlightRequests {
    wire: PipelineState,
    files: VecDeque<InFlightFile>,
}

impl InFlightRequests {
    fn new(config: PipelineConfig) -> Self {
        let wire = PipelineState::new(config);
        let files = VecDeque::with_capacity(wire.window_size());
        Self { wire, files }
    }

    fn push(&mut self, pending: crate::pipeline::PendingTransfer, file: InFlightFile) {
        self.wire.push(pending);
        self.files.push_back(file);
    }

    /// Removes the oldest request together with its file state.
    fn pop(&mut self) -> Option<(crate::pipeline::PendingTransfer, InFlightFile)> {
        let pending = self.wire.pop()?;
        let file = self
            .files
            .pop_front()
            .expect("in-flight queues are only ever pushed and popped together");
        Some((pending, file))
    }

    fn is_empty(&self) -> bool {
        self.wire.is_empty()
    }

    fn outstanding(&self) -> usize {
        self.wire.outstanding()
    }

    fn available_slots(&self) -> usize {
        self.wire.available_slots()
    }
}

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

/// Whether sparse writing is active for this pass.
///
/// Mirrors upstream's sparse negation on the redo pass: on entering the redo
/// after an append transfer, `recv_files` runs `if (append_mode) sparse_files =
/// -sparse_files;`, and every downstream write path gates on `sparse_files > 0`
/// (receiver.c:330,482; fileio.c:155,196). The redo rewrites the file from
/// scratch, so the append+sparse interaction no longer holds and sparse must be
/// disabled for the resend. A non-redo pass, or a redo that was not append mode,
/// keeps sparse as configured.
///
/// upstream: receiver.c:recv_files
fn sparse_enabled_for_pass(sparse: bool, append: bool, is_redo_pass: bool) -> bool {
    sparse && !(is_redo_pass && append)
}

/// Maps a diagnostic's log code to the multiplex frame a server sends for it.
///
/// `None` means the code is not peer-facing and must never be framed;
/// [`logging::drain_events_for_peer`] is expected to withhold those, and the
/// unit test below pins that the two agree. Matching every variant rather than
/// falling through on `_` is deliberate: a log code added later must be
/// classified here instead of silently inheriting a frame.
///
/// The protocol < 30 remaps upstream applies alongside this (`MSG_ERROR` ->
/// `MSG_ERROR_XFER`, `MSG_WARNING` -> `MSG_INFO`, log.c:359-363) are not
/// repeated here - they already live in the emitters, which gate on
/// `supports_generator_messages()`.
// upstream: log.c:358 rwrite() `enum msgcode msg = (enum msgcode)code;`
const fn peer_message_code(code: logging::LogCode) -> Option<protocol::MessageCode> {
    match code {
        logging::LogCode::ErrorXfer => Some(protocol::MessageCode::ErrorXfer),
        logging::LogCode::Warning => Some(protocol::MessageCode::Warning),
        // upstream: log.c:303-308 - FERROR_SOCKET and FERROR_UTF8 both
        // simplify to FERROR before the stream switch.
        logging::LogCode::Error | logging::LogCode::ErrorSocket | logging::LogCode::ErrorUtf8 => {
            Some(protocol::MessageCode::Error)
        }
        // upstream: log.c:331-333 - FLOG returns before the switch, so it is
        // never framed; it belongs to the daemon's log-file drain.
        logging::LogCode::Log => None,
        // FINFO and FCLIENT are peer-facing upstream but withheld here: see
        // `drain_events_for_peer` for why forwarding them needs the caller's
        // quiet state and would put every debug trace on the wire.
        logging::LogCode::Info | logging::LogCode::Client | logging::LogCode::None => None,
    }
}

/// Takes the thread-local diagnostics a server owes its peer, as wire messages.
///
/// Separated from the emit so it can be tested without standing up a whole
/// receiver: the pairing that matters is that whatever
/// [`logging::drain_events_for_peer`] hands back is exactly what
/// [`peer_message_code`] can classify, and that is checkable here directly.
fn drain_thread_local_peer_messages() -> Vec<(protocol::MessageCode, String)> {
    logging::drain_events_for_peer()
        .into_iter()
        .filter_map(|event| {
            let (logging::DiagnosticEvent::Info { code, message, .. }
            | logging::DiagnosticEvent::Debug { code, message, .. }) = event;
            peer_message_code(code).map(|code| (code, message))
        })
        .collect()
}

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

    /// Delivers the diagnostics the pipelined receiver queued (failed
    /// verification, per-file transfer errors) to the sink upstream's
    /// `rwrite()` selects for this process.
    ///
    /// A SERVER receiver (the far end of a push) frames each line for the
    /// client; a CLIENT receiver (a pull) owns the terminal and writes to its
    /// own stdout/stderr. Sending a frame from a client receiver is not merely
    /// a cosmetic slip: the peer is a `--server --sender` whose stdout IS the
    /// wire, so the text it renders lands in the middle of the multiplexed
    /// stream and desyncs the very phase-2 redo the warning announces.
    ///
    /// The queued text carries no trailing newline (it is asserted verbatim by
    /// the queueing unit tests); upstream's `rprintf` format strings end in
    /// `\n`, so the terminator is appended here, at the single point where the
    /// line becomes output.
    ///
    /// # Upstream Reference
    ///
    /// - `log.c:251-346` - `rwrite()`: `am_server` sends the frame and returns,
    ///   otherwise `FERROR_XFER`/`FWARNING` go to stderr and `FINFO` to stdout
    /// - `receiver.c:1088-1091` - the `failed verification` warning/error text
    fn emit_pipeline_messages<W>(
        &self,
        writer: &mut W,
        messages: Vec<(protocol::MessageCode, String)>,
    ) where
        W: crate::writer::MsgInfoSender + ?Sized,
    {
        for (code, text) in messages {
            let line = format!("{text}\n");
            // A diagnostic that cannot be delivered must not abort the
            // transfer; upstream's rwrite() likewise ignores the write result.
            let _ = match code {
                protocol::MessageCode::ErrorXfer => self.emit_error_xfer_line(writer, &line),
                protocol::MessageCode::Warning => self.emit_warning_line(writer, &line),
                protocol::MessageCode::Error => self.emit_error_line(writer, &line),
                _ => self.emit_info_line(writer, &line),
            };
        }
    }

    /// Emits everything queued for the peer this iteration, from both sources.
    ///
    /// The pipeline's own queue carries diagnostics the receiver raised about a
    /// specific file - verification failures, permission errors - pushed
    /// explicitly as it walks the batch. The thread-local diagnostic buffer
    /// carries everything raised through `warn_log!` from outside that loop,
    /// notably the destination-anchoring warning raised during setup.
    ///
    /// Upstream has no such split: `rwrite()` frames any peer-facing code from
    /// wherever it was raised (upstream: log.c:357-366). Draining only the
    /// pipeline queue is why a daemon receiver could lose its per-component
    /// path confinement without the warning that says so ever reaching the
    /// operator - the event stayed in the buffer and died with the thread.
    fn emit_peer_messages<W>(
        &self,
        writer: &mut W,
        pipelined_receiver: &mut crate::pipeline::receiver::PipelinedReceiver,
    ) where
        W: crate::writer::MsgInfoSender + ?Sized,
    {
        let mut messages = pipelined_receiver.drain_warnings();
        messages.extend(drain_thread_local_peer_messages());
        self.emit_pipeline_messages(writer, messages);
    }

    /// Runs one commit drain, flushing whatever the drain queued for the peer
    /// even when the drain itself is fatal.
    ///
    /// A bare `?` on the drain returns before `emit_peer_messages`, so the
    /// per-file diagnostic that explains the abort - `keep_backup failed: ...`
    /// for the commit-path backup that upstream answers with
    /// `exit_cleanup(RERR_FILEIO)` - dies with the loop and the operator sees
    /// only the exit line. Upstream loses nothing here: `_exit_cleanup()`
    /// reaches `io_flush(MSG_FLUSH)` before the process ends, so the `FERROR`
    /// the failed operation printed is already on its way out.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync-3.5.0/cleanup.c:242-253` - `_exit_cleanup()` flushes queued
    ///   messages on the fatal path.
    /// - `rsync-3.5.0/rsync.c:897-900` - the backup failure that makes this
    ///   path reachable.
    fn drain_or_report<W, F>(
        &self,
        writer: &mut W,
        pipelined_receiver: &mut crate::pipeline::receiver::PipelinedReceiver,
        drain: F,
    ) -> io::Result<(u64, Vec<(PathBuf, String)>)>
    where
        W: crate::writer::MsgInfoSender + ?Sized,
        F: FnOnce(
            &mut crate::pipeline::receiver::PipelinedReceiver,
        ) -> io::Result<(u64, Vec<(PathBuf, String)>)>,
    {
        let outcome = drain(pipelined_receiver);
        if outcome.is_err() {
            self.emit_peer_messages(writer, pipelined_receiver);
        }
        outcome
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
        &mut self,
        reader: &mut crate::reader::ServerReader<R>,
        writer: &mut W,
        pipeline_config: PipelineConfig,
        setup: &PipelineSetup,
        files_to_transfer: Vec<(usize, PathBuf, u32)>,
        metadata_errors: &mut Vec<(PathBuf, String)>,
        is_redo_pass: bool,
        total_files: usize,
        progress: &mut Option<&mut dyn crate::TransferProgressCallback>,
        // The connection-wide NDX diff-state codecs. Upstream io.c::write_ndx /
        // read_ndx keep a single static prev_positive/prev_negative for the whole
        // connection; the phase-2 redo pass (generator.c:2178-2216) re-requests a
        // file through that SAME state. The caller therefore threads one codec
        // pair through both the phase-1 and the redo call so the redo request's
        // positive NDX diff-encodes against the last phase-1 index, not a reset
        // base. Creating fresh codecs per pass (the pre-fix bug) reset
        // prev_positive to -1, so the daemon-sender decoded the redo NDX against
        // its running state and mis-read the file index, truncating the mux
        // stream ("multiplexed frame truncated").
        ndx_write_codec: &mut MonotonicNdxWriter,
        ndx_read_codec: &mut NdxCodecEnum,
    ) -> io::Result<PipelineResult> {
        use crate::disk_commit::{
            BackupConfig, DELAY_UPDATES_PARTIAL_DIR, DiskCommitConfig, PartialMode,
        };
        use crate::pipeline::receiver::{PipelinedReceiver, VerifyReport};
        use crate::shared::TransferDeadline;

        // upstream: generator.c:582-593 - itemize() also writes NDX + iflags
        // for entries with significant attribute diffs but no ITEM_TRANSFER,
        // interleaved with the transfer requests in flist order; the peer's
        // sender prints each row and echoes the attrs back (sender.c:292-294).
        // Merge the recorded metadata-only rows into the request stream so the
        // wire order matches upstream's single flist walk. The recording passes
        // (directories, then symlinks, then specials, then the candidate scan)
        // are each ascending but run back to back, so restore global
        // flist-index order before merging with the ascending transfer list.
        let mut no_transfer_rows =
            std::mem::take(&mut *self.server_no_transfer_itemize.borrow_mut());
        no_transfer_rows.sort_by_key(|&(idx, _)| idx);
        let files_to_transfer = if no_transfer_rows.is_empty() {
            files_to_transfer
        } else {
            let mut merged = Vec::with_capacity(files_to_transfer.len() + no_transfer_rows.len());
            let mut rows = no_transfer_rows.into_iter().peekable();
            for item in files_to_transfer {
                while let Some(&(idx, iflags)) = rows.peek().filter(|&&(idx, _)| idx < item.0) {
                    rows.next();
                    merged.push((idx, PathBuf::new(), u32::from(iflags)));
                }
                merged.push(item);
            }
            for (idx, iflags) in rows {
                merged.push((idx, PathBuf::new(), u32::from(iflags)));
            }
            merged
        };

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

        // upstream: receiver.c:recv_files - on entering the redo pass after an
        // append transfer, `if (append_mode) sparse_files = -sparse_files;`
        // negates sparse_files, and every downstream write path only enables
        // sparse when `sparse_files > 0`. The redo rewrites the file from
        // scratch, so the append+sparse interaction no longer applies; sparse
        // writing must be disabled for the resend.
        let use_sparse = sparse_enabled_for_pass(
            self.config.flags.sparse,
            self.config.flags.append,
            is_redo_pass,
        );

        let request_config = RequestConfig {
            protocol: self.protocol,
            write_iflags: self.protocol.supports_iflags(),
            checksum_length: setup.checksum_length,
            checksum_algorithm: setup.checksum_algorithm,
            negotiated_algorithms: self.negotiated_algorithms,
            compat_flags: self.compat_flags,
            checksum_seed: self.checksum_seed,
            // upstream: receiver.c:recv_files negates sparse_files on an
            // append-triggered redo (see use_sparse computation above).
            use_sparse,
            do_fsync: self.config.write.fsync,
            temp_dir: self.config.temp_dir.clone(),
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

        // Stage 0: the in-flight window OWNS its FileEntry (cloned at push,
        // bounded to the pipeline window = O(window)), so the loop no longer
        // holds a borrow into `self.file_list`.
        let mut pipeline = InFlightRequests::new(pipeline_config);
        let mut file_iter = files_to_transfer.into_iter();
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
        // upstream: cleanup.c - compute partial mode from --partial / --partial-dir flags.
        //
        // `--delay-updates` with no explicit `--partial-dir` stages through the
        // implicit `.~tmp~` beside each destination file:
        // upstream options.c:2563-2564 `if (delay_updates && !partial_dir)
        // partial_dir = tmp_partialdir;` (`static char tmp_partialdir[] = ".~tmp~"`).
        //
        // The substitution belongs here, on the receiver, rather than on the
        // option value itself: upstream keeps the implicit directory off the wire
        // by comparing the pointer against `tmp_partialdir` before forwarding
        // `--partial-dir` (options.c:3052-3055), sending only `--delay-updates`
        // so the peer re-derives `.~tmp~` for itself. oc builds its argv from
        // `ClientConfig`, which this never touches, so the same split holds
        // without threading provenance through the option value.
        let partial_mode = match (&self.config.partial_dir, self.config.write.delay_updates) {
            (Some(dir), _) => PartialMode::PartialDir(dir.clone()),
            (None, true) => PartialMode::PartialDir(PathBuf::from(DELAY_UPDATES_PARTIAL_DIR)),
            (None, false) if self.config.flags.partial => PartialMode::Partial,
            (None, false) => PartialMode::None,
        };
        let disk_config = DiskCommitConfig {
            do_fsync: self.config.write.fsync,
            // upstream: receiver.c:recv_files negates sparse_files on an
            // append-triggered redo (see use_sparse computation above).
            use_sparse,
            preallocate: self.config.flags.preallocate,
            dest_dir: Some(setup.dest_dir.clone()),
            #[cfg(unix)]
            sandbox: setup.sandbox.clone(),
            temp_dir: self.config.temp_dir.as_ref().map(PathBuf::from),
            metadata_opts: Some(setup.metadata_opts.clone()),
            // upstream: generator.c:2659 `make_backups = -make_backups`
            // negates make_backups during the redo pass so the inplace
            // pre-image copy does not overwrite the good phase-1 backup
            // with the now-corrupted destination.
            backup: if is_redo_pass { None } else { backup },
            acl_cache: setup.acl_cache.clone(),
            acl_id_map: setup.acl_id_map.clone(),
            xattr_filter: self.xattr_name_filter_arc(),
            io_uring_policy: self.config.write.io_uring_policy,
            io_uring_depth: self.config.write.io_uring_depth,
            partial_mode,
            delay_updates: self.config.write.delay_updates,
            append_verify: self.config.flags.append_verify && !is_redo_pass,
            daemon_module: self.config.connection.daemon_module.clone(),
            daemon_module_root: self.config.connection.daemon_module_root.clone(),
            ..DiskCommitConfig::default()
        };
        // upstream: receiver.c:1072,1085 - `stdout_format_has_i` and `read_batch`
        // are globals in `recv_files()`; here they travel with the mediator that
        // owns the verification result.
        let mut pipelined_receiver =
            PipelinedReceiver::new(disk_config)?.with_verify_report(VerifyReport {
                out_format_forwards_i: self.config.flags.info_flags.out_format_forwards_i,
                read_batch: self.local_replay,
            });
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
                // upstream: io.c:750 - perform_io() acts on got_kill_signal at
                // its loop boundary, never mid-frame. Same here: a shutdown
                // request between two file responses aborts the loop, and the
                // `pipelined_receiver.shutdown()` below hands the disk thread
                // its Shutdown message so the in-flight temp file is finalised
                // per --partial / --partial-dir.
                crate::shared::check_shutdown()?;
                if let Some(ref dl) = deadline {
                    if dl.is_reached() {
                        break;
                    }
                }

                // Collect a batch of files, compute signatures (potentially in
                // parallel for incremental sync), then send requests sequentially.
                {
                    use rayon::prelude::*;

                    // Stage 0: resolve each queued flist index to an owned
                    // FileEntry clone as it enters the window (bounded by
                    // available_slots <= window size), releasing the borrow on
                    // `self.file_list` before the rest of the loop body.
                    let batch: Vec<(usize, FileEntry, PathBuf, u32)> = file_iter
                        .by_ref()
                        .take(pipeline.available_slots())
                        .map(|(idx, path, iflags)| (idx, self.file_list[idx].clone(), path, iflags))
                        .collect();

                    // The phase-2 redo takes this same path: upstream re-enters
                    // the ordinary `recv_generator()` for a redo index
                    // (generator.c:2200), so the redo re-stats the destination,
                    // re-runs the basis selection, and re-sends a block
                    // signature via `generate_and_send_sums()` (generator.c:1967)
                    // - only with `csum_length = SUM_LENGTH` and `append_mode`
                    // negated (generator.c:2178,2186), both already applied by
                    // the caller and by `request_config` above. The retained
                    // in-place partial IS the redo's basis.
                    if !batch.is_empty() {
                        // Extract basis config fields for the closure to avoid
                        // capturing &self across rayon worker boundaries.
                        let fuzzy_level = self.config.flags.fuzzy_level;
                        let ref_dirs = &self.config.reference_directories;
                        let partial_dir = self.config.partial_dir.as_deref();
                        let protocol = self.protocol;
                        let compat_flags = self.compat_flags;
                        let whole_file = self.config.flags.whole_file;
                        let block_size = self.config.block_size;
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
                                .map(|(_, file_entry, file_path, base_iflags)| {
                                    // A metadata-only record transfers no data,
                                    // so no basis search is needed.
                                    if base_iflags & crate::generator::ItemFlags::ITEM_TRANSFER == 0
                                    {
                                        return crate::receiver::basis::BasisFileResult::EMPTY;
                                    }
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
                                        block_size,
                                    };
                                    find_basis_file_with_config(&basis_config)
                                })
                                .collect()
                        } else {
                            batch
                                .iter()
                                .map(|(_, file_entry, file_path, base_iflags)| {
                                    if base_iflags & crate::generator::ItemFlags::ITEM_TRANSFER == 0
                                    {
                                        return crate::receiver::basis::BasisFileResult::EMPTY;
                                    }
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
                                        block_size,
                                    };
                                    find_basis_file_with_config(&basis_config)
                                })
                                .collect()
                        };

                        // Send requests sequentially (wire order matters).
                        for ((file_idx, file_entry, file_path, base_iflags), basis_result) in
                            batch.into_iter().zip(sig_results)
                        {
                            if base_iflags & crate::generator::ItemFlags::ITEM_TRANSFER == 0 {
                                self.send_no_transfer_itemize(
                                    writer,
                                    &mut *ndx_write_codec,
                                    &mut pipeline,
                                    file_idx,
                                    file_entry,
                                    file_path,
                                    base_iflags,
                                )?;
                                continue;
                            }
                            // upstream: generator.c:569,598 - before requesting
                            // the file the generator diffs the sender's xattrs
                            // against the basis (fnamecmp) and requests every
                            // abbreviated value it cannot resolve locally. Use
                            // the same basis the delta selected so --fuzzy /
                            // --link-dest / --compare-dest / --partial-dir bases
                            // drive the resolution in upstream's priority order.
                            let xattr_request = self.build_xattr_request(
                                &file_entry,
                                basis_result.basis_path.as_deref(),
                            );
                            let pending = send_file_request_xattr(
                                writer,
                                &mut *ndx_write_codec,
                                self.flat_to_wire_ndx(file_idx),
                                file_path.clone(),
                                basis_result.signature,
                                basis_result.basis_path,
                                basis_result.fnamecmp_type,
                                basis_result.xname.as_deref(),
                                file_entry.size(),
                                base_iflags,
                                &request_config,
                                xattr_request.as_ref(),
                            )?;

                            pipeline.push(pending, (file_idx, file_path, file_entry, base_iflags));
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
                let (pending, (file_idx, file_path, file_entry, base_iflags)) =
                    pipeline.pop().expect("pipeline not empty");
                flushed_pending = flushed_pending.saturating_sub(1);

                // upstream: sender.c:292-294 - a non-transfer item is logged by
                // the sender and echoed back via write_ndx_and_attrs(); consume
                // the echo here so the response stream stays aligned with the
                // transfer replies that follow it in FIFO order.
                if base_iflags & crate::generator::ItemFlags::ITEM_TRANSFER == 0 {
                    let _ = crate::receiver::wire::SenderAttrs::read_with_codec_xattr(
                        &mut *reader,
                        &mut *ndx_read_codec,
                        request_config.preserve_xattrs,
                        request_config.want_xattr_optim,
                    )?;
                    continue;
                }

                // upstream: receiver.c:708-709 DEBUG_GTE(RECV, 1)
                debug_log!(Recv, 1, "recv_files({})", file_entry.path().display());

                let response_ctx = ResponseContext {
                    config: &request_config,
                    #[cfg(unix)]
                    sandbox: setup.sandbox.as_ref(),
                    #[cfg(unix)]
                    dest_dir: Some(setup.dest_dir.as_path()),
                };

                let xattr_list = self.resolve_xattr_list(&file_entry);
                let is_device_target = self.config.write.write_devices && file_entry.is_device();
                let response = process_file_response_streaming(
                    reader,
                    &mut *ndx_read_codec,
                    pending,
                    &response_ctx,
                    self,
                    &mut checksum_verifier,
                    pipelined_receiver.file_sender(),
                    pipelined_receiver.buf_return_rx(),
                    file_idx,
                    &file_entry,
                    is_device_target,
                    xattr_list,
                    &mut token_reader,
                );
                let result = match response {
                    Ok(result) => result,
                    Err(err) => {
                        // upstream: io.c:1809-1818 - the sender declined this
                        // file and moved on, so no response is coming. The
                        // sender has already reported why (FERROR_XFER,
                        // sender.c:719) and set io_error, which is what makes
                        // the run exit 23; the receiver simply drops the file,
                        // exactly as upstream's generator retires the entry via
                        // got_flist_entry_status(FES_NO_SEND).
                        let declined = err
                            .get_ref()
                            .and_then(|inner| {
                                inner.downcast_ref::<crate::reader::FileDeclinedError>()
                            })
                            .copied();
                        let awaited = self.flat_to_wire_ndx(file_idx);
                        match declined {
                            // The sender answers requests in NDX order and the
                            // window pops in the same order, so a decline can
                            // only ever name the file being awaited. Anything
                            // else means the two have drifted apart.
                            Some(d) if d.ndx == awaited => continue,
                            Some(d) => {
                                return Err(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    format!(
                                        "sender declined NDX {} while the receiver awaited NDX \
                                         {awaited} - request stream desynchronised",
                                        d.ndx
                                    ),
                                ));
                            }
                            None => return Err(err),
                        }
                    }
                };

                pipelined_receiver.note_commit_sent(
                    result.expected_checksum,
                    result.checksum_len,
                    file_path,
                    // upstream: receiver.c:1089 - the verification-failure line
                    // names `f_name(file, ..)`, not the joined destination path.
                    file_entry.path().clone(),
                    file_idx,
                    result.is_inplace,
                );

                // Non-blocking: collect any ready disk results.
                let (_disk_bytes, disk_meta_errors) =
                    self.drain_or_report(writer, &mut pipelined_receiver, |pr| {
                        pr.drain_ready_results()
                    })?;
                metadata_errors.extend(disk_meta_errors);

                // upstream: receiver.c:1063-1069 - a committed file (recv_ok == 1)
                // gets an immediate MSG_SUCCESS so the sender can unlink its
                // --remove-source-files source. Emit for every file the drain
                // just confirmed committed.
                self.emit_confirmed_source_removals(writer, &mut pipelined_receiver)?;

                self.emit_peer_messages(writer, &mut pipelined_receiver);

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
                        let _ = self.emit_or_record_itemize(writer, file_idx, &iflags, &file_entry);
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
            let (_disk_bytes, disk_meta_errors) =
                self.drain_or_report(writer, &mut pipelined_receiver, |pr| pr.drain_all_results())?;
            metadata_errors.extend(disk_meta_errors);

            // upstream: receiver.c:1063-1069 - flush MSG_SUCCESS for the final
            // batch of files the blocking drain just confirmed committed, so the
            // sender unlinks their --remove-source-files sources.
            self.emit_confirmed_source_removals(writer, &mut pipelined_receiver)?;

            self.emit_peer_messages(writer, &mut pipelined_receiver);

            // upstream: generator.c:2169 finish_hard_link() itemizes every
            // follower once the leader completes, before the phase-1 NDX_DONE.
            // Emit through the request-phase NDX diff-state (never the redo
            // pass, which carries no new followers) so a pushing client's
            // sender renders each `hf...` / `=> leader` row.
            if !is_redo_pass {
                #[cfg(unix)]
                self.emit_server_hardlink_follower_itemize(
                    writer,
                    ndx_write_codec.inner_mut(),
                    &setup.dest_dir,
                    setup.sandbox.as_deref(),
                )?;
                #[cfg(not(unix))]
                self.emit_server_hardlink_follower_itemize(
                    writer,
                    ndx_write_codec.inner_mut(),
                    &setup.dest_dir,
                )?;
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

    /// Writes one metadata-only itemize record (`NDX + iflags`, nothing else)
    /// and queues its pending echo behind the in-flight transfer requests.
    ///
    /// The record produces no sum head, basis byte, or xname - the framing
    /// bits were stripped at record time (`record_server_no_transfer_itemize`).
    /// The sender answers it with a bare `write_ndx_and_attrs()` echo, so a
    /// placeholder pending rides the FIFO queue and the pop side consumes the
    /// echo in order with the transfer replies.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:584-587` - `write_ndx()` + `write_shortint(iflags)`
    /// - `sender.c:292-294` - the sender logs the row, then echoes the attrs
    #[allow(clippy::too_many_arguments)]
    fn send_no_transfer_itemize<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        ndx_write_codec: &mut impl protocol::codec::NdxCodec,
        pipeline: &mut InFlightRequests,
        file_idx: usize,
        file_entry: FileEntry,
        file_path: PathBuf,
        base_iflags: u32,
    ) -> io::Result<()> {
        let wire_ndx = self.flat_to_wire_ndx(file_idx);
        ndx_write_codec.write_ndx(&mut *writer, wire_ndx)?;
        writer.write_all(&((base_iflags & 0xFFFF) as u16).to_le_bytes())?;
        pipeline.push(
            crate::pipeline::PendingTransfer::new_full_transfer(wire_ndx, file_path.clone(), 0),
            (file_idx, file_path, file_entry, base_iflags),
        );
        Ok(())
    }

    /// Dry-run transfer loop: sends NDX requests without data transfer.
    ///
    /// Mirrors upstream generator.c behavior during `!do_xfers`: sends NDX and
    /// iflags for each planned item, reads the echoed NDX+iflags from the
    /// sender. No sum head or file data is exchanged. This is what makes a push
    /// dry run print anything at all: on a push this side is the server
    /// receiver, its rows never reach the user's terminal directly, and the
    /// client sender renders them from exactly these iflags.
    ///
    /// The plan carries non-transfer items too - a new directory, a new symlink,
    /// an attribute-only regular-file change - because upstream's `itemize()`
    /// writes NDX + iflags for every entry with significant flags, not just the
    /// ones with `ITEM_TRANSFER` (generator.c:581-600), and the peer's
    /// `send_files()` echoes both kinds (sender.c:292-310 for non-transfer,
    /// sender.c:347-350 for `!do_xfers` transfers).
    ///
    /// Returns `(transfer_items, transferred_size)` - upstream bumps
    /// `stats.xferred_files` and `stats.total_transferred_size` before the
    /// `if (!do_xfers)` continue (receiver.c:781-784), so a dry run reports the
    /// same "Number of regular files transferred" as the real run would.
    ///
    /// upstream: generator.c:1858-1959 - `!do_xfers` path sends write_ndx() then
    /// goto cleanup, skipping write_sum_head(). sender.c:394-399 - `!do_xfers`
    /// logs the item and echoes write_ndx_and_attrs() without receive_sums().
    pub(in crate::receiver) fn run_dry_run_loop<
        R: Read,
        W: Write + crate::writer::MsgInfoSender + ?Sized,
    >(
        &self,
        reader: &mut crate::reader::ServerReader<R>,
        writer: &mut W,
        plan: &[super::candidates::DryRunItem<'_>],
    ) -> io::Result<(usize, u64)> {
        if plan.is_empty() {
            writer.flush()?;
            return Ok((0, 0));
        }

        let mut transfer_items = 0usize;
        let mut transferred_size = 0u64;
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
        for &(file_idx, file_entry, item_iflags) in plan {
            let needs_transfer = item_iflags & crate::generator::ItemFlags::ITEM_TRANSFER != 0;
            if needs_transfer {
                // upstream: receiver.c:783-784 - xferred_files and
                // total_transferred_size are summed before the `!do_xfers`
                // continue, so a dry run reports what the real run would move.
                transfer_items += 1;
                transferred_size += file_entry.size();
            }

            // upstream: generator.c:1938 - write_ndx(f_out, ndx)
            let wire_ndx = self.flat_to_wire_ndx(file_idx);
            ndx_write_codec.write_ndx(&mut *writer, wire_ndx)?;

            // upstream: generator.c:1937-1947 then itemize() at 581-600 - the
            // iflags shortint carries the full itemize bits computed against the
            // pre-transfer destination (ITEM_IS_NEW for an absent one, the
            // attribute diff for an existing one). Sending a bare ITEM_TRANSFER
            // here made every pushed file render as `<f.........` instead of
            // `<f+++++++++` and left the peer's created-file tally at zero.
            if write_iflags {
                writer.write_all(&((item_iflags & 0xFFFF) as u16).to_le_bytes())?;
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
            // Transfer items only: a directory's `-v` name line is produced by
            // the deferred `verbose_dir_lines` buffer, so naming it here too
            // would double it.
            if needs_transfer
                && self.config.flags.verbose
                && self.config.connection.client_mode
                && !self.should_emit_itemize()
            {
                info_log!(Name, 1, "{}", file_entry.path().display());
            }
        }

        writer.flush()?;
        Ok((transfer_items, transferred_size))
    }

    /// Receiver loop for `--only-write-batch=X` (upstream `write_batch < 0`).
    ///
    /// Unlike [`run_dry_run_loop`](Self::run_dry_run_loop), the generator sends
    /// REAL block checksums (a full sum head + signature per file), because
    /// upstream forces `dry_run = 1` only after `do_xfers` is computed
    /// (main.c:1839), so `do_xfers` stays 1 and the sender needs the checksums
    /// to build its batch. Nothing is written to the destination either way -
    /// upstream's `write_batch < 0` arm logs the item and `continue`s
    /// (receiver.c:811-817) - but where the sender's delta goes differs by
    /// direction, and so does what this loop must read back:
    ///
    /// - PUSH (this side is the remote server receiver, `am_server`): the
    ///   client sender diverted its token stream into its own batch fd
    ///   (sender.c:217 `f_xfer = write_batch < 0 ? batch_fd : f_out`), so only
    ///   the bare NDX+attrs echo from `write_ndx_and_attrs(f_out, ...)`
    ///   (sender.c:442) reaches the wire. Reading further would block forever.
    /// - PULL (this side is the local client receiver, `!am_server`): upstream
    ///   never forwards the flag to the remote sender (options.c:2850 sits in
    ///   the `am_sender` block), so that sender is an ordinary one writing sum
    ///   head + delta + file checksum onto the wire. Upstream drains it with
    ///   `discard_receive_data()` (receiver.c:813-814); skipping the read would
    ///   desync the connection - the next NDX read would parse delta bytes as a
    ///   frame header. The batch is recorded by the local tee on the read side
    ///   (io.c `write_batch_monitor_in`), so the stream must actually flow.
    ///
    /// Requests are sent one file at a time, flushing before each echo read, so
    /// there is no risk of a buffer-fill deadlock against a large signature.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:1839` - `if (write_batch < 0) dry_run = 1` (do_xfers stays 1)
    /// - `sender.c:442-443` - `write_ndx_and_attrs(f_out); write_sum_head(f_xfer)`
    /// - `receiver.c:811-817` - `write_batch < 0`: log, `if (!am_server)`
    ///   `discard_receive_data()`, no dest write
    /// - `receiver.c:524-527` - `discard_receive_data()`
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
            negotiated_algorithms: self.negotiated_algorithms,
            compat_flags: self.compat_flags,
            checksum_seed: self.checksum_seed,
            use_sparse: self.config.flags.sparse,
            do_fsync: self.config.write.fsync,
            temp_dir: self.config.temp_dir.clone(),
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

        // upstream: receiver.c:813 `if (!am_server) discard_receive_data(...)`.
        // `client_mode` is oc's `!am_server`, so only a pull drains a delta.
        let discard_sender_data = self.config.connection.client_mode;
        // upstream: token.c keeps one decompression context for the whole
        // session (the zstd DCtx must survive file boundaries), so build the
        // reader once and `reset()` it per file.
        let mut token_reader = if discard_sender_data {
            Some(request_config.create_token_reader()?)
        } else {
            None
        };
        // upstream: receiver.c:515 - `receive_data()` always trails the token
        // stream with `read_buf(f_in, sender_file_sum, xfer_sum_len)`, even on
        // the discard path where there is nothing to verify against.
        let discard_checksum_len = ChecksumVerifier::new(
            self.negotiated_algorithms.as_ref(),
            self.protocol,
            self.checksum_seed,
            self.compat_flags.as_ref(),
        )
        .digest_len();

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
                block_size: self.config.block_size,
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
            // request. On a push it reads the sum head, writes the delta into
            // its own batch fd, and echoes only NDX+attrs back to us.
            writer.flush()?;

            // upstream: sender.c:442 - write_ndx_and_attrs(f_out, ...) echo.
            let (_echoed_ndx, _sender_attrs) =
                crate::receiver::wire::SenderAttrs::read_with_codec_xattr(
                    reader,
                    &mut ndx_read_codec,
                    preserve_xattrs,
                    want_xattr_optim,
                )?;

            if let Some(token_reader) = token_reader.as_mut() {
                // upstream: sender.c:443 write_sum_head(f_xfer, s) - on a pull
                // `f_xfer == f_out`, so the sum head and the whole delta land
                // on the wire and must be consumed to keep the sender in
                // lockstep (receiver.c:813-814 discard_receive_data()).
                let _echoed_sum_head = crate::receiver::wire::SumHead::read(reader)?;
                token_reader.reset();
                crate::delta_apply::discard_delta_stream(
                    reader,
                    token_reader,
                    discard_checksum_len,
                )?;
            }

            // upstream: receiver.c:812 log_item(FCLIENT, file, iflags, NULL) -
            // the item is still logged even though nothing is written. Under
            // `-i`/`-vi` the deferred itemize row already carries the name.
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
}

#[cfg(test)]
mod tests {
    use super::sparse_enabled_for_pass;

    // upstream: receiver.c:recv_files negates sparse_files on the redo pass only
    // when append_mode was set, because the redo rewrites the file from scratch
    // and the append+sparse interaction no longer applies. These cases pin that
    // negation so a regression re-enabling sparse on an append-triggered redo
    // (which would diverge from upstream's `sparse_files > 0` write gating)
    // fails the build rather than silently producing a wrong destination file.

    #[test]
    fn append_triggered_redo_disables_sparse() {
        // --append + --sparse, redo pass: upstream negates sparse_files.
        assert!(!sparse_enabled_for_pass(true, true, true));
    }

    #[test]
    fn normal_pass_keeps_sparse_per_config() {
        // Non-redo transfer with --sparse: sparse stays enabled regardless of
        // whether --append was requested.
        assert!(sparse_enabled_for_pass(true, false, false));
        assert!(sparse_enabled_for_pass(true, true, false));
    }

    #[test]
    fn redo_without_append_keeps_sparse_per_config() {
        // A redo that was not append mode does not negate sparse_files upstream,
        // so sparse follows the configured flag.
        assert!(sparse_enabled_for_pass(true, false, true));
    }

    #[test]
    fn sparse_disabled_stays_disabled() {
        // Without --sparse there is nothing to negate in any pass.
        assert!(!sparse_enabled_for_pass(false, true, true));
        assert!(!sparse_enabled_for_pass(false, false, false));
    }
}

#[cfg(test)]
mod peer_message_tests {
    use super::{drain_thread_local_peer_messages, peer_message_code};
    use logging::{LogCode, drain_events};

    /// Every code the drain hands back must be classifiable.
    ///
    /// `drain_events_for_peer` (logging) and `peer_message_code` (here) encode
    /// the same upstream rule in two crates, and nothing in the type system
    /// ties them together - a code added to one and not the other would be
    /// silently dropped by the `filter_map`. This walks every `LogCode`
    /// variant through a real emit and asserts the pairing holds, so the two
    /// cannot drift apart unnoticed.
    #[test]
    fn everything_the_peer_drain_yields_is_classifiable() {
        let _ = drain_events();
        for code in [
            LogCode::ErrorXfer,
            LogCode::Error,
            LogCode::Warning,
            LogCode::ErrorSocket,
            LogCode::ErrorUtf8,
            LogCode::Info,
            LogCode::Client,
            LogCode::Log,
            LogCode::None,
        ] {
            logging::emit_info_coded(logging::InfoFlag::Misc, 0, code, format!("{code:?}"));
        }

        for event in logging::drain_events_for_peer() {
            let (logging::DiagnosticEvent::Info { code, .. }
            | logging::DiagnosticEvent::Debug { code, .. }) = event;
            assert!(
                peer_message_code(code).is_some(),
                "drain_events_for_peer yielded {code:?}, which peer_message_code \
                 discards - the two classifications have drifted and the \
                 diagnostic would be silently dropped"
            );
        }
        let _ = drain_events();
    }

    /// A `warn_log!` raised outside the pipeline loop reaches the wire list.
    ///
    /// This is the defect this module exists to fix: the destination-anchoring
    /// warning is emitted during receiver setup, never queued on the pipeline's
    /// own list, and before this drain existed it stayed in the thread-local
    /// buffer until the thread ended.
    #[test]
    fn a_warning_raised_outside_the_pipeline_becomes_a_peer_message() {
        let _ = drain_events();
        logging::warn_log!("receiver: could not anchor destination 'x'");

        let messages = drain_thread_local_peer_messages();

        assert_eq!(
            messages,
            vec![(
                protocol::MessageCode::Warning,
                "receiver: could not anchor destination 'x'".to_string()
            )],
            "a warn_log! outside the pipeline loop must be framed for the peer \
             as MSG_WARNING (upstream: log.c:357-366)"
        );
    }

    /// FLOG stays queued for the daemon's log-file drain.
    ///
    /// The non-vacuity companion to the test above: without it, a drain that
    /// took *everything* would also satisfy that assertion while breaking
    /// upstream's rule that FLOG never travels to the client
    /// (upstream: log.c:331-333) and stealing the events
    /// `drain_events_coded(Log)` expects to find at streams.rs.
    #[test]
    fn flog_is_left_for_the_daemon_log_drain() {
        let _ = drain_events();
        logging::emit_info_coded(
            logging::InfoFlag::Misc,
            0,
            LogCode::Log,
            "module accessed".to_string(),
        );

        assert!(
            drain_thread_local_peer_messages().is_empty(),
            "FLOG must not be framed for the client"
        );
        assert_eq!(
            logging::drain_events_coded(LogCode::Log).len(),
            1,
            "and it must still be there for the daemon's log-file drain"
        );
    }
}
