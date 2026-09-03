//! Goodbye handshake handling for the generator role.
//!
//! The handshake is split into `read_receiver_goodbye` and
//! `answer_goodbye_with_finalizer` so a caller can act in the gap between the
//! two - the last point at which the sender can still write a diagnostic the
//! peer will read. Helpers: `should_send_del_stats`,
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
use crate::receiver::ndx_stream::{FlistMarkerSink, NdxFrame, StreamRole, read_marker_aware_ndx};
use crate::role_trailer::error_location;

/// What the receiver's half of the goodbye handshake delivered.
///
/// Distinguishes "the peer said goodbye, answer it" from "there is nothing to
/// answer" so the caller can run work in the gap between the two halves without
/// having to re-derive whether the exchange is still live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::generator) enum GoodbyeArrival {
    /// The receiver's goodbye `NDX_DONE` arrived and is waiting for the
    /// sender's reply (upstream: `main.c:919-922` - the sender echoes it back).
    Done,
    /// This protocol version exchanges no goodbye, or the peer closed the
    /// connection before sending one. Nothing is owed to the peer.
    None,
}

/// The sender's view of the goodbye NDX stream.
///
/// Borrows the generator so `NDX_DEL_STATS` lands in its counters, and reports
/// [`StreamRole::Sender`] so upstream's `am_sender` term (`rsync.c:343`) is what
/// rejects any file-list marker. `inc_recurse` is reported truthfully rather
/// than forced off, so the rejection is attributable to the role and not to a
/// capability the transfer actually negotiated.
///
/// The primitive is defined under `receiver` because that is where the lazy
/// file-list state it inverts lives; it holds no receiver-only types, and the
/// generator already reaches across for shared wire types (see
/// `generator/protocol_io.rs`'s use of `receiver::SumHead`).
struct GoodbyeNdxSink<'a>(&'a mut GeneratorContext);

