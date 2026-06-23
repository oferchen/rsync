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

use std::io::{self, IoSlice, Read, Write};
use std::time::{Duration, Instant};

use logging::{InfoFlag, PhaseTimer, debug_log, info_gte, info_log};
use protocol::CompatibilityFlags;
use protocol::codec::{NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, NdxCodecEnum};
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

    /// Reads the generator's xattr abbreviation request when `ITEM_REPORT_XATTR`
    /// is set in iflags and xattrs are preserved.
    ///
    /// The generator emits a varint stream of delta-encoded 1-based entry
    /// numbers terminated by `0`, even when no entries are flagged for
    /// transfer. Consuming this stream is mandatory whenever the gating
    /// condition holds, otherwise the subsequent `sum_head` read picks up the
    /// terminator byte and the wire stream desynchronises - the failure
    /// observed under `-X --fake-super` where small-value xattr counts differ
    /// between the sender and receiver sides.
    ///
    /// Returns the per-file `XattrList` with `XSTATE_TODO` entries marked for
    /// the indices the generator requested, ready to be passed to
    /// [`Self::write_ndx_and_attrs`] or
    /// [`Self::write_ndx_iflags_and_xattr_response`]. Returns `None` when no
    /// xattr request is expected on the wire.
    ///
    /// # Upstream Reference
    ///
    /// - `sender.c:280-284` - `recv_xattr_request()` called when
    ///   `preserve_xattrs && iflags & ITEM_REPORT_XATTR && do_xfers
    ///    && !(want_xattr_optim && BITS_SET(iflags, ITEM_XNAME_FOLLOWS|ITEM_LOCAL_CHANGE))`
    /// - `xattrs.c:681-758` - `recv_xattr_request()` sender path marks entries
    ///   `XSTATE_TODO` for items the generator wants the full value of.
    pub(super) fn read_generator_xattr_request_if_any<R: Read>(
        &self,
        reader: &mut R,
        ndx: usize,
        iflags: &ItemFlags,
    ) -> io::Result<Option<protocol::xattr::XattrList>> {
        if !self.config.flags.xattrs {
            return Ok(None);
        }
        if iflags.raw() & ItemFlags::ITEM_REPORT_XATTR == 0 {
            return Ok(None);
        }
        // upstream: sender.c:281 also gates on the want_xattr_optim hardlink
        // optimisation. We mirror want_xattr_optim via the negotiated
        // CF_AVOID_XATTR_OPTIM capability flag - when the flag is NOT set,
        // upstream's want_xattr_optim is active and the optimisation skips
        // xattr exchange for local-change hardlinks.
        let want_xattr_optim = self
            .compat_flags
            .is_none_or(|f| !f.contains(CompatibilityFlags::AVOID_XATTR_OPTIMIZATION));
        if want_xattr_optim
            && (iflags.raw() & ItemFlags::ITEM_XNAME_FOLLOWS != 0)
            && (iflags.raw() & ItemFlags::ITEM_LOCAL_CHANGE != 0)
        {
            return Ok(None);
        }

        // Build the per-file xattr list by cloning the sender-side file
        // entry's cached xattrs so the TODO marks do not poison the cached
        // flist xattrs. When the file entry has no xattrs the list is
        // empty; the generator's request must still be drained because it
        // emits at least the 0 terminator under this gate.
        let mut list = if ndx < self.file_list.len() {
            self.file_list[ndx]
                .xattr_list()
                .cloned()
                .unwrap_or_default()
        } else {
            protocol::xattr::XattrList::new()
        };

        // upstream: xattrs.c:681 recv_xattr_request() marks XSTATE_TODO on
        // matching entries and consumes the stream up to the 0 terminator.
        let _indices = protocol::xattr::recv_xattr_request(reader, &mut list)?;
        Ok(Some(list))
    }

    /// Writes NDX, iflags, optional xattr response, and sum_head for one file.
    ///
    /// Combines upstream's `write_ndx_and_attrs()` (NDX + iflags + optional
    /// xattr_request body) with the immediately following `write_sum_head()`
    /// call. When `xattr_response` is `Some` and the file has entries the
    /// generator requested, the full xattr values are written between iflags
    /// and sum_head, matching the byte order of upstream sender.c:411-412.
    ///
    /// # Upstream Reference
    ///
    /// - `sender.c:180-196` - `write_ndx_and_attrs()` body (calls
    ///   `send_xattr_request(fname, file, f_out)` when ITEM_REPORT_XATTR set)
    /// - `sender.c:411-412` - `write_ndx_and_attrs()` followed by
    ///   `write_sum_head(f_xfer, s)`
    pub(super) fn write_ndx_and_attrs<W: Write>(
        &self,
        writer: &mut W,
        ndx_codec: &mut impl NdxCodec,
        ndx: i32,
        iflags: &ItemFlags,
        sum_head: &SumHead,
        xattr_response: Option<&mut protocol::xattr::XattrList>,
    ) -> io::Result<()> {
        self.write_ndx_iflags_and_xattr_response(writer, ndx_codec, ndx, iflags, xattr_response)?;
        sum_head.write(writer)?;
        Ok(())
    }

    /// Writes NDX, iflags, and optional xattr response without a sum_head.
    ///
    /// Used for non-transfer items (no `ITEM_TRANSFER` in iflags) and the
    /// `--dry-run` echo path, where upstream calls `write_ndx_and_attrs()`
    /// without a following `write_sum_head()`. When `xattr_response` is
    /// `Some` with entries flagged via [`XattrState::Todo`], the full values
    /// are written immediately after iflags via
    /// [`send_sender_xattr_response`](protocol::xattr::send_sender_xattr_response).
    ///
    /// # Upstream Reference
    ///
    /// - `sender.c:286-292` - non-transfer echo of NDX + iflags + xattr_request
    /// - `xattrs.c:623-675` - `send_xattr_request()` sender path
    ///
    /// [`XattrState::Todo`]: protocol::xattr::XattrState::Todo
    pub(super) fn write_ndx_iflags_and_xattr_response<W: Write>(
        &self,
        writer: &mut W,
        ndx_codec: &mut impl NdxCodec,
        ndx: i32,
        iflags: &ItemFlags,
        xattr_response: Option<&mut protocol::xattr::XattrList>,
    ) -> io::Result<()> {
        ndx_codec.write_ndx(writer, ndx)?;
        if self.protocol.supports_iflags() {
            writer.write_all(&iflags.significant_wire_bits().to_le_bytes())?;
        }
        // upstream: sender.c:192-196 - send_xattr_request(fname, file, f_out)
        // is invoked from inside write_ndx_and_attrs() when ITEM_REPORT_XATTR
        // is set in iflags. Skipping this body causes the receiver to read the
        // following bytes as a stale xattr request, desyncing the goodbye phase.
        let preserve_xattrs = self.config.flags.xattrs;
        if preserve_xattrs
            && (iflags.raw() & ItemFlags::ITEM_REPORT_XATTR != 0)
            && let Some(list) = xattr_response
        {
            protocol::xattr::send_sender_xattr_response(writer, list)?;
        }
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
            // upstream: sender.c:358 - rprintf(c, "file has vanished: %s\n", full_fname(...))
            eprintln!("file has vanished: \"{path_display}\"");
        } else {
            self.io_error |= super::io_error_flags::IOERR_GENERAL;
            // upstream: sender.c:362 - rsyserr(FERROR_XFER, errno, "send_files failed to open %s", ...)
            eprintln!(
                "rsync: [sender] send_files failed to open \"{path_display}\": {} ({})",
                error,
                error.raw_os_error().unwrap_or(0),
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
    /// Upstream `generator.c:582-583` emits when ANY of four OR'd conditions
    /// hold: significant flags set, `INFO_GTE(NAME, 2)`, `stdout_format_has_i
    /// > 1`, or an alternate basis name follows. Mirror the same semantic so
    /// unchanged entries (`iflags == 0`) still appear under `-vv` (the case
    /// the upstream `itemize.test` testsuite exercises with `-ivvplrtH`).
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:582-583` - emit gate: `(iflags & (SIGNIFICANT_ITEM_FLAGS
    ///   | ITEM_REPORT_XATTR)) || INFO_GTE(NAME, 2) || stdout_format_has_i > 1
    ///   || (xname && *xname)`
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
        // upstream: generator.c:582-583 - the gate is the OR of four
        // conditions. Significant flags is the common case; INFO_GTE(NAME, 2)
        // makes `-vv` surface unchanged entries; ITEM_XNAME_FOLLOWS forces
        // emission when an alternate basis name trails. `stdout_format_has_i
        // > 1` is the `-ii` "show even unchanged" knob; oc-rsync does not
        // distinguish single vs double `-i` yet, so that fourth condition is
        // omitted here and the other three are honored.
        let force_emit =
            info_gte(InfoFlag::Name, 2) || iflags.raw() & ItemFlags::ITEM_XNAME_FOLLOWS != 0;
        if !iflags.has_significant_flags() && !force_emit {
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
            // upstream: log.c:822-823 - when !am_server, rwrite() sends FCLIENT
            // to stdout. In a pull the local client is the receiver and this
            // Generator(sender) callback path surfaces the client-side row.
            if let Some(cb) = itemize_cb.as_mut() {
                cb.on_itemize(&line);
            }
            Ok(())
        } else {
            // upstream: sender.c:215 - `itemizing = am_server ?
            // logfile_format_has_i : stdout_format_has_i`. On a server-sender
            // (am_server) with no `--log-file-format`, logfile_format_has_i == 0,
            // so the sender emits no client-visible itemize at all:
            // maybe_log_item (log.c:828-843) only ever reaches
            // log_item(FLOG, ...), a local `--log-file` artifact, never an
            // MSG_INFO forward to the client. The client-visible itemize is
            // owned by the receiver-side generator (receiver/mod.rs::emit_itemize).
            // Forwarding a sender-direction row here duplicated every
            // transferred file under `-ii`/`-vv` over a remote shell (the
            // upstream `exclude-lsh` pull leg emitted both `>f` and `<f`).
            let _ = writer;
            Ok(())
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
        let entry_instant = Instant::now();
        self.timing.flist_xfer_start = Some(entry_instant);

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

        // Wrap the wire writer in a first-byte latency probe. Cost is one
        // branch per buffered write call - no per-entry sampling.
        // upstream: flist.c send_file_list / send_dir_name first-byte timing
        let mut probed = FirstByteWriter::new(writer, entry_instant);

        // When INC_RECURSE, only send initial segment entries; the rest
        // are sent via the SegmentScheduler during the transfer loop.
        let count = self
            .incremental
            .initial_segment_count
            .unwrap_or(self.file_list.len());
        for i in 0..count {
            let entry = &self.file_list[i];
            self.prepare_pending_acl(entry, i, &mut flist_writer);
            flist_writer.write_entry(&mut probed, entry)?;
        }

        // upstream: flist.c:2518 - write io_error with end marker (SAFE_FILE_LIST)
        let io_error_for_end = if self.io_error != 0 {
            Some(self.io_error)
        } else {
            None
        };
        flist_writer.write_end(&mut probed, io_error_for_end)?;
        probed.flush()?;

        let first_byte_latency = probed.first_byte_latency();

        // upstream: flist.c:send_file_entry() uses static variables - cache writer
        // to preserve compression state across sub-lists.
        self.incremental.flist_writer_cache = Some(flist_writer);

        self.timing.flist_xfer_end = Some(Instant::now());
        self.timing.flist_first_byte_latency = first_byte_latency;

        if let Some(latency) = first_byte_latency {
            // INC_RECURSE diagnostic I1: time from function entry to first
            // wire byte. Surfaced at -vv (info=flist1) and -v --info=stats3.
            info_log!(
                Flist,
                1,
                "send_file_list first-byte latency: {} us",
                latency.as_micros()
            );
            info_log!(
                Stats,
                3,
                "file list first-byte latency: {} us",
                latency.as_micros()
            );
            debug_log!(
                Flist,
                2,
                "send_file_list first-byte latency: {:?} ({} entries)",
                latency,
                count
            );
            #[cfg(feature = "tracing")]
            ::tracing::info!(
                target: "rsync::flist",
                latency_us = latency.as_micros() as u64,
                entries = count,
                "send_file_list first-byte latency"
            );
        }

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
    /// Updates the `SEGMENT_DISPATCH_CALLS` / `SEGMENT_DISPATCH_ELAPSED_NS`
    /// counters used for INC_RECURSE diagnostic I2 (#2197). The timer wraps
    /// the entire body so the zero-entry early-return path is still accounted
    /// for in the per-transfer dispatch count.
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
        let started = Instant::now();
        let result = self.encode_and_send_segment_inner(writer, segment, flist_writer, ndx_codec);
        super::record_segment_dispatch(started.elapsed());
        result
    }

    /// Body of [`Self::encode_and_send_segment`] without the timing wrapper,
    /// so the counter updates remain in a single place and the elapsed window
    /// excludes only the `Instant::now()` pair itself.
    ///
    /// Upstream always sends the NDX header and end-of-flist marker even for
    /// empty directories (flist.c:2117,2139-2146). Skipping them for count==0
    /// desynchronises `flist_done_remaining` from the receiver's NDX_DONE
    /// stream, causing a phase-transition NDX_DONE to be consumed as a
    /// flist-free echo.
    fn encode_and_send_segment_inner<W: Write>(
        &mut self,
        writer: &mut W,
        segment: &super::PendingSegment,
        flist_writer: &mut protocol::flist::FileListWriter,
        ndx_codec: &mut NdxCodecEnum,
    ) -> io::Result<()> {
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
        // upstream: flist.c:2117 - write_ndx(f, NDX_FLIST_OFFSET - dir_ndx)
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
        // upstream: flist.c:2139-2146 - always sends write_end_of_flist()
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
    /// Updates the `PREPARE_ACL_CALLS` / `PREPARE_ACL_ELAPSED_NS` counters
    /// used for INC_RECURSE diagnostic I5 (#2200). The timer wraps the entire
    /// body so the no-op early-return path is still accounted for in the
    /// per-segment call count.
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
        let started = Instant::now();
        self.prepare_pending_acl_inner(entry, index, flist_writer);
        super::record_prepare_acl(started.elapsed());
    }

    /// Body of [`Self::prepare_pending_acl`] without the timing wrapper, so
    /// the counter updates remain in a single place and the elapsed window
    /// excludes only the `Instant::now()` pair itself.
    fn prepare_pending_acl_inner(
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

/// `Write` adapter that records the elapsed time until the first non-empty
/// write reaches the underlying writer.
///
/// Used by [`GeneratorContext::send_file_list`] to instrument the gap between
/// function entry and the first wire byte. This is INC_RECURSE diagnostic
/// counter I1 (#2196) - the receiver-visible startup latency of the file
/// list stream.
///
/// upstream: flist.c send_file_list / send_dir_name first-byte timing
struct FirstByteWriter<'w, W: Write> {
    inner: &'w mut W,
    entry: Instant,
    first_byte_latency: Option<Duration>,
}

impl<'w, W: Write> FirstByteWriter<'w, W> {
    fn new(inner: &'w mut W, entry: Instant) -> Self {
        Self {
            inner,
            entry,
            first_byte_latency: None,
        }
    }

    fn first_byte_latency(&self) -> Option<Duration> {
        self.first_byte_latency
    }

    fn record(&mut self, n: usize) {
        if n > 0 && self.first_byte_latency.is_none() {
            self.first_byte_latency = Some(self.entry.elapsed());
        }
    }
}

impl<W: Write> Write for FirstByteWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.record(n);
        Ok(n)
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let n = self.inner.write_vectored(bufs)?;
        self.record(n);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
