//! One marker-aware NDX reader shared by every role, mirroring upstream
//! `read_ndx_and_attrs()` (`rsync.c:322-431`).
//!
//! Upstream funnels every NDX read on a live transfer stream through a single
//! function whose loop classifies the value before any attribute byte is
//! touched. The branch order is load-bearing:
//!
//! 1. `ndx >= 0` - a real per-file index; leave the loop (`rsync.c:332-333`).
//! 2. `NDX_DONE` - return immediately, reading **no** attributes
//!    (`rsync.c:334-335`).
//! 3. `NDX_DEL_STATS` - drain the five deletion varints and continue
//!    (`rsync.c:336-342`). This sits *before* the `inc_recurse` gate, so a peer
//!    that never negotiated INC_RECURSE still drains them.
//! 4. `!inc_recurse || am_sender` - any other negative value is a protocol
//!    violation (`rsync.c:343-352`).
//! 5. `NDX_FLIST_EOF` - the sub-list stream ended; set `flist_eof` and continue
//!    (`rsync.c:353-359`).
//! 6. otherwise - `dir_ndx = NDX_FLIST_OFFSET - ndx`, receive that sub-list
//!    segment and continue (`rsync.c:360-380`).
//!
//! After the loop the attribute tail is decoded: `iflags` via `read_shortint`
//! for protocol >= 29 (`rsync.c:383-384`), the basis-type byte on
//! `ITEM_BASIS_TYPE_FOLLOWS` (`rsync.c:403-405`) and the xname vstring on
//! `ITEM_XNAME_FOLLOWS` (`rsync.c:407-417`). That tail lives in
//! [`SenderAttrs::read_attrs_after_ndx`].
//!
//! # Why a sink trait
//!
//! Steps 3, 5 and 6 mutate state that belongs to the *caller's* role - the
//! receiver's `file_list` / `ndx_segments` / `flist_eof`, or the generator's
//! deletion counters - while the classifier itself owns nothing. Inverting the
//! dependency ([`FlistMarkerSink`]) lets the free function keep the branch
//! order in exactly one place while each role supplies its own effects.
//!
//! # Deliberate omissions
//!
//! - **Nothing is written on the `NDX_FLIST_EOF` branch.** Upstream's
//!   `write_int(f_out, NDX_FLIST_EOF)` (`rsync.c:358`) never reaches a socket:
//!   the sender is rejected by the `rsync.c:343` gate before it can get there,
//!   and the receiver child runs with `sock_f_out = -1; f_out = error_pipe[1]`
//!   (`main.c:1063-1067`), so the marker goes down the error pipe to its own
//!   generator. oc runs the generator and the receiver in one process over one
//!   `file_list`, so there is no peer to forward to.
//! - **No `start_flist_forward()` / `stop_flist_forward()`** (`io.c:1483-1489`).
//!   That teeing exists only because upstream's generator and receiver are
//!   separate processes that each parse their own copy of the sub-list bytes.
//! - **The `dir_ndx` range check** (`rsync.c:361-369`) is not duplicated here;
//!   [`ReceiverContext::receive_one_extra_segment`] already performs it
//!   (mirroring `flist.c:2622-2626`) before appending anything.
//! - **The protocol-29 keep-alive re-entry** (`rsync.c:386-391`) is not
//!   implemented. It is a known incompleteness, tracked separately. Note that
//!   it is *not* dead code: rsync 3.0.9 emits the raw form at exactly protocol
//!   29 (`io.c:953-968` - `write_int(sock_f_out, cur_flist->used)` followed by
//!   `write_shortint(sock_f_out, ITEM_IS_NEW)`), and 3.4.4 replaced it with an
//!   unconditional empty `MSG_DATA` (`io.c:1445-1453`), so the cutoff is rsync
//!   3.1.0 and the reader branch exists solely for peers <= 3.0.x. oc
//!   negotiates down to protocol 28, so the frame is reachable. Because the
//!   branch re-enters the loop *after* `iflags` is decoded, [`read_ndx_and_attrs`]
//!   keeps the attribute read inside its loop and marks the insertion point.
//! - **The non-regular-file guard** (`rsync.c:420-428`) and the xname
//!   `sanitize_path()` pass (`rsync.c:407-417`) stay with their current owners.

use std::io::{self, Read};

use protocol::codec::{NDX_DEL_STATS, NDX_DONE, NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec};
use protocol::stats::DeleteStats;

use super::ReceiverContext;
use super::wire::SenderAttrs;

