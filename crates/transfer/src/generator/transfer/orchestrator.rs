//! Top-level orchestrator that runs the generator role to completion.
//!
//! Contains `run`, which builds and sends the file list, drives the main
//! transfer loop, emits server-side stats, and performs the goodbye handshake.
//! Also emits cumulative INC_RECURSE diagnostic totals at end of transfer.
//!
//! # Upstream Reference
//!
//! - `sender.c:send_files()` - Main transfer loop
//! - `flist.c:2192` - `send_file_list()` builds and sends file list
//! - `main.c:875-906` - `read_final_goodbye()` protocol finalization

use std::io::{self, Read, Write};
use std::path::PathBuf;

use logging::{PhaseTimer, debug_log};

use super::super::GeneratorContext;
use super::super::protocol_io::calculate_duration_ms;
use crate::generator::GeneratorStats;
use crate::role_trailer::error_location;
use crate::transfer_state::TransferPhase;

impl GeneratorContext {
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
        let files_from_entries = self.resolve_files_from_paths(paths, &mut reader)?;

        // FSM: filter exchange complete. Advance to FileListTransfer.
        self.pipeline
            .advance_to(TransferPhase::FileListTransfer)
            .map_err(crate::fsm_error)?;

        let reader = &mut reader;

        // upstream: flist.c:2192 - send_file_list()
        let file_count = {
            let _t = PhaseTimer::new("file-list-build-send");
            if files_from_entries.is_empty() {
                self.build_file_list(paths)?;
            } else {
                // upstream: flist.c:2240-2244 - argv[0] is the base for --files-from
                let base_dir = paths.first().cloned().unwrap_or_else(|| PathBuf::from("."));
                self.build_file_list_with_base(&base_dir, &files_from_entries)?;
            }
            self.partition_file_list_for_inc_recurse();
            self.send_file_list(writer)?
        };

        self.send_id_lists(writer)?;
        self.send_io_error_flag(writer)?;

        // FSM: file list sent. Advance to DeltaTransfer.
        self.pipeline
            .advance_to(TransferPhase::DeltaTransfer)
            .map_err(crate::fsm_error)?;

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

        // upstream: log.c:311 - each MSG_ERROR_XFER the peer sends sets
        // got_xfer_error on receipt; main.c:1635 then _exit(RERR_PARTIAL).
        // The receiver emits MSG_ERROR_XFER when it cannot open a file's output
        // (e.g. mkstemp() denied by a read-only destination dir) and discards
        // the delta. Fold that into io_error so this sender/generator reports
        // exit 23 instead of a false success.
        if reader.xfer_error_count() > 0 {
            self.add_io_error(super::super::io_error_flags::IOERR_GENERAL);
        }

        // upstream: handle_stats() reports stats.total_size, the sum of all
        // flist file sizes (main.c:351 write_varlong30(f, stats.total_size, 3)).
        let total_size = self.file_list.iter().map(|e| e.size()).sum();

        Ok(GeneratorStats {
            files_listed: file_count,
            files_transferred: transfer_result.files_transferred,
            bytes_sent: transfer_result.bytes_sent,
            bytes_read: self.timing.total_bytes_read,
            matched_data: transfer_result.matched_data,
            literal_data: transfer_result.literal_data,
            total_size,
            flist_buildtime_ms: flist_buildtime,
            flist_xfertime_ms: flist_xfertime,
            flist_first_byte_latency: self.timing.flist_first_byte_latency,
            delete_stats: self.delete_stats,
            io_error: self.io_error,
        })
    }
}