impl FlistMarkerSink for GoodbyeNdxSink<'_> {
    type FrameMark = ();

    fn role(&self) -> StreamRole {
        StreamRole::Sender
    }

    fn last_file_ndx(&self) -> i32 {
        // upstream: rsync.c:345-348 - the last index of the newest flist, or -1
        // when nothing has been sent.
        self.0.file_list().len() as i32 - 1
    }

    fn ndx_is_active(&self, ndx: i32) -> bool {
        // upstream: sender.c:558-563 - `send_files()` tests F_IS_ACTIVE on the
        // entry the peer named. An out-of-range index is a different fault and
        // is owned by `last_file_ndx`, so it is not reported as cleared here.
        usize::try_from(ndx)
            .ok()
            .and_then(|flat| self.0.file_list().get(flat))
            .is_none_or(protocol::flist::FileEntry::is_active)
    }

    fn begin_frame(&mut self) {}

    fn on_del_stats(&mut self, stats: &DeleteStats) -> io::Result<()> {
        // upstream: main.c:238-247 read_del_stats() adds to the global counters.
        self.0.accumulate_delete_stats(stats);
        debug_log!(
            Flist,
            2,
            "consumed NDX_DEL_STATS during goodbye: {} deletions",
            stats.total()
        );
        Ok(())
    }

    fn inc_recurse(&self) -> bool {
        self.0.inc_recurse()
    }
}

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

    /// Variant of `handle_goodbye` that runs an
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
    #[cfg(test)]
    pub(in crate::generator) fn handle_goodbye_with_finalizer<R, W, F>(
        &mut self,
        reader: &mut R,
        writer: &mut W,
        ndx_read_codec: &mut protocol::codec::NdxCodecEnum,
        ndx_write_codec: &mut MonotonicNdxWriter,
        finalize_between_write_and_read: F,
    ) -> io::Result<()>
    where
        R: Read,
        W: Write,
        F: FnMut(&mut W) -> io::Result<()>,
    {
        if self.read_receiver_goodbye(reader, ndx_read_codec)? == GoodbyeArrival::Done {
            self.answer_goodbye_with_finalizer(
                reader,
                writer,
                ndx_read_codec,
                ndx_write_codec,
                finalize_between_write_and_read,
            )?;
        }
        Ok(())
    }

    /// Reads the receiver's goodbye `NDX_DONE`, the first half of the
    /// handshake.
    ///
    /// Split from the answering half so the caller owns the gap between the
    /// two, which is the last point in the run at which the sender can still
    /// write a diagnostic the peer will read. The `--remove-source-files` drain
    /// uses it (see `GeneratorContext::goodbye_draining_source_removals`).
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:916-919` - `read_final_goodbye()` reads the receiver's NDX first
    pub(in crate::generator) fn read_receiver_goodbye<R: Read>(
        &mut self,
        reader: &mut R,
        ndx_read_codec: &mut protocol::codec::NdxCodecEnum,
    ) -> io::Result<GoodbyeArrival> {
        if !self.protocol.supports_goodbye_exchange() {
            return Ok(GoodbyeArrival::None);
        }

        // Read first NDX_DONE from receiver, skipping any NDX_DEL_STATS.
        // upstream: main.c:904 - read_ndx_and_attrs() handles NDX_DEL_STATS internally.
        // Connection may close early in dry-run or when the remote daemon exits before
        // completing the goodbye exchange - treat this as acceptable.
        let ndx = match self.read_ndx_skipping_del_stats(reader, ndx_read_codec) {
            Ok(ndx) => ndx,
            Err(e) if is_early_close_error(&e) => {
                return Ok(GoodbyeArrival::None);
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
        Ok(GoodbyeArrival::Done)
    }

    /// Answers the receiver's goodbye, the second half of the handshake.
    ///
    /// Called only after [`read_receiver_goodbye`](Self::read_receiver_goodbye)
    /// reported [`GoodbyeArrival::Done`]. Writing the sender's `NDX_DONE` here
    /// is the point after which the peer stops reading, so anything the sender
    /// still owes the peer must already be on the wire.
    pub(in crate::generator) fn answer_goodbye_with_finalizer<R, W, F>(
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
    /// Delegates to the shared marker-aware reader
    /// ([`crate::receiver::ndx_stream::read_marker_aware_ndx`]) so the branch
    /// order lives in one place. The asymmetry with the receiver is upstream's:
    /// `rsync.c:343` rejects every negative NDX other than NDX_DONE and
    /// NDX_DEL_STATS when `am_sender` is set, so a file-list marker arriving
    /// here is a protocol violation rather than a segment to consume - hence
    /// [`GoodbyeNdxSink`] declares [`StreamRole::Sender`] while still reporting
    /// this transfer's real `inc_recurse`, leaving the `am_sender` term as the
    /// thing that rejects.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync.c:336-342` - NDX_DEL_STATS drain in `read_ndx_and_attrs()`
    /// - `rsync.c:343-352` - the `am_sender` rejection
    /// - `main.c:238-247` - `read_del_stats()` accumulates into global counters
    fn read_ndx_skipping_del_stats<R: Read>(
        &mut self,
        reader: &mut R,
        ndx_read_codec: &mut protocol::codec::NdxCodecEnum,
    ) -> io::Result<i32> {
        let mut sink = GoodbyeNdxSink(self);
        match read_marker_aware_ndx(reader, ndx_read_codec, &mut sink)? {
            NdxFrame::Done => Ok(NDX_DONE),
            NdxFrame::File(ndx) => Ok(ndx),
        }
    }

    /// Accumulates deletion statistics from an NDX_DEL_STATS message.
    /// (upstream: main.c:238-247 - `read_del_stats()` adds to global counters)
    pub(super) fn accumulate_delete_stats(&mut self, stats: &DeleteStats) {
        self.delete_stats.merge(stats);
    }
}