/// Which side of the connection is reading, mirroring upstream's `am_sender`
/// global and the `who_am_i()` tag in its diagnostics (`log.c:who_am_i()`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamRole {
    /// Reading the receiver's replies (upstream `am_sender = 1`).
    Sender,
    /// Reading the sender's echoes (upstream `am_sender = 0`).
    Receiver,
}

impl StreamRole {
    /// Upstream's `am_sender` term in the `rsync.c:343` gate.
    const fn am_sender(self) -> bool {
        matches!(self, Self::Sender)
    }

    /// The `[role=VERSION]` trailer standing in for upstream's `who_am_i()`.
    fn trailer(self) -> String {
        match self {
            Self::Sender => crate::role_trailer::sender(),
            Self::Receiver => crate::role_trailer::receiver(),
        }
    }
}

/// The state a marker-aware NDX read mutates, inverted out of the classifier.
///
/// Implementors supply the role-specific effects of upstream's `read_loop`
/// branches; [`read_ndx_step`] owns the order in which they are consulted.
pub(crate) trait FlistMarkerSink {
    /// Bookkeeping captured before the NDX read and handed back to the branch
    /// that consumes flist bytes.
    ///
    /// The receiver uses it to snapshot the raw read counter so a sub-list
    /// header or the EOF marker is billed to `flist_size`, exactly as upstream
    /// brackets `recv_file_list()` with `stats.total_read` (`flist.c:2615`,
    /// `flist.c:2789`).
    type FrameMark: Copy;

    /// Which side is reading (upstream `am_sender` / `who_am_i()`).
    fn role(&self) -> StreamRole;

    /// The highest valid file index, for the `(%d - %d)` span upstream prints
    /// on rejection (`rsync.c:344-350`).
    fn last_file_ndx(&self) -> i32;

    /// Whether `ndx` names a *live* entry - upstream's `F_IS_ACTIVE(file)`
    /// test.
    ///
    /// A peer that sends duplicate file-list entries gets one of them
    /// `clear_file()`d by `flist_sort_and_clean()`, which zeroes the basename
    /// and mode but leaves the slot indexable. Upstream refuses a transfer-phase
    /// index naming such a slot rather than dereferencing the cleared struct,
    /// because `f_name()` on it returns NULL.
    ///
    /// This is deliberately **not** defaulted: only a sink that owns a file list
    /// can answer, and a blanket `true` default would silently carry the guard
    /// past every future sink that forgets to override it. Sinks with no list of
    /// their own state that explicitly.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:871-881` - `recv_files()` refuses the inactive entry.
    /// - `sender.c:558-563` - `send_files()` refuses it identically.
    fn ndx_is_active(&self, ndx: i32) -> bool;

    /// Captures [`Self::FrameMark`] before the NDX is read.
    fn begin_frame(&mut self) -> Self::FrameMark;

    /// Consumes the deletion counters of an `NDX_DEL_STATS` frame.
    ///
    /// upstream: `rsync.c:338-341` - `read_del_stats(f_in)`, plus
    /// `write_del_stats(f_out)` when `am_sender && am_server`. oc's server
    /// sender emits its counters from its own goodbye sequence rather than from
    /// inside this loop, so the echo is the implementor's business.
    fn on_del_stats(&mut self, stats: &DeleteStats) -> io::Result<()>;

    /// Upstream `inc_recurse`: whether this reader may grow its file list from
    /// the stream at all (`rsync.c:343`).
    ///
    /// Defaults to `false`, which makes every sub-list marker a protocol
    /// violation - the correct answer for a call site outside the lazy
    /// file-list window.
    fn inc_recurse(&self) -> bool {
        false
    }

    /// Records that the sub-list stream has ended (`rsync.c:354`).
    ///
    /// Unreachable unless [`Self::inc_recurse`] is true and the role is the
    /// receiver, because the `rsync.c:343` gate runs first; the default is the
    /// no-lazy-file-list case, which tracks no such flag.
    fn set_flist_eof(&mut self, _mark: Self::FrameMark) {}

    /// Receives one INC_RECURSE sub-list segment framed by the raw marker
    /// `raw_ndx` (`rsync.c:375` - `recv_file_list(f_in, ndx)`).
    ///
    /// Unreachable unless [`Self::inc_recurse`] is true and the role is the
    /// receiver; the default restates the `rsync.c:343` rejection so no call
    /// site can silently grow a list it does not own.
    fn receive_segment<R: Read + ?Sized>(
        &mut self,
        _reader: &mut R,
        raw_ndx: i32,
        _mark: Self::FrameMark,
    ) -> io::Result<()> {
        Err(invalid_file_index(
            raw_ndx,
            self.last_file_ndx(),
            self.role(),
        ))
    }
}

