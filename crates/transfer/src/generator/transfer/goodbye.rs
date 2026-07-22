//! Goodbye handshake handling for the generator role.
//!
//! Contains `handle_goodbye` plus its helpers `should_send_del_stats`,
//! `read_ndx_skipping_del_stats`, and `accumulate_delete_stats`.
//!
//! # Upstream Reference
//!
//! - `main.c:893-924` - `read_final_goodbye()` with del_stats handling

use std::io::{self, Read, Write};

use logging::debug_log;
use protocol::codec::{MonotonicNdxWriter, NDX_DEL_STATS, NDX_DONE, NdxCodec};
use protocol::stats::DeleteStats;

use super::super::{GeneratorContext, is_early_close_error};
use crate::role_trailer::error_location;

impl GeneratorContext {
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
    /// - `main.c:893-924` - `read_final_goodbye()`
    /// - `main.c:901` - protocol < 29 uses `read_int(f_in)`
    /// - `main.c:903-904` - protocol >= 29 uses `read_ndx_and_attrs()`
    /// - `rsync.c:337-342` - NDX_DEL_STATS handling in `read_ndx_and_attrs()`
    /// - `main.c:225-238` - `write_del_stats()` format
    /// - `generator.c:2376-2381` - early del_stats path
    /// - `generator.c:2420-2425` - late del_stats path
    #[cfg(test)]
    pub(in crate::generator) fn handle_goodbye<R: Read, W: Write>(
        &mut self,
        reader: &mut R,
        writer: &mut W,
        ndx_read_codec: &mut protocol::codec::NdxCodecEnum,
        ndx_write_codec: &mut MonotonicNdxWriter,
    ) -> io::Result<()> {
        self.handle_goodbye_with_finalizer(
            reader,
            writer,
            ndx_read_codec,
            ndx_write_codec,
            |_writer| Ok(()),
        )
    }

    /// Variant of [`handle_goodbye`](Self::handle_goodbye) that runs an
    /// arbitrary finalizer between writing the sender's goodbye NDX_DONE and
    /// blocking on the receiver's final NDX_DONE reply.
    ///
    /// The finalizer is the hook that lets the daemon-sender flush codec
    /// state (e.g. emit the zlib `Z_FINISH` end-of-stream trailer under `-zz`
    /// daemon pull) before the read side blocks. Without this hook, a
    /// receiver running through `CompressedReader` can deadlock waiting on a
    /// closing deflate block that the sender has not yet emitted, while the
    /// sender simultaneously waits on the receiver's final NDX_DONE.
    ///
    /// upstream: `main.c:979-983 do_server_sender()` runs
    /// `io_flush(FULL_FLUSH)` immediately before `read_final_goodbye()` so
    /// the FIN is preceded by every buffered byte. Under `-zz` upstream's
    /// `write_buf()` bypasses the deflate stream entirely (see
    /// `io.c:2255 write_buf()`), so no codec finalisation is required there.
    /// In our writer-graph the goodbye NDX_DONE rides through
    /// `CompressedWriter`, so we additionally need the finalizer to emit
    /// `Z_FINISH` (`token.c:367 send_deflated_token()` performs the matching
    /// `deflateEnd()` at end of transfer) before the receiver tries to
    /// decompress past the in-flight block.
    pub(in crate::generator) fn handle_goodbye_with_finalizer<R, W, F>(
        &mut self,
        reader: &mut R,
        writer: &mut W,
        ndx_read_codec: &mut protocol::codec::NdxCodecEnum,
        ndx_write_codec: &mut MonotonicNdxWriter,
        mut finalize_between_write_and_read: F,
    ) -> io::Result<()>
    where
        R: Read,
        W: Write,
        F: FnMut(&mut W) -> io::Result<()>,
    {
        if !self.protocol.supports_goodbye_exchange() {
            return Ok(());
        }

        // Read first NDX_DONE from receiver, skipping any NDX_DEL_STATS.
        // upstream: main.c:904 - read_ndx_and_attrs() handles NDX_DEL_STATS internally.
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
            // upstream: main.c:1097 exit_cleanup(RERR_PROTOCOL) (exit 2). Tag the
            // error so the core exit-code mapper yields 2, not RERR_STREAMIO(12).
            return Err(protocol::protocol_violation(format!(
                "expected goodbye NDX_DONE (-1) from receiver, got {ndx} {}{}",
                error_location!(),
                crate::role_trailer::sender()
            )));
        }

        // For protocol 31+: conditionally send del_stats, echo NDX_DONE, read final NDX_DONE.
        //
        // Upstream gates del_stats sending on INFO_GTE(STATS, 2) (i.e. --stats was passed)
        // and splits it into early vs late paths depending on deletion timing:
        // - Early (generator.c:2393-2398): !(delete_during==2 || delete_after) =>
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

            // UTS-9.REOPEN: under -zz daemon pull the receiver's
            // CompressedReader cannot decode past an unterminated deflate
            // block while we block on read_ndx below, producing a deadlock
            // that surfaces to the user as "connection unexpectedly closed
            // (N bytes received so far) [receiver]" once the daemon times
            // out and FINs. Drive the finalizer here, between the goodbye
            // write and the goodbye read, so the codec can emit its
            // end-of-stream trailer before the receiver tries to advance.
            //
            // upstream: token.c:367 send_deflated_token() emits the
            // Z_FINISH-terminated stream at end of transfer; main.c:982
            // read_final_goodbye() is bracketed by io_flush(FULL_FLUSH).
            if let Err(e) = finalize_between_write_and_read(writer) {
                if is_early_close_error(&e) {
                    return Ok(());
                }
                return Err(e);
            }

            // Read final NDX_DONE - may fail if daemon kills receiver child early
            match self.read_ndx_skipping_del_stats(reader, ndx_read_codec) {
                Ok(final_ndx) => {
                    if final_ndx != NDX_DONE {
                        // upstream: main.c:1097 exit_cleanup(RERR_PROTOCOL)
                        // (exit 2); tagged so the mapper yields 2 not streamio.
                        return Err(protocol::protocol_violation(format!(
                            "expected final goodbye NDX_DONE (-1) from receiver, got {final_ndx} {}{}",
                            error_location!(),
                            crate::role_trailer::sender()
                        )));
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
    pub(in crate::generator) fn should_send_del_stats(&self) -> bool {
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
    pub(super) fn accumulate_delete_stats(&mut self, stats: &DeleteStats) {
        self.delete_stats.files = self.delete_stats.files.saturating_add(stats.files);
        self.delete_stats.dirs = self.delete_stats.dirs.saturating_add(stats.dirs);
        self.delete_stats.symlinks = self.delete_stats.symlinks.saturating_add(stats.symlinks);
        self.delete_stats.devices = self.delete_stats.devices.saturating_add(stats.devices);
        self.delete_stats.specials = self.delete_stats.specials.saturating_add(stats.specials);
    }
}
