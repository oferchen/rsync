//! Protocol I/O operations for the generator role.
//!
//! Handles file list transmission (`send_file_list`, `encode_and_send_segment`),
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

use logging::{PhaseTimer, debug_log};
use protocol::CompatibilityFlags;
use protocol::codec::{NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, NdxCodecEnum};
use protocol::wire::SignatureBlock;

use crate::role_trailer::error_location;

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

        // upstream: flist.c:2513-2514
        if inc_recurse || self.config.flags.numeric_ids {
            return Ok(());
        }

        let id0_names = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::ID0_NAMES));
        let protocol_version = self.protocol.as_u8();

        if self.config.flags.owner {
            self.uid_list.write(writer, id0_names, protocol_version)?;
        }
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
            // upstream: flist.c:2517-2518
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
        ndx_codec: &mut impl NdxCodec,
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
            // upstream: sender.c:358 - rprintf(c, "file has vanished: %s\n", ...)
            eprintln!(
                "file has vanished: {path_display} {}{}",
                error_location!(),
                crate::role_trailer::generator()
            );
        } else {
            self.io_error |= super::io_error_flags::IOERR_GENERAL;
            // upstream: sender.c:362 - rsyserr(FERROR_XFER, errno, "send_files failed to open %s", ...)
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

    /// Emits itemize output when conditions are met.
    ///
    /// In server mode (daemon/SSH), sends the formatted itemize string
    /// (`"%i %n%L\n"`) as a MSG_INFO multiplexed message to the client.
    /// In client mode, writes directly to the process stdout via the
    /// itemize callback, matching upstream's `rwrite()` `FCLIENT` path.
    ///
    /// # Upstream Reference
    ///
    /// - `sender.c:287` - `maybe_log_item()` for non-transfer items
    /// - `sender.c:430` - `log_item()` after file transfer
    /// - `log.c:330-340` - `rwrite()`: when `am_server`, sends MSG_INFO;
    ///   when `!am_server`, writes to stdout (FCLIENT)
    pub(super) fn maybe_emit_itemize<W: Write>(
        &self,
        writer: &mut super::super::writer::ServerWriter<W>,
        iflags: &super::item_flags::ItemFlags,
        ndx: usize,
        itemize_cb: &mut Option<&mut dyn super::super::ItemizeCallback>,
    ) -> io::Result<()> {
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

        if self.config.connection.client_mode {
            // upstream: log.c:330-340 - when !am_server, rwrite() sends to FCLIENT (stdout)
            if let Some(cb) = itemize_cb.as_mut() {
                cb.on_itemize(&line);
            }
            Ok(())
        } else {
            writer.send_message(protocol::MessageCode::Info, line.as_bytes())
        }
    }

    /// Sends the file list to the receiver.
    ///
    /// Encodes file entries using the configured `FileListWriter`, writes them to
    /// the wire, appends the io_error marker if any errors were accumulated during
    /// building, and caches the writer for INC_RECURSE sub-list continuation.
    ///
    /// When INC_RECURSE is active, only the initial segment is sent here; remaining
    /// per-directory segments are dispatched by `encode_and_send_segment` via the
    /// `SegmentScheduler` during the transfer loop.
    ///
    /// Returns the total file list length (all segments).
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2192` - `send_file_list()` main entry point
    /// - `flist.c:2518` - `write_int(f, io_error)` end marker with SAFE_FILE_LIST
    pub fn send_file_list<W: Write>(&mut self, writer: &mut W) -> io::Result<usize> {
        let _t = PhaseTimer::new("file-list-send");
        // upstream: stats.flist_xfertime
        self.timing.flist_xfer_start = Some(Instant::now());

        let mut flist_writer = self.build_flist_writer();

        // Set first_ndx so the writer can distinguish abbreviated vs
        // unabbreviated hardlink followers (leader in same vs previous segment).
        // upstream: flist.c:send_file_entry() uses first_ndx parameter
        let initial_ndx_start = self
            .incremental
            .ndx_segments
            .first()
            .map_or(0, |&(_, ndx_start)| ndx_start);
        flist_writer.set_first_ndx(initial_ndx_start);

        // When INC_RECURSE, only send initial segment entries; the rest
        // are sent via the SegmentScheduler during the transfer loop.
        let count = self
            .incremental
            .initial_segment_count
            .unwrap_or(self.file_list.len());
        for i in 0..count {
            let entry = &self.file_list[i];
            self.prepare_pending_acl(entry, i, &mut flist_writer);
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

    /// Encodes and sends a single file list sub-segment to the wire.
    ///
    /// Wire format per upstream `flist.c:send_extra_file_list()`:
    /// 1. Write `NDX_FLIST_OFFSET - parent_dir_ndx` (varint)
    /// 2. Write file entries for this segment
    /// 3. Write end-of-list marker (0 byte)
    /// 4. Update NDX translation table
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:send_extra_file_list()` - sends one directory's entries
    /// - `flist.c:2931` - `ndx_start = prev->ndx_start + prev->used + 1`
    pub(super) fn encode_and_send_segment<W: Write>(
        &mut self,
        writer: &mut W,
        segment: &super::PendingSegment,
        flist_writer: &mut protocol::flist::FileListWriter,
        ndx_codec: &mut NdxCodecEnum,
    ) -> io::Result<()> {
        if segment.count == 0 {
            return Ok(());
        }

        // Compute ndx_start for this sub-list.
        // upstream: flist.c:2931 - flist->ndx_start = prev->ndx_start + prev->used + 1
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

        // Signal new sub-list to receiver.
        ndx_codec.write_ndx(writer, NDX_FLIST_OFFSET - segment.parent_dir_ndx)?;

        // Set first_ndx so abbreviated vs unabbreviated followers are
        // correctly distinguished for this segment.
        // upstream: flist.c:send_file_entry() line 572
        flist_writer.set_first_ndx(seg_ndx_start);

        // Write file entries from the reordered file_list.
        let end = segment.flist_start + segment.count;
        for i in segment.flist_start..end {
            let entry = &self.file_list[i];
            self.prepare_pending_acl(entry, i, flist_writer);
            flist_writer.write_entry(writer, entry)?;
        }

        // End-of-flist marker (zero byte).
        flist_writer.write_end(writer, None)?;

        debug_log!(
            Flist,
            2,
            "sent sub-list for dir_ndx={}, {} entries (ndx_start={})",
            segment.parent_dir_ndx,
            segment.count,
            seg_ndx_start
        );

        Ok(())
    }

    /// Reads filesystem ACLs and sets them on the writer for the next entry.
    ///
    /// When `--acls` is enabled and the entry is not a symlink, reads the
    /// real access ACL (and default ACL for directories) from the filesystem
    /// and passes them to the writer via `set_pending_acl`. The writer will
    /// strip base permission entries before sending.
    ///
    /// # Upstream Reference
    ///
    /// Mirrors `flist.c:1627-1656` where `get_acl()` reads filesystem ACLs
    /// and `send_acl()` strips and sends them.
    fn prepare_pending_acl(
        &self,
        entry: &protocol::flist::FileEntry,
        index: usize,
        flist_writer: &mut protocol::flist::FileListWriter,
    ) {
        if !self.config.flags.acls || entry.is_symlink() {
            return;
        }

        let full_path = &self.full_paths[index];
        let mode = entry.mode();

        // upstream: acls.c:560-561 - read access ACL
        let access_acl = metadata::get_rsync_acl(full_path, mode, false);

        // upstream: acls.c:566-569 - read default ACL for directories
        let default_acl = if entry.is_dir() {
            Some(metadata::get_rsync_acl(full_path, mode, true))
        } else {
            None
        };

        flist_writer.set_pending_acl(access_acl, default_acl);
    }

    /// Sends `NDX_FLIST_EOF` and flushes, marking incremental sending as complete.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2534-2545` - NDX_FLIST_EOF dispatch
    pub(super) fn send_flist_eof<W: Write>(
        &mut self,
        writer: &mut W,
        ndx_codec: &mut NdxCodecEnum,
        segments_sent: usize,
    ) -> io::Result<()> {
        ndx_codec.write_ndx(writer, NDX_FLIST_EOF)?;
        writer.flush()?;
        self.incremental.flist_eof_sent = true;
        debug_log!(
            Flist,
            2,
            "sent NDX_FLIST_EOF, all {} sub-lists dispatched",
            segments_sent
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