/// A sink for call sites that must never grow a file list.
///
/// Upstream reaches `read_ndx_and_attrs()` from places where `inc_recurse` is
/// off or `am_sender` is set (`main.c:903` `read_final_goodbye()` on the sender
/// being the clearest); there every marker trips the `rsync.c:343` gate. This
/// is that peer expressed as a value.
///
/// Deletion counters are read off the wire (upstream drains them before the
/// gate) and discarded: oc's receiver runs its delete pass inline and reports
/// from `pending_del_stats`, so a peer-supplied echo carries no new information.
pub(crate) struct NoLazyFlist {
    role: StreamRole,
    last_file_ndx: i32,
}

impl NoLazyFlist {
    /// Builds the rejecting sink for `role`, whose highest valid file index is
    /// `last_file_ndx`.
    pub(crate) const fn new(role: StreamRole, last_file_ndx: i32) -> Self {
        Self {
            role,
            last_file_ndx,
        }
    }
}

impl FlistMarkerSink for NoLazyFlist {
    type FrameMark = ();

    fn role(&self) -> StreamRole {
        self.role
    }

    fn last_file_ndx(&self) -> i32 {
        self.last_file_ndx
    }

    fn ndx_is_active(&self, _ndx: i32) -> bool {
        // This sink carries a bound, not a list, so it cannot see a cleared
        // slot. Reporting every index active leaves the decision to the range
        // guard above, which is the only one it has the state to make.
        true
    }

    fn begin_frame(&mut self) {}

    fn on_del_stats(&mut self, _stats: &DeleteStats) -> io::Result<()> {
        Ok(())
    }
}

/// One iteration of upstream's `read_loop` (`rsync.c:329-381`).
///
/// Every variant names the branch that produced it so a reordering of
/// [`read_ndx_step`] is observable in tests, not just a behaviour change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NdxStep {
    /// `ndx >= 0`: a per-file index; the loop ends here (`rsync.c:332-333`).
    File(i32),
    /// `NDX_DONE`: phase complete, no attribute tail follows
    /// (`rsync.c:334-335`).
    Done,
    /// `NDX_DEL_STATS`: five deletion varints were drained (`rsync.c:336-342`).
    DelStats,
    /// `NDX_FLIST_EOF`: the sub-list stream ended (`rsync.c:353-359`).
    FlistEof,
    /// A sub-list segment for `dir_ndx` was received (`rsync.c:360-380`).
    Segment(i32),
}

/// Terminal outcome of upstream's `read_loop`: the only two ways out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NdxFrame {
    /// A per-file index whose attribute tail is still on the wire.
    File(i32),
    /// `NDX_DONE`, which upstream returns before reading any attributes.
    Done,
}

/// Builds upstream's `rsync.c:346-349` rejection.
///
/// Keeps upstream's wording - `Invalid file index: %d (%d - %d)` - and appends
/// oc's source location plus the `[role=VERSION]` trailer that stands in for
/// `who_am_i()`. Tagged as a protocol violation so the exit-code mapper yields
/// `RERR_PROTOCOL` (2), matching `exit_cleanup(RERR_PROTOCOL)` at
/// `rsync.c:351`.
pub(crate) fn invalid_file_index(ndx: i32, last: i32, role: StreamRole) -> io::Error {
    protocol::protocol_violation(format!(
        "Invalid file index: {ndx} ({NDX_DONE} - {last}) {}{}",
        crate::role_trailer::error_location!(),
        role.trailer()
    ))
}

/// Builds upstream's cleared-entry rejection.
///
/// Upstream prints the bare `rsync: refusing transfer of cleared file index %d`
/// at both sites; oc appends its source location and the `[role=VERSION]`
/// trailer that stands in for `who_am_i()`, as every other diagnostic in this
/// module does. Tagged as a protocol violation so the exit-code mapper yields
/// `RERR_PROTOCOL` (2), matching `exit_cleanup(RERR_PROTOCOL)` at both sites.
///
/// # Upstream Reference
///
/// - `receiver.c:871-881` - `recv_files()`.
/// - `sender.c:558-563` - `send_files()`.
pub(crate) fn cleared_file_index(ndx: i32, role: StreamRole) -> io::Error {
    protocol::protocol_violation(format!(
        "rsync: refusing transfer of cleared file index {ndx} {}{}",
        crate::role_trailer::error_location!(),
        role.trailer()
    ))
}

