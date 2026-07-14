//! Protocol phase exchange, goodbye handshake, and stats reception.
//!
//! Contains the wire-level handshake methods that manage NDX_DONE exchanges
//! between phase transitions, the goodbye sequence, and sender statistics
//! parsing. Called by `finalize_transfer` at the end of every transfer mode.
//!
//! # Upstream Reference
//!
//! - `main.c:875-906` - `read_final_goodbye()` with extended goodbye
//! - `main.c:356-384` - `handle_stats()` sends/receives statistics
//! - `io.c:read_ndx()` / `write_ndx()` - NDX wire encoding

use std::io::{self, Read, Write};

use logging::debug_log;
use protocol::CompatibilityFlags;
use protocol::codec::{
    NDX_DEL_STATS, NDX_DONE, NdxCodec, NdxCodecEnum, ProtocolCodec, create_ndx_codec,
    create_protocol_codec,
};

use crate::receiver::ReceiverContext;
use crate::receiver::stats::SenderStats;
use crate::transfer_state::TransferPhase;

impl ReceiverContext {
    /// Exchanges NDX_DONE messages for phase transitions.
    ///
    /// With INC_RECURSE, sends one NDX_DONE per segment then per extra phase.
    /// Without INC_RECURSE, alternates send/receive for each phase boundary.
    /// Always reads a final NDX_DONE from the sender after all phases complete.
    pub(in crate::receiver) fn exchange_phase_done<R: Read, W: Write + ?Sized>(
        &mut self,
        reader: &mut R,
        writer: &mut W,
        ndx_write_codec: &mut NdxCodecEnum,
        ndx_read_codec: &mut NdxCodecEnum,
    ) -> io::Result<()> {
        let inc_recurse = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::INC_RECURSE));

        let max_phase: i32 = if self.protocol.supports_multi_phase() {
            2
        } else {
            1
        };

        if inc_recurse {
            let num_segments = self.ndx_segments.len();
            for _ in 0..num_segments {
                // upstream: receiver.c:573 - flist_free(first_flist)
                // Reclaim heap data from the oldest completed segment
                // to reduce RSS before sending the per-segment NDX_DONE.
                self.reclaim_oldest_segment();
                ndx_write_codec.write_ndx_done(&mut *writer)?;
                writer.flush()?;
                self.read_expected_ndx_done(ndx_read_codec, reader, "segment completion")?;
            }

            // upstream: generator.c:2355-2357 - phase++ then "generate_files phase=%d"
            // The first `phase++` after the per-segment loop advances 0 -> 1.
            let mut phase: i32 = 1;
            // upstream: receiver.c:692-693 DEBUG_GTE(RECV, 1)
            debug_log!(Recv, 1, "recv_files phase={}", phase);
            debug_log!(Genr, 1, "generate_files phase={}", phase);

            for _phase_step in 2..=max_phase {
                ndx_write_codec.write_ndx_done(&mut *writer)?;
                writer.flush()?;
                self.read_expected_ndx_done(ndx_read_codec, reader, "phase transition")?;
                // upstream: generator.c:2366-2368 - phase++ on each additional
                // iteration (covers the redo phase).
                phase += 1;
                // upstream: receiver.c:692-693 DEBUG_GTE(RECV, 1)
                debug_log!(Recv, 1, "recv_files phase={}", phase);
                debug_log!(Genr, 1, "generate_files phase={}", phase);
            }

            ndx_write_codec.write_ndx_done(&mut *writer)?;
            writer.flush()?;

            // upstream: generator.c:2391-2394 - protocol >= 29 emits a final
            // phase=3 for the delay-updates pass. INC_RECURSE implies
            // protocol >= 30, so this always fires once on this branch.
            if self.protocol.supports_multi_phase() {
                phase += 1;
                debug_log!(Genr, 1, "generate_files phase={}", phase);
            }
        } else {
            let mut phase: i32 = 0;
            loop {
                ndx_write_codec.write_ndx_done(&mut *writer)?;
                writer.flush()?;
                phase += 1;
                // upstream: receiver.c:692-693 DEBUG_GTE(RECV, 1)
                debug_log!(Recv, 1, "recv_files phase={}", phase);
                // upstream: generator.c:2355-2357, 2366-2368, 2392-2394
                // "generate_files phase=%d" emitted after each phase++.
                debug_log!(Genr, 1, "generate_files phase={}", phase);
                if phase > max_phase {
                    break;
                }
                self.read_expected_ndx_done(ndx_read_codec, reader, "phase transition")?;
            }
        }

        self.read_expected_ndx_done(ndx_read_codec, reader, "sender final")?;

        Ok(())
    }

    /// Reads an NDX and validates it is NDX_DONE (-1).
    pub(in crate::receiver) fn read_expected_ndx_done<R: Read>(
        &self,
        ndx_read_codec: &mut NdxCodecEnum,
        reader: &mut R,
        context: &str,
    ) -> io::Result<()> {
        let ndx = ndx_read_codec.read_ndx(reader)?;
        if ndx != -1 {
            // upstream: io.c read_ndx / rsync.c:818 - a wire index that is not
            // the expected NDX_DONE (-1) is "File-list index N not in ..." and
            // aborts with exit_cleanup(RERR_PROTOCOL) (exit 2). Tag the error so
            // the core exit-code mapper yields RERR_PROTOCOL, not RERR_STREAMIO.
            return Err(protocol::protocol_violation(format!(
                "expected NDX_DONE (-1) from sender during {context}, got {ndx} {}{}",
                crate::role_trailer::error_location!(),
                crate::role_trailer::receiver()
            )));
        }
        Ok(())
    }

    /// Async twin of [`read_expected_ndx_done`](Self::read_expected_ndx_done).
    ///
    /// Reads one NDX off an [`AsyncRead`](tokio::io::AsyncRead) via the codec's
    /// `.await` NDX reader - the counterpart of the sync `read_ndx` used above,
    /// sharing the same delta-decode leaf - and applies the identical NDX_DONE
    /// (-1) validation and error text. It consumes the same bytes and yields the
    /// same result as the sync leaf. Gated on `tokio-transfer`.
    #[cfg(feature = "tokio-transfer")]
    pub(in crate::receiver) async fn read_expected_ndx_done_async<R>(
        &self,
        ndx_read_codec: &mut NdxCodecEnum,
        reader: &mut R,
        context: &str,
    ) -> io::Result<()>
    where
        R: tokio::io::AsyncRead + Unpin + ?Sized,
    {
        let ndx = ndx_read_codec.read_ndx_async(reader).await?;
        if ndx != -1 {
            // upstream: io.c read_ndx / rsync.c:818 exit_cleanup(RERR_PROTOCOL)
            // (exit 2); see the sync twin `read_expected_ndx_done`.
            return Err(protocol::protocol_violation(format!(
                "expected NDX_DONE (-1) from sender during {context}, got {ndx} {}{}",
                crate::role_trailer::error_location!(),
                crate::role_trailer::receiver()
            )));
        }
        Ok(())
    }

    /// Async twin of [`exchange_phase_done`](Self::exchange_phase_done).
    ///
    /// The request half - `write_ndx_done` / `write_ndx` and `flush` on the
    /// synchronous `writer` - stays blocking, exactly as `receive_file_async`
    /// keeps its per-file request half sync. Only the sender's echoed NDX_DONE
    /// reads become `.await` (via
    /// [`read_expected_ndx_done_async`](Self::read_expected_ndx_done_async)). The
    /// phase-counter walk, the `reclaim_oldest_segment` bookkeeping, and the
    /// `Genr` debug emissions are the identical sync logic, so for the same wire
    /// bytes this exchanges the same NDX_DONE sequence and emits the same
    /// diagnostics as the blocking path.
    ///
    /// Gated on `tokio-transfer`; consumed by the async finalization sequence.
    #[cfg(feature = "tokio-transfer")]
    pub(in crate::receiver) async fn exchange_phase_done_async<R, W>(
        &mut self,
        reader: &mut R,
        writer: &mut W,
        ndx_write_codec: &mut NdxCodecEnum,
        ndx_read_codec: &mut NdxCodecEnum,
    ) -> io::Result<()>
    where
        R: tokio::io::AsyncRead + Unpin + ?Sized,
        W: Write + ?Sized,
    {
        let inc_recurse = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::INC_RECURSE));

        let max_phase: i32 = if self.protocol.supports_multi_phase() {
            2
        } else {
            1
        };

        if inc_recurse {
            let num_segments = self.ndx_segments.len();
            for _ in 0..num_segments {
                // upstream: receiver.c:573 - flist_free(first_flist)
                self.reclaim_oldest_segment();
                ndx_write_codec.write_ndx_done(&mut *writer)?;
                writer.flush()?;
                self.read_expected_ndx_done_async(ndx_read_codec, reader, "segment completion")
                    .await?;
            }

            // upstream: generator.c:2355-2357 - phase++ then "generate_files phase=%d"
            let mut phase: i32 = 1;
            // upstream: receiver.c:692-693 DEBUG_GTE(RECV, 1)
            debug_log!(Recv, 1, "recv_files phase={}", phase);
            debug_log!(Genr, 1, "generate_files phase={}", phase);

            for _phase_step in 2..=max_phase {
                ndx_write_codec.write_ndx_done(&mut *writer)?;
                writer.flush()?;
                self.read_expected_ndx_done_async(ndx_read_codec, reader, "phase transition")
                    .await?;
                phase += 1;
                // upstream: receiver.c:692-693 DEBUG_GTE(RECV, 1)
                debug_log!(Recv, 1, "recv_files phase={}", phase);
                debug_log!(Genr, 1, "generate_files phase={}", phase);
            }

            ndx_write_codec.write_ndx_done(&mut *writer)?;
            writer.flush()?;

            // upstream: generator.c:2391-2394 - protocol >= 29 emits a final phase.
            if self.protocol.supports_multi_phase() {
                phase += 1;
                debug_log!(Genr, 1, "generate_files phase={}", phase);
            }
        } else {
            let mut phase: i32 = 0;
            loop {
                ndx_write_codec.write_ndx_done(&mut *writer)?;
                writer.flush()?;
                phase += 1;
                // upstream: receiver.c:692-693 DEBUG_GTE(RECV, 1)
                debug_log!(Recv, 1, "recv_files phase={}", phase);
                debug_log!(Genr, 1, "generate_files phase={}", phase);
                if phase > max_phase {
                    break;
                }
                self.read_expected_ndx_done_async(ndx_read_codec, reader, "phase transition")
                    .await?;
            }
        }

        self.read_expected_ndx_done_async(ndx_read_codec, reader, "sender final")
            .await?;

        Ok(())
    }

    /// Handles the goodbye handshake at end of transfer.
    ///
    /// For protocol >= 31, sends `NDX_DEL_STATS` (when `--delete` ran) followed
    /// by `NDX_DONE`. For extended goodbye (protocol >= 32), additionally reads
    /// the echo and sends a final `NDX_DONE`.
    ///
    /// `NDX_DEL_STATS` emission mirrors upstream's daemon-recv parent process
    /// (the generator running alongside the receiver child). Our receiver
    /// performs the delete pass inline rather than forking, so it carries the
    /// counters in `self.pending_del_stats` and emits them here to keep wire
    /// parity with upstream.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:1107-1132` - daemon-recv parent process runs `generate_files`
    /// - `generator.c:2393-2398` - early `write_del_stats(f_out)` emission
    /// - `main.c:225-238` - `write_del_stats()` wire format
    pub(in crate::receiver) fn handle_goodbye<R: Read, W: Write + ?Sized>(
        &self,
        reader: &mut R,
        writer: &mut W,
        ndx_write_codec: &mut NdxCodecEnum,
        ndx_read_codec: &mut NdxCodecEnum,
    ) -> io::Result<()> {
        if !self.protocol.supports_goodbye_exchange() {
            return Ok(());
        }

        // upstream: generator.c:2393-2394 -
        //   `if (protocol_version >= 31 && EARLY_DELETE_DONE_MSG()) {
        //       if (delete_mode || force_delete || read_batch)
        //           write_del_stats(f_out);
        //   }`
        // Runs in the daemon-recv parent's `generate_files()`. We always
        // sweep before the transfer (EARLY case), so the early-emission gate
        // applies whenever deletion was requested. `force_delete` and
        // `read_batch` are not yet wired into `ParsedServerFlags`; `flags.delete`
        // is the only term we evaluate today.
        if self.protocol.supports_extended_goodbye() && self.config.flags.delete {
            ndx_write_codec.write_ndx(&mut *writer, NDX_DEL_STATS)?;
            self.pending_del_stats.write_to(&mut *writer)?;
        }

        ndx_write_codec.write_ndx_done(&mut *writer)?;
        writer.flush()?;

        if self.protocol.supports_extended_goodbye() {
            // upstream: main.c:875-906 read_final_goodbye() - the sender may
            // send NDX_DEL_STATS before the NDX_DONE echo. Loop to skip it.
            loop {
                let ndx = ndx_read_codec.read_ndx(reader)?;
                if ndx == NDX_DONE {
                    break;
                }
                if ndx == NDX_DEL_STATS {
                    // Consume the 5 varints of deletion statistics
                    let _stats = protocol::stats::DeleteStats::read_from(reader)?;
                    continue;
                }
                // upstream: main.c:922 exit_cleanup(RERR_PROTOCOL) (exit 2) - a
                // non-NDX_DONE goodbye echo is a protocol violation, tagged so
                // the core exit-code mapper yields 2 rather than RERR_STREAMIO(12).
                return Err(protocol::protocol_violation(format!(
                    "expected goodbye NDX_DONE echo (-1) from sender, got {ndx} {}{}",
                    crate::role_trailer::error_location!(),
                    crate::role_trailer::receiver()
                )));
            }

            ndx_write_codec.write_ndx_done(&mut *writer)?;
            writer.flush()?;
        }

        Ok(())
    }

    /// Async twin of [`handle_goodbye`](Self::handle_goodbye).
    ///
    /// The send half - `write_ndx` / `write_ndx_done`, `pending_del_stats.write_to`,
    /// and `flush` on the synchronous `writer` - stays blocking; only the
    /// sender's goodbye echo NDX reads (and the `NDX_DEL_STATS` payload drain)
    /// become `.await` (via `read_ndx_async` and
    /// [`DeleteStats::read_from_async`](protocol::stats::DeleteStats::read_from_async)).
    /// The protocol gating, the early-delete-stats emission, and the echo-skip
    /// loop are the identical sync logic, so for the same wire bytes this
    /// exchanges the same goodbye frames as the blocking path.
    ///
    /// Gated on `tokio-transfer`; consumed by the async finalization sequence.
    #[cfg(feature = "tokio-transfer")]
    pub(in crate::receiver) async fn handle_goodbye_async<R, W>(
        &self,
        reader: &mut R,
        writer: &mut W,
        ndx_write_codec: &mut NdxCodecEnum,
        ndx_read_codec: &mut NdxCodecEnum,
    ) -> io::Result<()>
    where
        R: tokio::io::AsyncRead + Unpin + ?Sized,
        W: Write + ?Sized,
    {
        if !self.protocol.supports_goodbye_exchange() {
            return Ok(());
        }

        // upstream: generator.c:2393-2394 - early write_del_stats(f_out).
        if self.protocol.supports_extended_goodbye() && self.config.flags.delete {
            ndx_write_codec.write_ndx(&mut *writer, NDX_DEL_STATS)?;
            self.pending_del_stats.write_to(&mut *writer)?;
        }

        ndx_write_codec.write_ndx_done(&mut *writer)?;
        writer.flush()?;

        if self.protocol.supports_extended_goodbye() {
            // upstream: main.c:875-906 read_final_goodbye() - the sender may
            // send NDX_DEL_STATS before the NDX_DONE echo. Loop to skip it.
            loop {
                let ndx = ndx_read_codec.read_ndx_async(reader).await?;
                if ndx == NDX_DONE {
                    break;
                }
                if ndx == NDX_DEL_STATS {
                    // Consume the 5 varints of deletion statistics.
                    let _stats = protocol::stats::DeleteStats::read_from_async(reader).await?;
                    continue;
                }
                // upstream: main.c:922 exit_cleanup(RERR_PROTOCOL) (exit 2) - a
                // non-NDX_DONE goodbye echo is a protocol violation, tagged so
                // the core exit-code mapper yields 2 rather than RERR_STREAMIO(12).
                return Err(protocol::protocol_violation(format!(
                    "expected goodbye NDX_DONE echo (-1) from sender, got {ndx} {}{}",
                    crate::role_trailer::error_location!(),
                    crate::role_trailer::receiver()
                )));
            }

            ndx_write_codec.write_ndx_done(&mut *writer)?;
            writer.flush()?;
        }

        Ok(())
    }

    /// Receives transfer statistics from the sender.
    ///
    /// Reads total_read, total_written, total_size and optionally flist build/xfer
    /// times (protocol >= 31) using the protocol-appropriate codec.
    pub(in crate::receiver) fn receive_stats<R: Read + ?Sized>(
        &self,
        reader: &mut R,
    ) -> io::Result<SenderStats> {
        let stats_codec = create_protocol_codec(self.protocol.as_u8());

        let total_read = stats_codec.read_stat(reader)? as u64;
        let total_written = stats_codec.read_stat(reader)? as u64;
        let total_size = stats_codec.read_stat(reader)? as u64;

        let (flist_buildtime_ms, flist_xfertime_ms) = if self.protocol.supports_flist_times() {
            let buildtime = stats_codec.read_stat(reader)? as u64;
            let xfertime = stats_codec.read_stat(reader)? as u64;
            (Some(buildtime), Some(xfertime))
        } else {
            (None, None)
        };

        Ok(SenderStats {
            total_read,
            total_written,
            total_size,
            flist_buildtime_ms,
            flist_xfertime_ms,
        })
    }

    /// Async twin of [`receive_stats`](Self::receive_stats).
    ///
    /// Reads the same three (or five, for protocol >= 31) stat values via the
    /// protocol codec's async `read_stat_async` - the `.await` counterpart of
    /// the sync `read_stat` used above, sharing the same longint/varlong
    /// primitives. It yields the same `SenderStats` and consumes the same bytes
    /// as the sync leaf. Gated on `tokio-transfer`; additive and unwired.
    ///
    /// Unwired: the atomic receiver fork (the coupled ASY-7 redo) consumes this
    /// leaf later; until then it is exercised only by the parity tests.
    #[cfg(feature = "tokio-transfer")]
    #[allow(dead_code)]
    pub(in crate::receiver) async fn receive_stats_async<R>(
        &self,
        reader: &mut R,
    ) -> io::Result<SenderStats>
    where
        R: tokio::io::AsyncRead + Unpin + ?Sized,
    {
        let stats_codec = create_protocol_codec(self.protocol.as_u8());

        let total_read = stats_codec.read_stat_async(reader).await? as u64;
        let total_written = stats_codec.read_stat_async(reader).await? as u64;
        let total_size = stats_codec.read_stat_async(reader).await? as u64;

        let (flist_buildtime_ms, flist_xfertime_ms) = if self.protocol.supports_flist_times() {
            let buildtime = stats_codec.read_stat_async(reader).await? as u64;
            let xfertime = stats_codec.read_stat_async(reader).await? as u64;
            (Some(buildtime), Some(xfertime))
        } else {
            (None, None)
        };

        Ok(SenderStats {
            total_read,
            total_written,
            total_size,
            flist_buildtime_ms,
            flist_xfertime_ms,
        })
    }

    /// Exchanges phase transitions, receives stats, and handles goodbye handshake.
    ///
    /// This is the common finalization sequence shared by all transfer modes.
    pub(in crate::receiver) fn finalize_transfer<R: Read, W: Write + ?Sized>(
        &mut self,
        reader: &mut R,
        writer: &mut W,
    ) -> io::Result<()> {
        // FSM: delta transfer complete. Advance to Finalization.
        self.pipeline
            .advance_to(TransferPhase::Finalization)
            .map_err(crate::fsm_error)?;

        let mut ndx_write_codec = create_ndx_codec(self.protocol.as_u8());
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        self.exchange_phase_done(reader, writer, &mut ndx_write_codec, &mut ndx_read_codec)?;

        if self.config.connection.client_mode {
            let _sender_stats = self.receive_stats(reader)?;
        }

        self.handle_goodbye(reader, writer, &mut ndx_write_codec, &mut ndx_read_codec)?;

        // upstream: main.c:1067 do_recv() child path and main.c:1117/1123
        // do_recv() parent path both call io_flush(FULL_FLUSH) immediately
        // after the final NDX_DONE write so the kernel ships the goodbye
        // frame (and any trailing multiplexed MSG_INFO frames) before the
        // transport FIN. Mirrors the symmetric flush in
        // `generator::transfer::orchestrator::run` (UTS-15.c) at the bottom
        // of the generator role. Without this flush the upstream-rsync
        // reverse-daemon-delta interop scenario (oc-rsync client pushing to
        // an upstream daemon receiver) hangs at the test timeout because the
        // peer awaits a final NDX_DONE echo that is still sitting in the
        // userspace writer buffer.
        //
        // Rule 12 (fail-loud): surface the flush error unless the peer has
        // already shut down. Early close during goodbye-shutdown is rare and
        // the transfer is over, so any other error is treated as a real
        // failure rather than swallowed.
        if let Err(e) = writer.flush() {
            if !crate::is_early_close_error(&e) {
                return Err(e);
            }
        }

        // upstream: receiver.c:1114-1115 DEBUG_GTE(RECV, 1)
        debug_log!(Recv, 1, "recv_files finished");

        // upstream: generator.c:2436-2437 - "generate_files finished" emitted at
        // the bottom of generate_files() after the final goodbye handshake.
        debug_log!(Genr, 1, "generate_files finished");

        // INC_RECURSE diagnostic I4 (#2199): emit cumulative NDX conversion
        // call count and partition_point comparison depth from the receiver
        // side. Aggregated across all receiver transfers in this process.
        let (ndx_calls, ndx_cmps) = crate::receiver::ndx_convert_totals();
        debug_log!(
            Genr,
            1,
            "receiver ndx_convert totals: calls={} partition_point_depth={}",
            ndx_calls,
            ndx_cmps
        );
        #[cfg(feature = "tracing")]
        ::tracing::debug!(
            target: "rsync::receiver::ndx_convert",
            calls = ndx_calls,
            partition_point_depth = ndx_cmps,
            "receiver ndx_convert totals"
        );

        // FSM: finalization complete. Advance to Complete.
        self.pipeline
            .advance_to(TransferPhase::Complete)
            .map_err(crate::fsm_error)?;

        Ok(())
    }

    /// Async twin of [`finalize_transfer`](Self::finalize_transfer).
    ///
    /// Runs the receiver's end-of-transfer finalization sequence with `.await`
    /// on every wire read: the phase-done exchange
    /// ([`exchange_phase_done_async`](Self::exchange_phase_done_async)), the
    /// client-mode sender-stats read
    /// ([`receive_stats_async`](Self::receive_stats_async)), and the goodbye
    /// handshake ([`handle_goodbye_async`](Self::handle_goodbye_async)). The FSM
    /// advances, the trailing flush (with the same early-close tolerance), and
    /// the `Genr`/`ndx_convert` diagnostics are the identical sync logic - only
    /// the wire reads differ, so for the same wire bytes this drives the same
    /// finalization frames and emits the same diagnostics as the blocking path.
    ///
    /// This is the async finalization tail the coupled async receiver driver
    /// (the deferred atomic ASY receiver fork) calls after its per-file
    /// [`receive_file_async`](crate::receiver::ReceiverContext::receive_file_async)
    /// loop, in place of [`finalize_transfer`](Self::finalize_transfer). Gated on
    /// `tokio-transfer`.
    #[cfg(feature = "tokio-transfer")]
    #[allow(dead_code)]
    pub(in crate::receiver) async fn finalize_transfer_async<R, W>(
        &mut self,
        reader: &mut R,
        writer: &mut W,
    ) -> io::Result<()>
    where
        R: tokio::io::AsyncRead + Unpin + ?Sized,
        W: Write + ?Sized,
    {
        // FSM: delta transfer complete. Advance to Finalization.
        self.pipeline
            .advance_to(TransferPhase::Finalization)
            .map_err(crate::fsm_error)?;

        let mut ndx_write_codec = create_ndx_codec(self.protocol.as_u8());
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        self.exchange_phase_done_async(reader, writer, &mut ndx_write_codec, &mut ndx_read_codec)
            .await?;

        if self.config.connection.client_mode {
            let _sender_stats = self.receive_stats_async(reader).await?;
        }

        self.handle_goodbye_async(reader, writer, &mut ndx_write_codec, &mut ndx_read_codec)
            .await?;

        // upstream: main.c:1067/1117/1123 - io_flush(FULL_FLUSH) after the final
        // NDX_DONE write. Fail-loud unless the peer already shut down.
        if let Err(e) = writer.flush() {
            if !crate::is_early_close_error(&e) {
                return Err(e);
            }
        }

        // upstream: receiver.c:1114-1115 DEBUG_GTE(RECV, 1)
        debug_log!(Recv, 1, "recv_files finished");

        // upstream: generator.c:2436-2437 - "generate_files finished".
        debug_log!(Genr, 1, "generate_files finished");

        let (ndx_calls, ndx_cmps) = crate::receiver::ndx_convert_totals();
        debug_log!(
            Genr,
            1,
            "receiver ndx_convert totals: calls={} partition_point_depth={}",
            ndx_calls,
            ndx_cmps
        );
        #[cfg(feature = "tracing")]
        ::tracing::debug!(
            target: "rsync::receiver::ndx_convert",
            calls = ndx_calls,
            partition_point_depth = ndx_cmps,
            "receiver ndx_convert totals"
        );

        // FSM: finalization complete. Advance to Complete.
        self.pipeline
            .advance_to(TransferPhase::Complete)
            .map_err(crate::fsm_error)?;

        Ok(())
    }
}

