//! Top-level orchestrator that runs the generator role to completion.
//!
//! Contains `run`, which builds and sends the file list, drives the main
//! transfer loop, emits server-side stats, and performs the goodbye handshake.
//! Also emits cumulative INC_RECURSE diagnostic totals at end of transfer.
//!
//! # Upstream Reference
//!
//! - `sender.c:send_files()` - Main transfer loop
//! - `flist.c:2227` - `send_file_list()` builds and sends file list
//! - `main.c:893-924` - `read_final_goodbye()` protocol finalization

use std::io::{self, Read, Write};
use std::path::PathBuf;

use logging::{PhaseTimer, debug_log};

use super::super::GeneratorContext;
use super::super::protocol_io::calculate_duration_ms;
use crate::generator::GeneratorStats;
use crate::role_trailer::error_location;
use crate::transfer_state::TransferPhase;

impl GeneratorContext {
    /// Prints the `sending incremental file list` banner on the client's own
    /// output at file-list-send time, ahead of any per-file rows.
    ///
    /// Mirrors upstream `flist.c:2248-2252`: the banner fires only for a
    /// client-side sender (`!am_server` -> `client_mode`), under incremental
    /// recursion - which upstream disables when `!recurse` (compat.c:172-173),
    /// so a non-recursive single-file `-v` push prints nothing - and when the
    /// FLIST info category is at level >= 1, so `--info=flist0` suppresses it
    /// even at `-v`. This is the send-side twin of the receiver's `receiving
    /// incremental file list` banner in `receiver/transfer/setup/context.rs`.
    fn announce_incremental_flist(&self) -> io::Result<()> {
        if !self.should_announce_incremental_flist() {
            return Ok(());
        }
        let banner: &[u8] = b"sending incremental file list\n";
        if self.config.flags.msgs_to_stderr {
            io::stderr().write_all(banner)
        } else {
            io::stdout().write_all(banner)
        }
    }

