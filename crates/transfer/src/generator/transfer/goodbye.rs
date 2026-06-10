//! Goodbye handshake handling for the generator role.
//!
//! Contains `handle_goodbye` plus its helpers `should_send_del_stats`,
//! `read_ndx_skipping_del_stats`, and `accumulate_delete_stats`.
//!
//! # Upstream Reference
//!
//! - `main.c:875-906` - `read_final_goodbye()` with del_stats handling

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
    /// Deletion statistics are sent whenever deletion is active, following
    /// upstream 3.4.4's early/late timing (the `--stats`/`INFO_GTE(STATS, 2)`
    /// requirement was removed in 3.4.4). Both paths share the predicate
    /// `delete_mode || force_delete || read_batch`:
    /// - **Early** (delete_during or delete_before): sent at the early-delete
    ///   checkpoint just after the first phase NDX_DONE pair.
    /// - **Late** (delete_delay or delete_after): sent at the late-delete
    ///   checkpoint after delayed deletions complete.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:875-906` - `read_final_goodbye()`
    /// - `main.c:883` - protocol < 29 uses `read_int(f_in)`
    /// - `main.c:885-886` - protocol >= 29 uses `read_ndx_and_attrs()`
    /// - `rsync.c:337-342` - NDX_DEL_STATS handling in `read_ndx_and_attrs()`
    /// - `main.c:225-238` - `write_del_stats()` format
    /// - `generator.c:2393-2398` (3.4.4) - early del_stats path (no STATS gate)
    /// - `generator.c:2437-2442` (3.4.4) - late del_stats path (no STATS gate)
    pub(in crate::generator) fn handle_goodbye<R: Read, W: Write>(
        &mut self,
        reader: &mut R,
        writer: &mut W,
        ndx_read_codec: &mut protocol::codec::NdxCodecEnum,
        ndx_write_codec: &mut MonotonicNdxWriter,
    ) -> io::Result<()> {
        if !self.protocol.supports_goodbye_exchange() {
            return Ok(());
        }

        // Read first NDX_DONE from receiver, skipping any NDX_DEL_STATS.
        // upstream: main.c:886 - read_ndx_and_attrs() handles NDX_DEL_STATS internally.
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
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "expected goodbye NDX_DONE (-1) from receiver, got {ndx} {}{}",
                    error_location!(),
                    crate::role_trailer::sender()
                ),
            ));
        }

        // For protocol 31+: conditionally send del_stats, echo NDX_DONE, read final NDX_DONE.
        //
        // Upstream 3.4.4 removed the INFO_GTE(STATS, 2) (--stats) gate. del_stats are now
        // sent whenever deletion is active, split into early vs late paths by timing:
        // - Early (generator.c:2393-2398 in 3.4.4): !(delete_during==2 || delete_after) =>
        //   send del_stats when (delete_mode || force_delete || read_batch)
        // - Late  (generator.c:2437-2442 in 3.4.4): (delete_during==2 || delete_after) =>
        //   send del_stats when (delete_mode || force_delete || read_batch)
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

            // Read final NDX_DONE - may fail if daemon kills receiver child early
            match self.read_ndx_skipping_del_stats(reader, ndx_read_codec) {
                Ok(final_ndx) => {
                    if final_ndx != NDX_DONE {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "expected final goodbye NDX_DONE (-1) from receiver, got {final_ndx} {}{}",
                                error_location!(),
                                crate::role_trailer::sender()
                            ),
                        ));
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
    /// Mirrors upstream 3.4.4's conditional logic for `write_del_stats()` in
    /// the generator goodbye sequence. Upstream 3.4.4 removed the
    /// `INFO_GTE(STATS, 2)` (`--stats`) gate; del_stats are now emitted
    /// whenever deletion is active, with timing split by early vs late:
    ///
    /// - **Early** (`!late_delete`): `delete_mode || force_delete || read_batch`
    ///   (upstream: generator.c:2394 in 3.4.4)
    /// - **Late** (`late_delete`): same condition
    ///   (upstream: generator.c:2439 in 3.4.4)
    ///
    /// Note: oc-rsync does not currently track `force_delete` or `read_batch`
    /// as distinct flags in the transfer config. `flags.delete` covers the
    /// `delete_mode` case; the other two are out of scope for URV-6.a and
    /// will be wired in once the corresponding config plumbing exists.
    pub(in crate::generator) fn should_send_del_stats(&self) -> bool {
        // upstream: generator.c:2394 / 2439 in 3.4.4 -
        // delete_mode || force_delete || read_batch (no INFO_GTE(STATS, 2) gate).
        // Early and late paths share the same predicate; `late_delete` governs
        // *which* checkpoint emits the message, not whether to emit.
        self.config.flags.delete
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