/// Reads and classifies exactly one NDX frame, running the marker side effects
/// through `sink`.
///
/// This is one iteration of upstream's `read_loop`; the branch order is the
/// contract. Callers that want upstream's *whole* loop use
/// [`read_marker_aware_ndx`]; callers that must react to each marker as it
/// lands (the lazy sub-list fetch, which may not over-read past the segment it
/// needed) drive this directly.
///
/// # Upstream Reference
///
/// - `rsync.c:329-381` - `read_loop`
pub(crate) fn read_ndx_step<R, C, S>(
    reader: &mut R,
    ndx_codec: &mut C,
    sink: &mut S,
) -> io::Result<NdxStep>
where
    R: Read + ?Sized,
    C: NdxCodec + ?Sized,
    S: FlistMarkerSink + ?Sized,
{
    let mark = sink.begin_frame();
    let ndx = ndx_codec.read_ndx(reader)?;

    // upstream: rsync.c:332-333 - a real file index leaves the loop.
    if ndx >= 0 {
        return Ok(NdxStep::File(ndx));
    }

    // upstream: rsync.c:334-335 - NDX_DONE returns before any attribute byte.
    if ndx == NDX_DONE {
        return Ok(NdxStep::Done);
    }

    // upstream: rsync.c:336-342 - drained ahead of the inc_recurse gate below,
    // so a peer without INC_RECURSE still consumes the counters instead of
    // desyncing on them.
    if ndx == NDX_DEL_STATS {
        let stats = DeleteStats::read_from(reader)?;
        sink.on_del_stats(&stats)?;
        return Ok(NdxStep::DelStats);
    }

    // upstream: rsync.c:343-352 - every remaining negative value is a file-list
    // marker, which only a receiver inside the INC_RECURSE window may consume.
    if !sink.inc_recurse() || sink.role().am_sender() {
        return Err(invalid_file_index(ndx, sink.last_file_ndx(), sink.role()));
    }

    // upstream: rsync.c:353-359 - flist_eof = 1. See the module docs for why
    // nothing is written back here.
    if ndx == NDX_FLIST_EOF {
        sink.set_flist_eof(mark);
        return Ok(NdxStep::FlistEof);
    }

    // upstream: rsync.c:360-380 - dir_ndx = NDX_FLIST_OFFSET - ndx, then
    // recv_file_list(f_in, ndx). The range check against dir_flist->used
    // (rsync.c:361-369) lives in the sink's segment receiver.
    sink.receive_segment(reader, ndx, mark)?;
    Ok(NdxStep::Segment(NDX_FLIST_OFFSET - ndx))
}

/// Runs upstream's `read_loop` for a call site that reads **no** attribute
/// tail, consuming markers until a per-file index or `NDX_DONE` surfaces.
///
/// oc's phase-boundary and goodbye reads accept only `NDX_DONE`; a per-file
/// index there is a protocol violation the caller reports without ever needing
/// its attributes. Because no `iflags` is decoded, the protocol-29 keep-alive
/// re-entry (`rsync.c:386-391`, predicated on `iflags == ITEM_IS_NEW`) cannot
/// apply to this entry point - that seam belongs to [`read_ndx_and_attrs`].
///
/// # Upstream Reference
///
/// - `rsync.c:329-381` - `read_loop`
pub(crate) fn read_marker_aware_ndx<R, C, S>(
    reader: &mut R,
    ndx_codec: &mut C,
    sink: &mut S,
) -> io::Result<NdxFrame>
where
    R: Read + ?Sized,
    C: NdxCodec + ?Sized,
    S: FlistMarkerSink + ?Sized,
{
    loop {
        match read_ndx_step(reader, ndx_codec, sink)? {
            NdxStep::File(ndx) => return Ok(NdxFrame::File(ndx)),
            NdxStep::Done => return Ok(NdxFrame::Done),
            NdxStep::DelStats | NdxStep::FlistEof | NdxStep::Segment(_) => {}
        }
    }
}

