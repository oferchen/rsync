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

use protocol::CompatibilityFlags;
use protocol::codec::{
    NDX_DEL_STATS, NDX_DONE, NdxCodec, NdxCodecEnum, ProtocolCodec, create_ndx_codec,
    create_protocol_codec,
};

use crate::receiver::ReceiverContext;
use crate::receiver::stats::SenderStats;

impl ReceiverContext {
    /// Exchanges NDX_DONE messages for phase transitions.
    ///
    /// With INC_RECURSE, sends one NDX_DONE per segment then per extra phase.
    /// Without INC_RECURSE, alternates send/receive for each phase boundary.
    /// Always reads a final NDX_DONE from the sender after all phases complete.
    pub(in crate::receiver) fn exchange_phase_done<R: Read, W: Write + ?Sized>(
        &self,
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
                ndx_write_codec.write_ndx_done(&mut *writer)?;
                writer.flush()?;
                self.read_expected_ndx_done(ndx_read_codec, reader, "segment completion")?;
            }

            for _phase in 2..=max_phase {
                ndx_write_codec.write_ndx_done(&mut *writer)?;
                writer.flush()?;
                self.read_expected_ndx_done(ndx_read_codec, reader, "phase transition")?;
            }

            ndx_write_codec.write_ndx_done(&mut *writer)?;
            writer.flush()?;
        } else {
            let mut phase: i32 = 0;
            loop {
                ndx_write_codec.write_ndx_done(&mut *writer)?;
                writer.flush()?;
                phase += 1;
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
    /// For protocol >= 31, sends NDX_DONE. For extended goodbye (protocol >= 32),
    /// additionally reads the echo and sends a final NDX_DONE.
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
        &self,
        reader: &mut R,
        writer: &mut W,
    ) -> io::Result<()> {
        let mut ndx_write_codec = create_ndx_codec(self.protocol.as_u8());
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        self.exchange_phase_done(reader, writer, &mut ndx_write_codec, &mut ndx_read_codec)?;

        if self.config.connection.client_mode {
            let _sender_stats = self.receive_stats(reader)?;
        }

        self.handle_goodbye(reader, writer, &mut ndx_write_codec, &mut ndx_read_codec)?;

        Ok(())
    }
}
