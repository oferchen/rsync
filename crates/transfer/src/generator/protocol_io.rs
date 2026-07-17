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
use protocol::ProtocolVersion;
use protocol::codec::{NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, NdxCodecEnum};
use protocol::wire::SignatureBlock;

use super::GeneratorContext;
use super::item_flags::ItemFlags;

/// Per-file NDX + item-flags header that precedes a file's `sum_head` / delta
/// payload on the wire.
///
/// Bundles the NDX with the iflags-gated trailing fields that always travel
/// together (upstream `sender.c:180-189`): `fnamecmp_type` is emitted when
/// `iflags.has_basis_type()` and `xname` when `iflags.has_xname()`. Grouping
/// them as a single parameter object keeps the two writer methods below at a
/// manageable arity and prevents the four fields from drifting apart at call
/// sites.
pub(super) struct NdxAttrs<'a> {
    /// Wire NDX of the file (diff-encoded by the NDX codec).
    pub ndx: i32,
    /// 16-bit item flags; the `*_FOLLOWS` bits gate the two fields below.
    pub iflags: &'a ItemFlags,
    /// fnamecmp basis type, written only when `iflags.has_basis_type()`.
    pub fnamecmp_type: Option<protocol::FnameCmpType>,
    /// Extended name, written as a vstring only when `iflags.has_xname()`.
    pub xname: Option<&'a [u8]>,
}
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

        // upstream: flist.c:2548 - `if (numeric_ids <= 0 && !inc_recurse)
        // send_id_lists(f);`. The list stays on the wire for `numeric_ids <= 0`
        // (Off and daemon-forced -1); only an explicit client --numeric-ids
        // (`> 0`) drops it. Under daemon-forced numeric-ids the list is sent but
        // empty, since add_uid()/add_gid() are gated on `!numeric_ids`.
        if inc_recurse || self.config.flags.numeric_ids.is_explicit() {
            return Ok(());
        }

        let id0_names = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::ID0_NAMES));
        let protocol_version = self.protocol.as_u8();

        // upstream: uidlist.c:409,412 - send the uid/gid list when preserving
        // ownership OR ACLs. `--acls` injects named-entry ids into the same
        // list (see collect_acl_id_mappings), so the receiver can remap them.
        if self.config.flags.owner || self.config.flags.acls {
            self.uid_list.write(writer, id0_names, protocol_version)?;
        }
        if self.config.flags.group || self.config.flags.acls {
            self.gid_list.write(writer, id0_names, protocol_version)?;
        }

        writer.flush()
    }

    /// Feeds named ACL-entry user/group ids into the shared uid/gid id-list.
    ///
    /// Mirrors upstream `send_ida_entries()` (`acls.c:592-595`), which calls
    /// `add_uid(ida->id)`/`add_gid(ida->id)` for every named ACL entry (unless
    /// `numeric_ids`) so the receiver remaps those ids through the same table as
    /// file owners via `match_acl_ids()` (`uidlist.c:483-484`).
    ///
    /// Must run after the file list is sent (so the ACL cache is fully
    /// populated) and before [`Self::send_id_lists`]. No-op under `numeric_ids`
    /// or when the sender-side ACL cache is unavailable.
    #[cfg(unix)]
    pub(crate) fn collect_acl_id_mappings(&mut self) {
        use metadata::id_lookup::{lookup_group_name_cached, lookup_user_name_cached};

        // upstream: acls.c:593,595 - `name = numeric_ids ? NULL : add_uid(...)`;
        // the named-entry id is added (and later mapped) only when names are in
        // play, i.e. `numeric_ids == 0`. Both daemon-forced and explicit
        // numeric-ids suppress it (`numeric_ids != 0`).
        if self.config.flags.numeric_ids.maps_numeric() || !self.config.flags.acls {
            return;
        }

        let Some(writer) = self.incremental.flist_writer_cache.as_ref() else {
            return;
        };

        // Snapshot the (id, is_user) pairs so the borrow of the cache ends
        // before we mutate the id-lists.
        let named: Vec<(u32, bool)> = writer
            .acl_cache()
            .iter_acls()
            .flat_map(|acl| acl.names.iter())
            .map(|ida| (ida.id, ida.is_user()))
            .collect();

        for (id, is_user) in named {
            if is_user {
                if !self.uid_list.contains(id) {
                    let name = lookup_user_name_cached(id).ok().flatten();
                    self.uid_list.add_id(id, name);
                }
            } else if !self.gid_list.contains(id) {
                let name = lookup_group_name_cached(id).ok().flatten();
                self.gid_list.add_id(id, name);
            }
        }
    }

    /// No-op on non-Unix platforms - ACL ids are not remapped by numeric id.
    #[cfg(not(unix))]
    pub(crate) fn collect_acl_id_mappings(&mut self) {}

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
        // optimisation. upstream: compat.c:747 -
        // `want_xattr_optim = protocol_version >= 31 && !(compat_flags & CF_AVOID_XATTR_OPTIM)`.
        // The optimisation only exists at protocol 31+, so it must stay off for
        // proto-30 peers (rsync 3.0.x) where CF_AVOID_XATTR_OPTIM is undefined -
        // otherwise the generator skips a request the sender still expects and
        // desyncs the stream.
        let want_xattr_optim = self.protocol.as_u8() >= 31
            && self
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
        attrs: &NdxAttrs<'_>,
        sum_head: &SumHead,
        xattr_response: Option<&mut protocol::xattr::XattrList>,
    ) -> io::Result<()> {
        self.write_ndx_iflags_and_xattr_response(writer, ndx_codec, attrs, xattr_response)?;
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
        attrs: &NdxAttrs<'_>,
        xattr_response: Option<&mut protocol::xattr::XattrList>,
    ) -> io::Result<()> {
        let NdxAttrs {
            ndx,
            iflags,
            fnamecmp_type,
            xname,
        } = *attrs;
        ndx_codec.write_ndx(writer, ndx)?;
        if self.protocol.supports_iflags() {
            // upstream: sender.c:184 - write_shortint(f_out, iflags) writes the
            // FULL 16-bit iflags, including the ITEM_BASIS_TYPE_FOLLOWS /
            // ITEM_XNAME_FOLLOWS framing bits. The receiver reads those bits to
            // decide whether the trailing fnamecmp_type / xname fields follow;
            // `significant_wire_bits` strips them (it exists for itemize display
            // only), so the receiver stops expecting the trailing bytes and the
            // wire desyncs - over a socket the goodbye then closes with unread
            // data and the kernel RSTs the stream.
            writer.write_all(&((iflags.raw() & 0xFFFF) as u16).to_le_bytes())?;
        }
        // upstream: sender.c:186-189 - write fnamecmp_type and the extended name
        // immediately after iflags when their *_FOLLOWS bits are set.
        if iflags.has_basis_type() {
            if let Some(ft) = fnamecmp_type {
                writer.write_all(&[ft.to_wire()])?;
            }
        }
        if iflags.has_xname() {
            // upstream: sender.c:189 write_vstring(f_out, xname, strlen(xname)).
            // The xname length prefix is a 1- or 2-byte vstring (io.c:2297), NOT
            // a varint: the two encodings only agree for len <= 0x7F, so a longer
            // fuzzy basename or hard-link leader name would desync the receiver's
            // read_vstring (io.c:2004). An empty xname still emits its 0 length
            // byte.
            protocol::write_vstring(writer, xname.unwrap_or(&[]))?;
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
    /// The `path_display` parameter is a pre-formatted path string reconstructed
    /// from the entry's interned source base (see
    /// [`GeneratorContext::reconstruct_source_path`](super::GeneratorContext::reconstruct_source_path)).
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
                "rsync: [sender] send_files failed to open \"{path_display}\": {}",
                engine::local_copy::upstream_io_error(error),
            );
        }
        if self.protocol.supports_generator_messages() {
            writer.send_no_send(ndx)?;
        }
        Ok(())
    }

    /// Skips a source that has shrunk below its file-list length in append
    /// mode, warning and sending MSG_NO_SEND for protocol >= 30.
    ///
    /// Appending only ever extends a file, so a source now shorter than the
    /// length recorded when the file list was built would corrupt the
    /// destination. Upstream refuses to send it, warns with "skipped diminished
    /// file", and (protocol >= 30) sends MSG_NO_SEND so the receiver drops the
    /// pending entry. Unlike an open failure, a diminished skip sets no
    /// `io_error` bit and does not affect the exit code.
    ///
    /// # Upstream Reference
    ///
    /// - `sender.c:421-429`: `if (append_mode > 0 && st.st_size < F_LENGTH(file))`
    ///   -> `rprintf(FWARNING, "skipped diminished file: %s\n", ...)` then
    ///   `send_msg_int(MSG_NO_SEND, ndx)`.
    pub(super) fn record_diminished_skip<W: Write>(
        &mut self,
        writer: &mut super::super::writer::ServerWriter<W>,
        ndx: i32,
        path_display: &str,
    ) -> io::Result<()> {
        // upstream: sender.c:422 - rprintf(FWARNING, "skipped diminished file: %s\n", ...)
        eprintln!("skipped diminished file: \"{path_display}\"");
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
        xname: Option<&[u8]>,
        itemize_cb: &mut Option<&mut dyn super::super::ItemizeCallback>,
    ) -> io::Result<()> {
        if !self.config.flags.info_flags.itemize {
            return Ok(());
        }
        // upstream: generator.c:582-583 - the gate is the OR of four
        // conditions. Significant flags is the common case; INFO_GTE(NAME, 2)
        // makes `-vv` surface unchanged entries; `stdout_format_has_i > 1` is
        // the `-ii` "show even unchanged" knob (tracked as
        // `info_flags.itemize_unchanged`); ITEM_XNAME_FOLLOWS forces emission
        // when an alternate basis name trails. On a push the sender owns the
        // client-visible itemize (the remote generator forwards iflags over the
        // wire, generator.c:583-599), so honoring `itemize_unchanged` here keeps
        // `-ii` unchanged rows on the sole client-side print path.
        let force_emit = info_gte(InfoFlag::Name, 2)
            || self.config.flags.info_flags.itemize_unchanged
            || iflags.raw() & ItemFlags::ITEM_XNAME_FOLLOWS != 0;
        if !iflags.has_significant_flags() && !force_emit {
            return Ok(());
        }
        if ndx >= self.file_list.len() {
            return Ok(());
        }

        let entry = &self.file_list[ndx];
        let ctx = self.itemize_context();
        // Generator role is always the sender side. The wire xname carries the
        // hard-link leader for an ITEM_XNAME_FOLLOWS follower so `%L` can append
        // the ` => leader` suffix (upstream sender.c:293 maybe_log_item passes
        // the xname buffer as the hlink arg to log_item -> log.c:643-646).
        let line = super::itemize::format_itemize_line(iflags, entry, true, &ctx, xname);

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
        // Record the owning directory's flat index so a directory itemize read
        // back at the gap NDX `seg_ndx_start - 1` resolves to the directory
        // entry rather than to the trailing file of the previous segment.
        // upstream: sender.c:269-272 - `dir_flist->files[cur_flist->parent_ndx]`.
        self.incremental
            .segment_parent_flat
            .push(segment.parent_flat_idx as i32);

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

        let full_path = self.reconstruct_source_path(index);
        let mode = entry.mode();

        // upstream: acls.c:560-561 - read access ACL
        let access_acl = metadata::get_rsync_acl(&full_path, mode, false);

        // upstream: acls.c:566-569 - read default ACL for directories
        let default_acl = if entry.is_dir() {
            Some(metadata::get_rsync_acl(&full_path, mode, true))
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
/// This is the keepalive-free path used for whole-file transfers and unit tests.
/// The live sender loop uses [`read_signature_blocks_keepalive`] so a slow read on
/// an older protocol does not trip the peer's `--timeout`.
///
/// # Upstream Reference
///
/// - `sender.c:120` - `receive_sums()` reads signature blocks
/// - `match.c:395` - Block format: rolling_sum (4 bytes) + strong_sum (s2length bytes)
pub fn read_signature_blocks<R: Read>(
    reader: &mut R,
    sum_head: &SumHead,
) -> io::Result<Vec<SignatureBlock>> {
    read_signature_blocks_keepalive(reader, sum_head, 0, || Ok(()))
}

/// Reads signature blocks from the receiver, poking a keepalive every `lull_mod`
/// blocks so a large/slow checksum read does not exceed the peer's I/O timeout.
///
/// `on_lull` is invoked once every `lull_mod` blocks (starting at block 0), or
/// never when `lull_mod` is 0. It maps to upstream's `maybe_send_keepalive()`,
/// which itself only emits an empty `MSG_DATA` frame once a full `allowed_lull`
/// has elapsed with no output - so passing a keepalive closure is wire-neutral
/// until the lull actually fires.
///
/// # Upstream Reference
///
/// - `sender.c:73` - `receive_sums()`; `lull_mod = protocol_version >= 31 ? 0 : allowed_lull * 5`
/// - `sender.c:115-116` - `if (lull_mod && !(i % lull_mod)) maybe_send_keepalive(time(NULL), True)`
/// - `io.c:1453` - `maybe_send_keepalive()` gates the actual emission on `allowed_lull`
/// - `match.c:395` - Block format: rolling_sum (4 bytes) + strong_sum (s2length bytes)
pub fn read_signature_blocks_keepalive<R, F>(
    reader: &mut R,
    sum_head: &SumHead,
    lull_mod: u32,
    mut on_lull: F,
) -> io::Result<Vec<SignatureBlock>>
where
    R: Read,
    F: FnMut() -> io::Result<()>,
{
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

        // upstream: sender.c:115-116 - if (lull_mod && !(i % lull_mod))
        //     maybe_send_keepalive(time(NULL), True);
        // Poke a keepalive at the same cadence so a long checksum read on an
        // older protocol keeps the write side alive.
        if lull_mod != 0 && i % lull_mod == 0 {
            on_lull()?;
        }
    }

    Ok(blocks)
}

/// Derives upstream's signature-read keepalive cadence for a sender.
///
/// Returns the number of blocks between keepalive pokes, or 0 to disable them.
/// Keepalives are only needed on protocols below 31; newer protocols multiplex
/// the checksum stream so the sender's read no longer starves the write side.
///
/// # Upstream Reference
///
/// - `sender.c:76` - `int lull_mod = protocol_version >= 31 ? 0 : allowed_lull * 5;`
///   (`allowed_lull` is in seconds, derived from `--timeout` at io.c:1151).
#[must_use]
pub fn signature_read_lull_mod(protocol: ProtocolVersion, allowed_lull: Option<Duration>) -> u32 {
    if protocol.as_u8() >= 31 {
        return 0;
    }
    match allowed_lull {
        Some(lull) => u32::try_from(lull.as_secs())
            .unwrap_or(u32::MAX)
            .saturating_mul(5),
        None => 0,
    }
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