/// Upstream `read_ndx_and_attrs()` end to end: the marker loop plus the
/// attribute tail, which `NDX_DONE` deliberately skips.
///
/// `None` is `NDX_DONE`. Returning the attributes only alongside a per-file
/// index is what makes it impossible to consume the two phantom `iflags` bytes
/// that upstream never sends after `NDX_DONE` (`rsync.c:334-335`).
///
/// The attribute read sits **inside** the loop on purpose. Upstream's
/// `read_loop` has a second entry point *after* `iflags` is decoded: the
/// protocol-29 keep-alive branch (`rsync.c:386-391`) `goto`s back into it. That
/// branch is not implemented here (see the module docs); the `continue` seam it
/// needs is marked below so adding it is an insertion, not a restructuring.
///
/// # Upstream Reference
///
/// - `rsync.c:322-431` - `read_ndx_and_attrs()`
/// - `rsync.c:328` - the `read_loop:` label this `loop` stands for
pub(crate) fn read_ndx_and_attrs<R, C, S>(
    reader: &mut R,
    ndx_codec: &mut C,
    sink: &mut S,
    preserve_xattrs: bool,
    want_xattr_optim: bool,
) -> io::Result<Option<(i32, SenderAttrs)>>
where
    R: Read + ?Sized,
    C: NdxCodec + ?Sized,
    S: FlistMarkerSink + ?Sized,
{
    let protocol_version = ndx_codec.protocol_version();

    loop {
        // upstream: rsync.c:329-381 - the marker loop.
        let ndx = match read_ndx_step(reader, ndx_codec, sink)? {
            NdxStep::File(ndx) => ndx,
            NdxStep::Done => return Ok(None),
            NdxStep::DelStats | NdxStep::FlistEof | NdxStep::Segment(_) => continue,
        };

        // upstream: rsync.c:383-417 - iflags, then the optional basis-type byte
        // and xname vstring.
        let attrs = SenderAttrs::read_attrs_after_ndx(
            reader,
            protocol_version,
            preserve_xattrs,
            want_xattr_optim,
        )?;

        // SEAM - upstream: rsync.c:386-391
        //   if (protocol_version < 30 && ndx == cur_flist->used && iflags == ITEM_IS_NEW) {
        //       if (am_sender) maybe_send_keepalive(time(NULL), MSK_ALLOW_FLUSH);
        //       goto read_loop;
        //   }
        // A `continue` here re-enters the loop above. Deliberately absent; see
        // the module docs for why it is tracked separately.

        return Ok(Some((ndx, attrs)));
    }
}

/// The receiver's own state is the lazy file-list sink: it owns `file_list`,
/// `ndx_segments`, `flist_eof` and the sub-list decoder, so it delegates
/// straight to [`ReceiverContext::receive_one_extra_segment`] rather than
/// reimplementing segment reception.
impl FlistMarkerSink for ReceiverContext {
    /// The raw-read snapshot bracketing a flist span (`flist.c:2615`).
    type FrameMark = u64;

    fn role(&self) -> StreamRole {
        StreamRole::Receiver
    }

    fn last_file_ndx(&self) -> i32 {
        // upstream: rsync.c:345-348 - `first_flist->prev->ndx_start +
        // first_flist->prev->used - 1`, i.e. the last index of the newest
        // segment, or -1 when no list has been received yet.
        let &(flat_start, ndx_start) = self
            .ndx_segments
            .last()
            .expect("initial segment always exists");
        let used = self.file_list.len().saturating_sub(flat_start) as i32;
        ndx_start + used - 1
    }

    fn ndx_is_active(&self, ndx: i32) -> bool {
        // upstream: receiver.c:871-881 - `recv_files()` tests F_IS_ACTIVE on
        // the entry the peer's index resolves to. An NDX outside every segment
        // is a range fault, not a cleared entry, so it is deferred to the guard
        // that owns that diagnostic rather than reported as cleared here.
        self.wire_to_flat_ndx(ndx)
            .and_then(|flat| self.file_list.get(flat))
            .is_none_or(protocol::flist::FileEntry::is_active)
    }

    fn begin_frame(&mut self) -> u64 {
        // Snapshot before the ndx read: upstream's raw counter ticks on arrival
        // (io.c:820), so a segment header or the EOF marker is attributed to an
        // adjacent recv_file_list span on any real link. The span is kept only
        // when the frame turns out to be flist traffic; NDX_DONE and per-file
        // replies are transfer-phase frames upstream never counts.
        self.flist_span_start()
    }

    fn on_del_stats(&mut self, _stats: &DeleteStats) -> io::Result<()> {
        // upstream: rsync.c:338 read_del_stats() folds the peer's counters into
        // the global stats. oc's receiver performs its delete pass inline and
        // reports from `pending_del_stats`, so the echoed copy is redundant; it
        // is drained purely to keep the stream aligned.
        Ok(())
    }

    fn inc_recurse(&self) -> bool {
        self.compat_flags
            .is_some_and(|f| f.contains(protocol::CompatibilityFlags::INC_RECURSE))
    }

    fn set_flist_eof(&mut self, mark: u64) {
        logging::debug_log!(Flist, 2, "received NDX_FLIST_EOF, file list complete");
        self.flist_eof = true;
        self.flist_span_end(mark);
    }

    fn receive_segment<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        raw_ndx: i32,
        mark: u64,
    ) -> io::Result<()> {
        self.receive_one_extra_segment(reader, raw_ndx, mark)
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests;
