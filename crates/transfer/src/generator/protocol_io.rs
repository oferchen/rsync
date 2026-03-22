//! Protocol I/O operations for the generator role.
//!
//! Handles file list transmission (`send_file_list`, `send_extra_file_lists`),
//! UID/GID name mapping lists (`send_id_lists`), signature block reading from
//! the receiver, and NDX + iflags wire encoding.
//!
//! # Upstream Reference
//!
//! - `flist.c:2192-2545` - File list building and sending
//! - `uidlist.c:407-414` - `send_id_lists()` for name-based ownership
//! - `sender.c:120` - `receive_sums()` reads signature blocks

use std::io::{self, Read, Write};
use std::time::Instant;

use logging::debug_log;
use protocol::CompatibilityFlags;
use protocol::codec::{NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, NdxCodecEnum, create_ndx_codec};
use protocol::wire::SignatureBlock;

use super::GeneratorContext;
use super::item_flags::ItemFlags;
use crate::receiver::SumHead;

impl GeneratorContext {
    /// Sends UID/GID name-to-ID mapping lists to the receiver.
    ///
    /// When `--numeric-ids` is not set, transmits name mappings so the receiver can
    /// translate user/group names to local numeric IDs. When `--numeric-ids` is set,
    /// no mappings are sent and numeric IDs are used as-is.
    ///
    /// # Wire Format
    ///
    /// Each list contains `(varint id, byte name_len, name_bytes)*` tuples terminated
    /// by `varint 0`. With `ID0_NAMES` compat flag, an additional name for id=0
    /// follows the terminator.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2513-2514` - `if (numeric_ids <= 0 && !inc_recurse) send_id_lists(f);`
    /// - `uidlist.c:407-414` - `send_id_lists()`
    pub(crate) fn send_id_lists<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let inc_recurse = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::INC_RECURSE));

        // upstream: flist.c:2513-2514 - skip for INC_RECURSE or numeric_ids
        if inc_recurse || self.config.flags.numeric_ids {
            return Ok(());
        }

        let id0_names = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::ID0_NAMES));

        let protocol_version = self.protocol.as_u8();

        // upstream: uidlist.c:408 - send_uid_list()
        if self.config.flags.owner {
            self.uid_list.write(writer, id0_names, protocol_version)?;
        }

        // upstream: uidlist.c:412 - send_gid_list()
        if self.config.flags.group {
            self.gid_list.write(writer, id0_names, protocol_version)?;
        }

        writer.flush()
    }

    /// Sends io_error flag for protocol < 30.
    ///
    /// For protocol >= 30, errors are sent as part of the file list end marker
    /// via SAFE_FILE_LIST support instead.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2517-2518`: `write_int(f, ignore_errors ? 0 : io_error);`
    pub(super) fn send_io_error_flag<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        if self.protocol.uses_fixed_encoding() {
            // For protocol < 30, send io_error as 4-byte int
            // If ignore_errors is set, send 0 instead of actual io_error
            let value = if self.config.deletion.ignore_errors {
                0
            } else {
                self.io_error
            };
            writer.write_all(&value.to_le_bytes())?;
            writer.flush()?;
        }
        Ok(())
    }

    /// Writes NDX, iflags, and sum_head for one file.
    ///
    /// upstream: sender.c:180-187 write_ndx_and_attrs()
    pub(super) fn write_ndx_and_attrs<W: Write>(
        &self,
        writer: &mut W,
        ndx_codec: &mut NdxCodecEnum,
        ndx: i32,
        iflags: &ItemFlags,
        sum_head: &SumHead,
    ) -> io::Result<()> {
        ndx_codec.write_ndx(writer, ndx)?;
        if self.protocol.supports_iflags() {
            writer.write_all(&iflags.significant_wire_bits().to_le_bytes())?;
        }
        sum_head.write(writer)?;
        Ok(())
    }

    /// Records an I/O error, logs the appropriate warning/error, and sends
    /// MSG_NO_SEND for protocol >= 30.
    ///
    /// For `NotFound` errors, logs "file has vanished: <path>" (upstream: `FWARNING`)
    /// and sets `IOERR_VANISHED`. For other errors, logs the open failure as an error
    /// and sets `IOERR_GENERAL`.
    ///
    /// The `path_display` parameter is a pre-formatted path string to avoid
    /// borrow conflicts with `&mut self` (the path comes from `self.full_paths`).
    ///
    /// # Upstream Reference
    ///
    /// - `sender.c:354-369`: open failure handling with vanished vs general distinction
    pub(super) fn record_open_failure<W: Write>(
        &mut self,
        writer: &mut super::super::writer::ServerWriter<W>,
        ndx: i32,
        error: &io::Error,
        path_display: &str,
    ) -> io::Result<()> {
        if error.kind() == io::ErrorKind::NotFound {
            self.io_error |= super::io_error_flags::IOERR_VANISHED;
            // upstream: sender.c:358 — rprintf(c, "file has vanished: %s\n", ...)
            eprintln!(
                "file has vanished: {path_display} {}{}",
                error_location!(),
                crate::role_trailer::generator()
            );
        } else {
            self.io_error |= super::io_error_flags::IOERR_GENERAL;
            // upstream: sender.c:362 — rsyserr(FERROR_XFER, errno, "send_files failed to open %s", ...)
            eprintln!(
                "rsync: send_files failed to open \"{path_display}\": {} ({}) {}{}",
                error,
                error.raw_os_error().unwrap_or(0),
                error_location!(),
                crate::role_trailer::generator(),
            );
        }
        if self.protocol.supports_generator_messages() {
            writer.send_no_send(ndx)?;
        }
        Ok(())
    }

    /// Emits a MSG_INFO frame with itemize output when conditions are met.
    ///
    /// Sends the formatted itemize string (`"%i %n%L\n"`) as a MSG_INFO
    /// multiplexed message to the client. This is only done when:
    /// - The server is in daemon/SSH mode (not client mode)
    /// - The client requested itemize output (`-i`/`--itemize-changes`)
    /// - The file index is valid
    ///
    /// # Upstream Reference
    ///
    /// - `sender.c:287` - `maybe_log_item()` for non-transfer items
    /// - `sender.c:430` - `log_item()` after file transfer
    /// - `log.c:330-340` - `rwrite()` converts FCLIENT/FINFO to `send_msg(MSG_INFO)`
    ///   when `am_server` is true
    pub(super) fn maybe_emit_itemize<W: Write>(
        &self,
        writer: &mut super::super::writer::ServerWriter<W>,
        iflags: &super::item_flags::ItemFlags,
        ndx: usize,
    ) -> io::Result<()> {
        // Only emit in server mode (daemon or SSH) when the client requested itemize
        if self.config.connection.client_mode {
            return Ok(());
        }
        if !self.config.flags.info_flags.itemize {
            return Ok(());
        }
        if ndx >= self.file_list.len() {
            return Ok(());
        }

        let entry = &self.file_list[ndx];
        let ctx = self.itemize_context();
        // Generator role is always the sender side
        let line = super::itemize::format_itemize_line(iflags, entry, true, &ctx);
        writer.send_message(protocol::MessageCode::Info, line.as_bytes())
    }

    /// Sends NDX_FLIST_EOF if incremental recursion is enabled.
    ///
    /// This signals to the receiver that there are no more incremental file lists.
    /// For a simple (non-recursive directory) transfer, `send_dir_ndx` is -1, so we
    /// always send `NDX_FLIST_EOF` when INC_RECURSE is enabled.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2534-2545` in `send_file_list()`:
    ///   ```c
    ///   if (inc_recurse) {
    ///       if (send_dir_ndx < 0) {
    ///           write_ndx(f, NDX_FLIST_EOF);
    ///           flist_eof = 1;
    ///       }
    ///   }
    ///   ```
    pub(super) fn send_flist_eof_if_inc_recurse<W: Write>(
        &mut self,
        writer: &mut W,
    ) -> io::Result<()> {
        if self.incremental.flist_eof_sent {
            return Ok(());
        }
        if let Some(flags) = self.compat_flags
            && flags.contains(CompatibilityFlags::INC_RECURSE)
        {
            let mut ndx_codec = create_ndx_codec(self.protocol.as_u8());
            ndx_codec.write_ndx(writer, NDX_FLIST_EOF)?;
            writer.flush()?;
            self.incremental.flist_eof_sent = true;
        }
        Ok(())
    }

    /// Sends the file list to the receiver.
    ///
    /// Encodes file entries using the configured `FileListWriter`, writes them to
    /// the wire, appends the io_error marker if any errors were accumulated during
    /// building, and caches the writer for INC_RECURSE sub-list continuation.
    ///
    /// When INC_RECURSE is active, only the initial segment is sent here; remaining
    /// per-directory segments are dispatched by [`send_extra_file_lists`](Self::send_extra_file_lists).
    ///
    /// Returns the total file list length (all segments).
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2192` - `send_file_list()` main entry point
    /// - `flist.c:2518` - `write_int(f, io_error)` end marker with SAFE_FILE_LIST
    pub fn send_file_list<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<usize> {
        // upstream: stats.flist_xfertime
        self.timing.flist_xfer_start = Some(Instant::now());

        let mut flist_writer = self.build_flist_writer();

        // When INC_RECURSE, only send initial segment entries; the rest
        // are sent via send_extra_file_lists() during the transfer loop.
        let entries_to_send = if let Some(count) = self.incremental.initial_segment_count {
            &self.file_list[..count]
        } else {
            &self.file_list
        };
        for entry in entries_to_send {
            flist_writer.write_entry(writer, entry)?;
        }

        // upstream: flist.c:2518 - write io_error with end marker (SAFE_FILE_LIST)
        let io_error_for_end = if self.io_error != 0 {
            Some(self.io_error)
        } else {
            None
        };
        flist_writer.write_end(writer, io_error_for_end)?;
        writer.flush()?;

        // upstream: flist.c:send_file_entry() uses static variables - cache writer
        // to preserve compression state across sub-lists.
        self.incremental.flist_writer_cache = Some(flist_writer);

        self.timing.flist_xfer_end = Some(Instant::now());

        Ok(self.file_list.len())
    }

    /// Sends pending file list segments during the transfer loop.
    ///
    /// Called before reading each NDX request from the receiver. Since we eagerly
    /// scanned all files, this sends all pending segments at once on the first call.
    /// Subsequent calls are no-ops.
    ///
    /// # Upstream Reference
    ///
    /// - `sender.c:send_files()` line 250: `send_extra_file_list(f, MIN_FILECNT_LOOKAHEAD)`
    /// - `flist.c:send_extra_file_list()` — sends one directory's entries
    pub(super) fn send_extra_file_lists<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if !self.inc_recurse()
            || self.incremental.flist_eof_sent
            || self.incremental.pending_segments.is_empty()
        {
            return Ok(());
        }

        // Reuse the cached writer from send_file_list() to preserve compression
        // state across sub-lists, matching upstream's static variables in
        // send_file_entry() (prev_name, prev_mode, prev_uid, prev_gid).
        let mut flist_writer = self
            .incremental
            .flist_writer_cache
            .take()
            .unwrap_or_else(|| self.build_flist_writer());

        // Send all pending segments. Since we eagerly scanned, send them all at once
        // (upstream uses MIN_FILECNT_LOOKAHEAD=1000 for throttling with lazy scanning).
        let segments = std::mem::take(&mut self.incremental.pending_segments);
        let mut ndx_codec = create_ndx_codec(self.protocol.as_u8());

        for segment in &segments {
            if segment.count == 0 {
                continue;
            }

            // Build ndx_segments entry for this sub-list.
            // upstream: flist.c:2931 — flist->ndx_start = prev->ndx_start + prev->used + 1
            let &(prev_flat_start, prev_ndx_start) = self
                .incremental
                .ndx_segments
                .last()
                .expect("initial segment exists");
            let prev_used = (segment.flist_start - prev_flat_start) as i32;
            let seg_ndx_start = prev_ndx_start + prev_used + 1;
            self.incremental
                .ndx_segments
                .push((segment.flist_start, seg_ndx_start));

            // Write NDX_FLIST_OFFSET - dir_ndx to signal a new sub-list
            ndx_codec.write_ndx(writer, NDX_FLIST_OFFSET - segment.parent_dir_ndx)?;

            // Write file entries from the reordered file_list
            let end = segment.flist_start + segment.count;
            for entry in &self.file_list[segment.flist_start..end] {
                flist_writer.write_entry(writer, entry)?;
            }

            // Write end-of-flist marker (zero byte)
            flist_writer.write_end(writer, None)?;

            debug_log!(
                Flist,
                2,
                "sent sub-list for dir_ndx={}, {} entries (ndx_start={})",
                segment.parent_dir_ndx,
                segment.count,
                seg_ndx_start
            );
        }

        // All segments sent — send NDX_FLIST_EOF
        ndx_codec.write_ndx(writer, NDX_FLIST_EOF)?;
        writer.flush()?;
        self.incremental.flist_eof_sent = true;
        debug_log!(
            Flist,
            2,
            "sent NDX_FLIST_EOF, all {} sub-lists dispatched",
            segments.len()
        );

        Ok(())
    }
}

/// Reads signature blocks from the receiver.
///
/// After reading sum_head, this reads the rolling and strong checksums for each block.
/// When sum_head.count is 0, returns an empty Vec (whole-file transfer).
///
/// # Upstream Reference
///
/// - `sender.c:120` - `receive_sums()` reads signature blocks
/// - `match.c:395` - Block format: rolling_sum (4 bytes) + strong_sum (s2length bytes)
pub fn read_signature_blocks<R: Read>(
    reader: &mut R,
    sum_head: &SumHead,
) -> io::Result<Vec<SignatureBlock>> {
    if sum_head.is_empty() {
        // No basis file (count=0), whole-file transfer - no blocks to read
        return Ok(Vec::new());
    }

    let mut blocks = Vec::with_capacity(sum_head.count as usize);

    for i in 0..sum_head.count {
        // Read rolling checksum (4 bytes LE)
        let mut rolling_bytes = [0u8; 4];
        reader.read_exact(&mut rolling_bytes)?;
        let rolling_sum = u32::from_le_bytes(rolling_bytes);

        // Read strong checksum (s2length bytes)
        let mut strong_sum = vec![0u8; sum_head.s2length as usize];
        reader.read_exact(&mut strong_sum)?;

        blocks.push(SignatureBlock {
            index: i,
            rolling_sum,
            strong_sum,
        });
    }

    Ok(blocks)
}

/// Calculates duration in milliseconds between two optional timestamps.
///
/// Returns 0 if either timestamp is `None`.
///
/// # Usage
///
/// Used for calculating `flist_buildtime` and `flist_xfertime` statistics
/// sent to the client during protocol finalization.
#[must_use]
pub fn calculate_duration_ms(start: Option<Instant>, end: Option<Instant>) -> u64 {
    match (start, end) {
        (Some(s), Some(e)) => e.duration_since(s).as_millis() as u64,
        _ => 0,
    }
}
