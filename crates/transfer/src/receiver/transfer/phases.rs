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
            debug_log!(Genr, 1, "generate_files phase={}", phase);

            for _phase_step in 2..=max_phase {
                ndx_write_codec.write_ndx_done(&mut *writer)?;
                writer.flush()?;
                self.read_expected_ndx_done(ndx_read_codec, reader, "phase transition")?;
                // upstream: generator.c:2366-2368 - phase++ on each additional
                // iteration (covers the redo phase).
                phase += 1;
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
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "expected NDX_DONE (-1) from sender during {context}, got {ndx} {}{}",
                    crate::role_trailer::error_location!(),
                    crate::role_trailer::receiver()
                ),
            ));
        }
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
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "expected goodbye NDX_DONE echo (-1) from sender, got {ndx} {}{}",
                        crate::role_trailer::error_location!(),
                        crate::role_trailer::receiver()
                    ),
                ));
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
}