#[cfg(test)]
mod genr_debug_emission_tests {
    //! Wording tests for `--debug=GENR` producer emissions.
    //!
    //! Each test asserts the exact wire string that upstream rsync 3.4.1
    //! emits from `generator.c generate_files` and `recv_generator` under
    //! `DEBUG_GTE(GENR, 1)`. Strings are matched byte-for-byte because
    //! interop harnesses grep for these literals.

    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, debug_log, drain_events, init};
    use std::path::PathBuf;

    fn init_genr_level1() {
        let mut cfg = VerbosityConfig::default();
        cfg.debug.genr = 1;
        init(cfg);
        let _ = drain_events();
    }

    fn genr_messages() -> Vec<String> {
        drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Debug {
                    flag: DebugFlag::Genr,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn generator_starting_matches_upstream() {
        // upstream: generator.c:2260-2261 - "generator starting pid=%d"
        init_genr_level1();
        let pid: u32 = 12345;
        debug_log!(Genr, 1, "generator starting pid={}", pid);
        let msgs = genr_messages();
        assert!(
            msgs.iter().any(|m| m == "generator starting pid=12345"),
            "missing upstream wording: {msgs:?}"
        );
    }

    #[test]
    fn recv_generator_per_file_matches_upstream() {
        // upstream: generator.c:1234-1235 - "recv_generator(%s,%d)"
        init_genr_level1();
        let name = PathBuf::from("dir/file.txt");
        let ndx: i32 = 7;
        debug_log!(Genr, 1, "recv_generator({},{})", name.display(), ndx);
        let msgs = genr_messages();
        assert!(
            msgs.iter().any(|m| m == "recv_generator(dir/file.txt,7)"),
            "missing upstream wording: {msgs:?}"
        );
    }

    #[test]
    fn generate_files_phase_matches_upstream() {
        // upstream: generator.c:2355-2357, 2366-2368, 2392-2394
        // "generate_files phase=%d" - the phase counter advances 1 -> 2 -> 3
        // across the regular, redo, and protocol-29+ delay-updates phases.
        init_genr_level1();
        for phase in 1..=3 {
            debug_log!(Genr, 1, "generate_files phase={}", phase);
        }
        let msgs = genr_messages();
        for phase in 1..=3 {
            let expected = format!("generate_files phase={phase}");
            assert!(
                msgs.iter().any(|m| m == &expected),
                "missing upstream wording {expected:?}: {msgs:?}"
            );
        }
    }

    #[test]
    fn generate_files_finished_matches_upstream() {
        // upstream: generator.c:2436-2437 - "generate_files finished"
        init_genr_level1();
        debug_log!(Genr, 1, "generate_files finished");
        let msgs = genr_messages();
        assert!(
            msgs.iter().any(|m| m == "generate_files finished"),
            "missing upstream wording: {msgs:?}"
        );
    }

    #[test]
    fn genr_emissions_suppressed_when_disabled() {
        let cfg = VerbosityConfig::default();
        init(cfg);
        let _ = drain_events();
        debug_log!(Genr, 1, "generator starting pid=1");
        debug_log!(Genr, 1, "recv_generator(foo,0)");
        debug_log!(Genr, 1, "generate_files phase=1");
        debug_log!(Genr, 1, "generate_files finished");
        let msgs = genr_messages();
        assert!(
            msgs.is_empty(),
            "Genr debug emissions must be gated; got: {msgs:?}"
        );
    }

    fn init_flist_level1() {
        let mut cfg = VerbosityConfig::default();
        cfg.debug.flist = 1;
        init(cfg);
        let _ = drain_events();
    }

    fn flist_messages() -> Vec<String> {
        drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Debug {
                    flag: DebugFlag::Flist,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn delta_transmission_enabled_matches_upstream() {
        // upstream: generator.c:2290-2295 - "delta-transmission enabled" when
        // whole_file is off (the rolling-checksum delta path is used).
        init_flist_level1();
        debug_log!(Flist, 1, "delta-transmission {}", "enabled");
        let msgs = flist_messages();
        assert!(
            msgs.iter().any(|m| m == "delta-transmission enabled"),
            "missing upstream wording: {msgs:?}"
        );
    }

    #[test]
    fn delta_transmission_disabled_matches_upstream() {
        // upstream: generator.c:2290-2295 - "delta-transmission disabled for
        // local transfer or --whole-file" when whole_file is on.
        init_flist_level1();
        debug_log!(
            Flist,
            1,
            "delta-transmission {}",
            "disabled for local transfer or --whole-file"
        );
        let msgs = flist_messages();
        assert!(
            msgs.iter()
                .any(|m| m == "delta-transmission disabled for local transfer or --whole-file"),
            "missing upstream wording: {msgs:?}"
        );
    }

    #[test]
    fn delta_transmission_suppressed_when_disabled() {
        // DEBUG_GTE(FLIST, 1) is first active at -vv; nothing emits by default.
        let cfg = VerbosityConfig::default();
        init(cfg);
        let _ = drain_events();
        debug_log!(Flist, 1, "delta-transmission enabled");
        let msgs = flist_messages();
        assert!(
            msgs.is_empty(),
            "Flist debug emissions must be gated; got: {msgs:?}"
        );
    }
}

#[cfg(all(test, feature = "tokio-transfer"))]
mod finalize_parity_tests {
    //! Sync vs async wire-byte parity for the receiver finalization tail.
    //!
    //! Builds the sender-side finalization wire (the NDX_DONE phase echoes,
    //! optional sender stats, and the goodbye echo) with the sync codec - the
    //! exact bytes the sync [`finalize_transfer`] reads - then drives that
    //! identical wire through both [`finalize_transfer`] and the `.await`-based
    //! [`finalize_transfer_async`]. Asserts both succeed, consume the same byte
    //! count, and drive the receiver's writer to the same captured bytes,
    //! including when the async wire is delivered one byte at a time across
    //! `.await` points. Also covers `exchange_phase_done` / `handle_goodbye`
    //! transitively (finalize composes them), and pins the error text: a
    //! non-NDX_DONE in place of a phase echo rejects identically on both paths.

    use std::ffi::OsString;
    use std::io::{Cursor, Write};
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use protocol::ProtocolVersion;
    use protocol::codec::{NdxCodec, create_ndx_codec};
    use tokio::io::{AsyncRead, ReadBuf};

    use crate::config::ServerConfig;
    use crate::handshake::HandshakeResult;
    use crate::receiver::ReceiverContext;
    use crate::role::ServerRole;

    /// Delivers at most `chunk` bytes per `poll_read`, forcing the async
    /// finalize reads to cross `.await` boundaries mid-value when `chunk == 1`.
    struct ChunkedReader {
        inner: Cursor<Vec<u8>>,
        chunk: usize,
    }

    impl ChunkedReader {
        fn new(bytes: Vec<u8>, chunk: usize) -> Self {
            Self {
                inner: Cursor::new(bytes),
                chunk: chunk.max(1),
            }
        }

        fn consumed(&self) -> u64 {
            self.inner.position()
        }
    }

    impl AsyncRead for ChunkedReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let limit = self.chunk.min(buf.remaining());
            if limit == 0 {
                return Poll::Ready(Ok(()));
            }
            let pos = self.inner.position() as usize;
            let data = self.inner.get_ref();
            if pos >= data.len() {
                return Poll::Ready(Ok(()));
            }
            let end = (pos + limit).min(data.len());
            let slice = data[pos..end].to_vec();
            buf.put_slice(&slice);
            self.inner.set_position(end as u64);
            Poll::Ready(Ok(()))
        }
    }

    /// A capturing writer with a no-op `MsgInfoSender`, so the receiver's
    /// finalize send-half (NDX_DONE frames) is recorded for parity comparison.
    struct CaptureWriter(Vec<u8>);
    impl Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl crate::writer::MsgInfoSender for CaptureWriter {
        fn send_msg_info(&mut self, _data: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn handshake_for(protocol_version: u8) -> HandshakeResult {
        HandshakeResult {
            protocol: ProtocolVersion::try_from(protocol_version).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
        }
    }

    fn receiver_for(protocol_version: u8) -> ReceiverContext {
        let handshake = handshake_for(protocol_version);
        let config = ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(protocol_version).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            args: vec![OsString::from(".")],
            ..Default::default()
        };
        let mut ctx = ReceiverContext::new_for_test(&handshake, config);
        ctx.advance_pipeline_to_delta_transfer_for_test();
        ctx
    }

    /// Builds the sender-side finalization wire the receiver reads: `phase_dones`
    /// NDX_DONE frames for the phase exchange, then `goodbye_dones` NDX_DONE
    /// frames for the goodbye echo. Encoded with the same codec the receiver
    /// decodes with, so the bytes are exactly what the wire carries.
    fn build_finalize_wire(
        protocol_version: u8,
        phase_dones: usize,
        goodbye_dones: usize,
    ) -> Vec<u8> {
        let mut codec = create_ndx_codec(protocol_version);
        let mut wire = Vec::new();
        for _ in 0..(phase_dones + goodbye_dones) {
            codec.write_ndx_done(&mut wire).unwrap();
        }
        wire
    }

    /// The async finalization tail consumes the identical sender wire and drives
    /// the receiver's writer to the identical bytes as the sync tail, on both a
    /// legacy (single-phase, no extended goodbye) and a modern (multi-phase,
    /// extended goodbye) protocol - even when the wire is delivered one byte per
    /// poll. Non-client so no sender-stats read is interleaved (that leaf has its
    /// own parity test).
    #[tokio::test(flavor = "current_thread")]
    async fn finalize_transfer_async_matches_sync() {
        // Protocol 32: multi-phase (3 phase NDX_DONE) + extended goodbye
        // (1 echo NDX_DONE). Protocol 28: single-phase (2 phase NDX_DONE:
        // one write+read at phase 1, then phase 2 > max_phase=1 breaks after
        // the second read) and no goodbye exchange.
        //
        // Phase-done counts are derived by tracing exchange_phase_done's
        // non-inc-recurse loop for the fresh (single-segment) receiver.
        for (protocol, phase_dones, goodbye_dones) in [(28u8, 2usize, 0usize), (32u8, 3, 1)] {
            let wire = build_finalize_wire(protocol, phase_dones, goodbye_dones);

            // Sync reference.
            let mut sync_ctx = receiver_for(protocol);
            let mut sync_reader = Cursor::new(wire.clone());
            let mut sync_writer = CaptureWriter(Vec::new());
            sync_ctx
                .finalize_transfer(&mut sync_reader, &mut sync_writer)
                .expect("sync finalize must succeed");
            let sync_consumed = sync_reader.position();
            let sync_sent = sync_writer.0.clone();

            for chunk in [1usize, 2, 3, wire.len().max(1)] {
                let mut async_ctx = receiver_for(protocol);
                let mut async_reader = ChunkedReader::new(wire.clone(), chunk);
                let mut async_writer = CaptureWriter(Vec::new());
                async_ctx
                    .finalize_transfer_async(&mut async_reader, &mut async_writer)
                    .await
                    .unwrap_or_else(|e| {
                        panic!("protocol={protocol} chunk={chunk}: async finalize failed: {e}")
                    });

                assert_eq!(
                    async_reader.consumed(),
                    sync_consumed,
                    "protocol={protocol} chunk={chunk}: async finalize consumed a different byte count"
                );
                assert_eq!(
                    async_writer.0, sync_sent,
                    "protocol={protocol} chunk={chunk}: async finalize send-half diverged from sync"
                );
            }
        }
    }

    /// A non-NDX_DONE value in place of the sender's first phase echo is rejected
    /// identically by both the sync and async phase exchange: same `InvalidData`
    /// kind, so the async twin does not silently accept a desynced phase frame.
    #[tokio::test(flavor = "current_thread")]
    async fn exchange_phase_done_async_rejects_non_done_like_sync() {
        let protocol = 32u8;
        // A single valid NDX (0), which is not NDX_DONE (-1), where the first
        // phase echo is expected. Both paths write their first NDX_DONE, then
        // read this and must reject it.
        let mut codec = create_ndx_codec(protocol);
        let mut wire = Vec::new();
        codec.write_ndx(&mut wire, 0).unwrap();

        let mut sync_ctx = receiver_for(protocol);
        let mut sync_reader = Cursor::new(wire.clone());
        let mut sync_writer = CaptureWriter(Vec::new());
        let sync_err = sync_ctx
            .finalize_transfer(&mut sync_reader, &mut sync_writer)
            .expect_err("sync finalize must reject a non-NDX_DONE phase echo");

        let mut async_ctx = receiver_for(protocol);
        let mut async_reader = ChunkedReader::new(wire, 1);
        let mut async_writer = CaptureWriter(Vec::new());
        let async_err = async_ctx
            .finalize_transfer_async(&mut async_reader, &mut async_writer)
            .await
            .expect_err("async finalize must reject a non-NDX_DONE phase echo");

        assert_eq!(
            async_err.kind(),
            sync_err.kind(),
            "async finalize error kind must match sync"
        );
        assert_eq!(async_err.kind(), std::io::ErrorKind::InvalidData);
    }
}

#[cfg(all(test, feature = "tokio-transfer"))]
mod receive_stats_parity_tests {
    //! Sync vs async wire-byte parity for [`receive_stats`] /
    //! [`receive_stats_async`].
    //!
    //! Encodes the sender-stats fields with the protocol codec, then decodes
    //! the identical bytes with the blocking [`receive_stats`] and the
    //! `.await`-driven [`receive_stats_async`], asserting every field matches
    //! and that both consume the same number of bytes - including when the
    //! wire is delivered one byte at a time across await points.

    use std::ffi::OsString;
    use std::io::Cursor;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use protocol::ProtocolVersion;
    use protocol::codec::{ProtocolCodec, create_protocol_codec};
    use tokio::io::{AsyncRead, ReadBuf};

    use crate::config::ServerConfig;
    use crate::handshake::HandshakeResult;
    use crate::receiver::ReceiverContext;
    use crate::role::ServerRole;

    struct ChunkedReader {
        inner: Cursor<Vec<u8>>,
        chunk: usize,
    }

    impl ChunkedReader {
        fn new(bytes: Vec<u8>, chunk: usize) -> Self {
            Self {
                inner: Cursor::new(bytes),
                chunk: chunk.max(1),
            }
        }

        fn consumed(&self) -> u64 {
            self.inner.position()
        }
    }

    impl AsyncRead for ChunkedReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let limit = self.chunk.min(buf.remaining());
            if limit == 0 {
                return Poll::Ready(Ok(()));
            }
            let mut scratch = vec![0u8; limit];
            let mut scratch_buf = ReadBuf::new(&mut scratch);
            match Pin::new(&mut self.inner).poll_read(cx, &mut scratch_buf) {
                Poll::Ready(Ok(())) => {
                    buf.put_slice(scratch_buf.filled());
                    Poll::Ready(Ok(()))
                }
                other => other,
            }
        }
    }

    fn handshake_for(protocol_version: u8) -> HandshakeResult {
        HandshakeResult {
            protocol: ProtocolVersion::try_from(protocol_version).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
        }
    }

    fn receiver_for(protocol_version: u8) -> ReceiverContext {
        let handshake = handshake_for(protocol_version);
        let config = ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(protocol_version).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            args: vec![OsString::from(".")],
            ..Default::default()
        };
        ReceiverContext::new_for_test(&handshake, config)
    }

    /// Encodes the sender-stats fields the way upstream `handle_stats` does:
    /// total_read, total_written, total_size, then (protocol >= 29) the two
    /// flist times, each via the protocol codec's `write_stat`.
    fn encode_stats(protocol_version: u8, values: &[i64]) -> Vec<u8> {
        let codec = create_protocol_codec(protocol_version);
        let mut wire = Vec::new();
        for &v in values {
            codec.write_stat(&mut wire, v).unwrap();
        }
        wire
    }

    #[tokio::test(flavor = "current_thread")]
    async fn receive_stats_async_matches_sync() {
        // Protocol 32 sends 5 stat fields (>= 29 adds the two flist times);
        // protocol 28 sends only the first 3.
        for (protocol, fields) in [
            (28u8, vec![10_000i64, 512, 1_048_576]),
            (32u8, vec![10_000i64, 512, 1_048_576, 7, 3]),
        ] {
            let wire = encode_stats(protocol, &fields);
            let receiver = receiver_for(protocol);

            let mut sync_cur = Cursor::new(wire.clone());
            let sync_stats = receiver.receive_stats(&mut sync_cur).unwrap();
            let sync_consumed = sync_cur.position();

            for chunk in [1usize, 2, 3, 7, 13] {
                let mut reader = ChunkedReader::new(wire.clone(), chunk);
                let async_stats = receiver.receive_stats_async(&mut reader).await.unwrap();

                assert_eq!(async_stats.total_read, sync_stats.total_read);
                assert_eq!(async_stats.total_written, sync_stats.total_written);
                assert_eq!(async_stats.total_size, sync_stats.total_size);
                assert_eq!(
                    async_stats.flist_buildtime_ms,
                    sync_stats.flist_buildtime_ms
                );
                assert_eq!(async_stats.flist_xfertime_ms, sync_stats.flist_xfertime_ms);
                assert_eq!(
                    reader.consumed(),
                    sync_consumed,
                    "async receive_stats consumed a different byte count (protocol {protocol}, chunk {chunk})"
                );
            }
        }
    }
}