    /// Whether this sender prints the `sending incremental file list` banner
    /// on its own client-visible output (see [`Self::announce_incremental_flist`]).
    pub(crate) fn should_announce_incremental_flist(&self) -> bool {
        self.config.connection.client_mode
            && self.config.flags.recursive
            && logging::info_gte(logging::InfoFlag::Flist, 1)
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
    /// - `flist.c:2227` - `send_file_list()` builds and sends file list
    /// - `main.c:893-924` - `read_final_goodbye()` protocol finalization
    pub fn run<R: Read, W: Write>(
        &mut self,
        mut reader: super::super::super::reader::ServerReader<R>,
        writer: &mut super::super::super::writer::ServerWriter<W>,
        paths: &[PathBuf],
        mut progress: Option<&mut dyn super::super::super::TransferProgressCallback>,
        mut itemize: Option<&mut dyn super::super::super::ItemizeCallback>,
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

        // upstream: main.c:1266-1276 - flush pending multiplex output before
        // blocking on recv_filter_list(). Upstream's perform_io() flushes the
        // output buffer while waiting for input via select(), but our separate
        // read/write streams cannot do that. Without this flush, any buffered
        // data (e.g. MSG_IO_TIMEOUT) stays unsent while we block reading the
        // client's filter list, causing a protocol ordering deadlock in daemon
        // pull mode where the client waits for server output before proceeding.
        if !self.config.connection.client_mode {
            writer.flush()?;
        }

        // upstream: main.c:1276 - recv_filter_list() in server mode
        self.receive_filter_list_if_server(&mut reader)?;

        // upstream: flist.c:2248-2252 send_file_list() - a client-side sender
        // announces `sending incremental file list` at file-list-send time,
        // before the walk produces any per-file output. Write it DIRECTLY to
        // the client stream: the deferred `info_log!` event buffer is only
        // drained by the CLI after the live per-file rows and the summary
        // stats, which would print the banner dead last on every ssh/daemon
        // push. Mirrors the receive-side banner in
        // `receiver/transfer/setup/context.rs`.
        self.announce_incremental_flist()?;

        // upstream: flist.c:2240-2264 - resolve --files-from paths if configured
        let files_from_entries = self.resolve_files_from_paths(paths, &mut reader)?;

        // FSM: filter exchange complete. Advance to FileListTransfer.
        self.pipeline
            .advance_to(TransferPhase::FileListTransfer)
            .map_err(crate::fsm_error)?;

        let reader = &mut reader;

        // upstream: flist.c:2227 - send_file_list()
        let file_count = {
            let _t = PhaseTimer::new("file-list-build-send");
            let build = if files_from_entries.is_empty() {
                self.build_file_list(paths)
            } else {
                // upstream: flist.c:2240-2244 - argv[0] is the base for --files-from
                let base_dir = paths.first().cloned().unwrap_or_else(|| PathBuf::from("."));
                self.build_file_list_with_base(&base_dir, &files_from_entries)
            };
            // upstream reports walk failures from inside send_file_list() via
            // rwrite(), which reaches f_out directly. oc has no writer during
            // the walk, so the queued lines go out here - still before the
            // first file-list byte, and before an aborted walk propagates, so
            // the client learns why either way.
            self.flush_flist_diagnostics(writer)?;
            build?;
            self.partition_file_list_for_inc_recurse();
            // Partitioning can report an entry whose parent is missing from the
            // list. Drain again so that line also precedes the first file-list
            // byte; the queue was emptied above, so this is a no-op otherwise.
            self.flush_flist_diagnostics(writer)?;
            self.send_file_list(writer)?
        };

        // upstream: acls.c:592-595 - named ACL-entry ids join the shared id-list
        // so the receiver remaps them like file owners. Runs after the file list
        // is sent (ACL cache complete) and before the id-list is transmitted.
        self.collect_acl_id_mappings();
        self.send_id_lists(writer)?;
        self.send_io_error_flag(writer)?;

        // FSM: file list sent. Advance to DeltaTransfer.
        self.pipeline
            .advance_to(TransferPhase::DeltaTransfer)
            .map_err(crate::fsm_error)?;

        // upstream: main.c:968-974 do_server_sender() -
        //   flist = send_file_list(f_out, argc, argv);
        //   if (!flist || flist->used == 0) { io_end_buffering_in(0); exit_cleanup(0); }
        //
        // An empty file list means every requested path failed to be listed
        // (missing source, unreadable directory, everything filtered out). The
        // peer's client_run() mirrors this: with an empty list it skips do_recv()
        // entirely (main.c:1379-1391), so it never sends an ndx, never sends
        // NDX_DONE, and never joins the goodbye handshake - it just reports the
        // io_error we already sent and waits in noop_io_until_death() for our FIN.
        // Entering send_files() here would block forever reading an ndx the peer
        // is never going to write. Returning now lets the caller tear the
        // connection down, which is what releases the peer.
        //
        // Server-side only, exactly as upstream: a client-mode sender (push)
        // runs send_files() unconditionally from client_run() (main.c:1343).
        if file_count == 0 && !self.config.connection.client_mode {
            // upstream: exit_cleanup(0) -> io_flush(FULL_FLUSH) before _exit.
            writer.flush_all_pending()?;
            self.pipeline
                .advance_to(TransferPhase::Finalization)
                .map_err(crate::fsm_error)?;
            self.pipeline
                .advance_to(TransferPhase::Complete)
                .map_err(crate::fsm_error)?;
            return Ok(GeneratorStats {
                files_listed: 0,
                flist_buildtime_ms: calculate_duration_ms(
                    self.timing.flist_build_start,
                    self.timing.flist_build_end,
                ),
                flist_xfertime_ms: calculate_duration_ms(
                    self.timing.flist_xfer_start,
                    self.timing.flist_xfer_end,
                ),
                flist_first_byte_latency: self.timing.flist_first_byte_latency,
                io_error: self.io_error,
                got_xfer_error: self.got_xfer_error,
                ..GeneratorStats::default()
            });
        }

        // INC_RECURSE sub-lists are sent lazily inside the loop via
        // SegmentScheduler, matching upstream sender.c:227,261 cadence.
        let transfer_result = {
            let _t = PhaseTimer::new("generator-transfer-loop");
            self.run_transfer_loop(reader, writer, &mut progress, &mut itemize)?
        };

        // FSM: delta transfer complete. Advance to Finalization.
        self.pipeline
            .advance_to(TransferPhase::Finalization)
            .map_err(crate::fsm_error)?;

        // upstream: main.c:978-980 - do_server_sender() calls io_flush then handle_stats
        // before read_final_goodbye. Server-sender writes transfer stats to the wire;
        // client-sender handle_stats(-1) puts nothing on the wire but, under
        // --write-batch, writes the same five values straight to batch_fd
        // (main.c:374-383). Both land BEFORE read_final_goodbye tees the goodbye
        // NDX_DONE, which is the order --read-batch parses them back in.
        let flist_buildtime =
            calculate_duration_ms(self.timing.flist_build_start, self.timing.flist_build_end);
        let flist_xfertime =
            calculate_duration_ms(self.timing.flist_xfer_start, self.timing.flist_xfer_end);
        if !self.config.connection.client_mode {
            self.send_stats(writer, &transfer_result, flist_buildtime, flist_xfertime)?;
        } else {
            self.record_batch_stats(&transfer_result, flist_buildtime, flist_xfertime)?;
        }

        let mut ndx_read_codec = transfer_result.ndx_read_codec;
        let mut ndx_write_codec = transfer_result.ndx_write_codec;

        // UTS-9.REOPEN (daemon-gzip-download): under `-zz` daemon pull the
        // receiver decodes the goodbye NDX_DONE through `CompressedReader`,
        // so the deflate stream must be closed before we block on its
        // reply. Run `finalize_compression()` BETWEEN our goodbye write and
        // the receiver's goodbye read; finalising AFTER `handle_goodbye`
        // returns would never happen because the read would deadlock on
        // the unterminated deflate block.
        //
        // upstream: `main.c:979-983 do_server_sender()` brackets
        // `read_final_goodbye()` with `io_flush(FULL_FLUSH)`. Upstream's
        // goodbye NDX_DONE rides through `write_buf()` (`io.c:2255`) which
        // bypasses the deflate stream entirely. Our writer-graph routes
        // it through `CompressedWriter`, so we additionally need to drive
        // `CompressedWriter::finish()` here (matching
        // `token.c:367 send_deflated_token()`'s end-of-transfer
        // `deflateEnd()` contract). `finalize_compression` downgrades the
        // writer back to multiplex mode so any trailing diagnostic frame
        // still rides out before FIN.
        //
        // Rule 12 (fail-loud): surface the flush error unless the peer has
        // already shut down. Early close during goodbye-shutdown is rare
        // and the transfer is over, so any other error is treated as a
        // real failure rather than swallowed.
        self.handle_goodbye_with_finalizer(
            reader,
            writer,
            &mut ndx_read_codec,
            &mut ndx_write_codec,
            |w| match w.finalize_compression() {
                Ok(()) => Ok(()),
                Err(e) if super::super::is_early_close_error(&e) => Ok(()),
                Err(e) => Err(e),
            },
        )?;

        // upstream: io.c:1623-1637 - MSG_SUCCESS(ndx) frames arrive interleaved
        // with the receiver's NDX requests and are demultiplexed as the sender
        // drives the transfer loop and goodbye handshake. Now that the wire is
        // drained, run the deferred --remove-source-files unlink for every file
        // the peer confirmed committed (sender.c:131-182 successful_send()). A
        // file the peer never confirmed keeps its source: an interrupted or
        // failed transfer returns via `?` above and never reaches this point,
        // so its source is intentionally left in place. This is the crash-safe
        // ordering the inline unlink violated.
        for wire_ndx in reader.take_success_indices() {
            self.io_error |= self.confirm_source_removal(wire_ndx);
        }

        // UTS-V3.A drain barrier: explicit user-space drain after
        // `handle_goodbye_with_finalizer` returns and before the writer
        // graph drops. The audit traced the cluster-A wire-cutoffs
        // (~2.25 MB on batch-mode, alt-dest, and daemon-refuse-compress;
        // ~615 KB on daemon-gzip-download) to bytes still sitting in the
        // multiplex BufWriter / codec trailer when the daemon's
        // `SO_LINGER` + `shutdown(SHUT_WR)` teardown fired.
        //
        // `flush_all_pending` is idempotent: it re-runs
        // `finalize_compression` (no-op on a Multiplex writer that has
        // already been finalised inside `handle_goodbye_with_finalizer`,
        // emits the codec trailer if any branch returned early), then
        // flushes the multiplex BufWriter so the next byte goes straight
        // to the kernel. Peer-already-closed is tolerated; every other
        // I/O error surfaces.
        //
        // Companion call: the daemon teardown invokes
        // `writer::shutdown_send_side` on the underlying TcpStream after
        // the read-drain loop completes - that drains the kernel send
        // buffer and issues the explicit `shutdown(SHUT_WR)`. The two
        // calls together replace the implicit `Drop` + `SO_LINGER`
        // hand-off with an observable two-stage barrier.
        //
        // Server-side only: client-mode keeps stdio open for the parent
        // process to own teardown. Stdio (remote-shell daemon mode) is
        // not server-side here, but the flush still benefits any buffered
        // byte that needs to reach the pipe before control returns.
        //
        // upstream: cleanup.c::handle_cleanup() brackets the sender's
        // final `io_flush(FULL_FLUSH)` with the process exit so every
        // user-space byte hits the wire before the kernel queues FIN.
        if !self.config.connection.client_mode {
            writer.flush_all_pending()?;
        }

        // Calculate timing stats for return value
        let flist_buildtime =
            calculate_duration_ms(self.timing.flist_build_start, self.timing.flist_build_end);
        let flist_xfertime =
            calculate_duration_ms(self.timing.flist_xfer_start, self.timing.flist_xfer_end);

        // INC_RECURSE diagnostic I4 (#2199): emit cumulative NDX conversion
        // call count and partition_point comparison depth. Aggregated across
        // all generator transfers in this process so operators can see how
        // hot the wire/flat conversion path is relative to file counts.
        let (ndx_calls, ndx_cmps) = super::super::ndx_convert_totals();
        debug_log!(
            Genr,
            1,
            "generator ndx_convert totals: calls={} partition_point_depth={}",
            ndx_calls,
            ndx_cmps
        );
        #[cfg(feature = "tracing")]
        ::tracing::debug!(
            target: "rsync::generator::ndx_convert",
            calls = ndx_calls,
            partition_point_depth = ndx_cmps,
            "generator ndx_convert totals"
        );

        // INC_RECURSE diagnostic I3 (#2198): emit cumulative writer.flush()
        // call count from the generator transfer hot path. Aggregated across
        // all generator transfers in this process so operators can see how
        // often the sender forces a flush relative to file counts.
        let flush_calls = super::super::flush_rate_totals();
        debug_log!(
            Send,
            1,
            "generator writer.flush totals: calls={}",
            flush_calls
        );
        #[cfg(feature = "tracing")]
        ::tracing::debug!(
            target: "rsync::generator::flush_rate",
            calls = flush_calls,
            "generator writer.flush totals"
        );

        // INC_RECURSE diagnostic I5 (#2200): emit cumulative
        // prepare_pending_acl call count and elapsed wall time. Aggregated
        // across all generator transfers in this process so operators can
        // see how often per-entry ACL prep fires per segment and what share
        // of segment-encoding time it consumes.
        let (acl_calls, acl_elapsed_ns) = super::super::prepare_acl_totals();
        debug_log!(
            Genr,
            1,
            "generator prepare_pending_acl totals: calls={} elapsed_ns={}",
            acl_calls,
            acl_elapsed_ns
        );
        #[cfg(feature = "tracing")]
        ::tracing::debug!(
            target: "rsync::generator::prepare_acl",
            calls = acl_calls,
            elapsed_ns = acl_elapsed_ns,
            "generator prepare_pending_acl totals"
        );

        // INC_RECURSE diagnostic I2 (#2197): emit cumulative
        // encode_and_send_segment dispatch count and elapsed wall time.
        // Aggregated across all generator transfers in this process so
        // operators can see how often per-directory sub-lists are flushed to
        // the wire and what share of transfer time their encoding consumes.
        let (segment_calls, segment_elapsed_ns) = super::super::segment_dispatch_totals();
        debug_log!(
            Genr,
            1,
            "generator encode_and_send_segment totals: calls={} elapsed_ns={}",
            segment_calls,
            segment_elapsed_ns
        );
        #[cfg(feature = "tracing")]
        ::tracing::debug!(
            target: "rsync::generator::segment_dispatch",
            calls = segment_calls,
            elapsed_ns = segment_elapsed_ns,
            "generator encode_and_send_segment totals"
        );

        // FSM: finalization complete. Advance to Complete.
        self.pipeline
            .advance_to(TransferPhase::Complete)
            .map_err(crate::fsm_error)?;

        // upstream: log.c:310-311 - each MSG_ERROR_XFER the peer sends sets
        // got_xfer_error on receipt; cleanup.c:217-218 then reports
        // RERR_PARTIAL. The receiver emits MSG_ERROR_XFER when it cannot open a
        // file's output (e.g. mkstemp() denied by a read-only destination dir)
        // and discards the delta. Recorded as got_xfer_error rather than an
        // io_error bit: io_error is a wire field with its own meaning for the
        // peer, and by this point it has already been sent.
        if reader.xfer_error_count() > 0 {
            self.got_xfer_error = true;
        }

        // upstream: handle_stats() reports stats.total_size (main.c:351
        // write_varlong30(f, stats.total_size, 3)), accumulated in
        // send_file_entry() as `F_LENGTH(file)` for regular files and symlinks
        // only (flist.c:690-691). Read the value tallied at send time - summing
        // `self.file_list` here is wrong because INC_RECURSE drains sent
        // segments, leaving only the final sub-list, and would also count
        // directory sizes that upstream excludes.
        let flist_send_stats = self.flist_send_stats;

        Ok(GeneratorStats {
            files_listed: file_count,
            num_dirs: flist_send_stats.num_dirs,
            num_symlinks: flist_send_stats.num_symlinks,
            num_devices: flist_send_stats.num_devices,
            num_specials: flist_send_stats.num_specials,
            files_transferred: transfer_result.files_transferred,
            transferred_file_size: transfer_result.transferred_file_size,
            bytes_sent: transfer_result.bytes_sent,
            bytes_read: self.timing.total_bytes_read,
            matched_data: transfer_result.matched_data,
            literal_data: transfer_result.literal_data,
            total_size: flist_send_stats.total_size,
            flist_buildtime_ms: flist_buildtime,
            flist_xfertime_ms: flist_xfertime,
            flist_first_byte_latency: self.timing.flist_first_byte_latency,
            delete_stats: self.delete_stats,
            created_stats: transfer_result.created_stats,
            io_error: self.io_error,
            got_xfer_error: self.got_xfer_error,
        })
    }
}
