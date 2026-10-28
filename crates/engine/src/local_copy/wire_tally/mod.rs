//! Wire model behind a local copy's `sent` and `received` totals.
//!
//! Upstream runs every local copy as a forked client sender and a server
//! receiver joined by a socketpair, and the totals it prints are the client's
//! raw socket counters (`io.c:938` `stats.total_read`, `io.c:977`
//! `stats.total_written`, cached by `main.c:334-335 handle_stats()`). oc-rsync
//! copies in one process, so this module replays the stream the two processes
//! would have exchanged through the protocol crate's own encoders, writing into
//! byte counters instead of a socket.
//!
//! # Counted window
//!
//! The handshake runs before multiplexing starts and is not counted. After it,
//! the client writes the file list, an echo of every itemized entry, each
//! transfer (NDX, iflags, sum head, tokens, whole-file sum) and its NDX_DONEs,
//! all in `MSG_DATA` frames with a 4-byte header each. `client_run()`
//! (`main.c:1398-1403`) caches the counters before `read_final_goodbye()`, so
//! the goodbye echo (`main.c:932-935`) is excluded. The client reads the
//! generator's items, sum requests and NDX_DONEs; the server's
//! `MSG_BLOCK_STATS` and its last goodbye NDX_DONE arrive after the cache and
//! are excluded too.
//!
//! # Frame boundaries
//!
//! Payload bytes are a pure function of the transfer. Frame headers are not:
//! upstream closes a frame whenever a process blocks for input or fills its
//! 64 KiB buffer (`io.c:perform_io()`), so the count depends on scheduling.
//! [`frames`] models the schedule a quiet machine produces: one frame per
//! protocol round and per buffer fill. A single-directory tree below one
//! buffer reproduces 3.5.1 exactly; deeper or larger trees can differ by one
//! 4-byte header per frame upstream happens to merge or split.

mod frames;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use protocol::codec::{NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, NdxCodecEnum};
use protocol::flist::{FileEntry, FileListWriter, SubListPlan, compare_file_entries};
use protocol::idlist::IdList;
use protocol::wire::{SumHead, write_token_block_match, write_token_end, write_token_literal};
use protocol::{CompatibilityFlags, ProtocolVersion};

use crate::local_copy::LocalCopyOptions;

use frames::FrameSchedule;

/// Session parameters the local wire model counts against.
///
/// A local copy negotiates a protocol version and compatibility flags between
/// its two halves exactly as a remote one does (`compat.c:setup_protocol()`),
/// and the totals depend on both: the flags pick the file-list layout and
/// encoding, the version picks the NDX and integer encodings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WireTotals {
    protocol: ProtocolVersion,
    compat_flags: CompatibilityFlags,
    itemize_unchanged: bool,
}

impl WireTotals {
    /// Creates the model parameters for a session at `protocol` with the
    /// negotiated `compat_flags`.
    #[must_use]
    pub const fn new(protocol: ProtocolVersion, compat_flags: CompatibilityFlags) -> Self {
        Self {
            protocol,
            compat_flags,
            itemize_unchanged: false,
        }
    }

    /// Records whether the generator itemizes entries that did not change.
    ///
    /// upstream: generator.c:587-588 `itemize()` sends an NDX and iflags for
    /// an unchanged entry when `INFO_GTE(NAME, 2)` (`-vv`) or
    /// `stdout_format_has_i > 1` (`-ii`).
    #[must_use]
    pub const fn with_itemize_unchanged(mut self, itemize_unchanged: bool) -> Self {
        self.itemize_unchanged = itemize_unchanged;
        self
    }

    /// The negotiated protocol version.
    #[must_use]
    pub const fn protocol(&self) -> ProtocolVersion {
        self.protocol
    }

    /// The negotiated compatibility flags.
    #[must_use]
    pub const fn compat_flags(&self) -> CompatibilityFlags {
        self.compat_flags
    }

    /// Whether the generator itemizes unchanged entries.
    #[must_use]
    pub const fn itemize_unchanged(&self) -> bool {
        self.itemize_unchanged
    }

    fn inc_recurse(&self) -> bool {
        self.compat_flags.contains(CompatibilityFlags::INC_RECURSE)
    }
}

/// Counts bytes written through it and discards them.
#[derive(Debug, Default)]
struct ByteCounter(u64);

impl io::Write for ByteCounter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// One NDX-addressed exchange between the generator and the sender.
#[derive(Clone, Copy, Debug)]
enum Exchange {
    /// `itemize()` reported the entry without moving data: NDX and iflags in
    /// both directions (generator.c:588-592, sender.c:584-586).
    Item,
    /// A data transfer: the generator's request carries the sum head and the
    /// block sums (generator.c:793 `write_sum_head()` plus 4 + `s2length`
    /// bytes per block), the sender's reply echoes the head and appends the
    /// token stream and whole-file sum (sender.c:768-781).
    Transfer { head: SumHead, tokens: u64 },
}

/// A transfer whose token stream is still being counted.
#[derive(Debug)]
struct PendingTransfer {
    entry: u32,
    head: SumHead,
    tokens: ByteCounter,
    literal_run: u64,
}

impl PendingTransfer {
    /// Writes the pending literal run the way `simple_send_token()` does:
    /// chunks of at most `CHUNK_SIZE`, each behind a 4-byte length.
    ///
    /// upstream: token.c:319-327 - only the length of the run matters, so the
    /// encoder is fed zeroes.
    fn flush_literal_run(&mut self) {
        static ZEROES: [u8; protocol::wire::CHUNK_SIZE] = [0; protocol::wire::CHUNK_SIZE];
        while self.literal_run > 0 {
            let len = self.literal_run.min(ZEROES.len() as u64) as usize;
            write_token_literal(&mut self.tokens, &ZEROES[..len]).expect("counting never fails");
            self.literal_run -= len as u64;
        }
    }
}

/// Records what a local copy would have put on upstream's socketpair and
/// computes the client's `sent` and `received` totals from it.
///
/// Built only when the run prints its totals, so a plain copy pays nothing.
#[derive(Debug)]
pub(crate) struct WireTally {
    totals: WireTotals,
    /// File-list entries in traversal order.
    entries: Vec<FileEntry>,
    /// Each entry's position in its directory's `readdir()` order.
    readdir: Vec<u32>,
    by_path: HashMap<PathBuf, u32>,
    exchanges: Vec<(u32, Exchange)>,
    pending: Option<PendingTransfer>,
    /// Width of the whole-file sum after the tokens (`xfer_sum_len`).
    xfer_sum_len: u64,
    /// Width of the per-file sum in a `--checksum` file list.
    flist_sum_len: Option<usize>,
    preserve_uid: bool,
    preserve_gid: bool,
    preserve_links: bool,
    preserve_devices: bool,
    preserve_specials: bool,
    preserve_atimes: bool,
    preserve_crtimes: bool,
    numeric_ids: bool,
    /// `--dry-run`: the sender only echoes the request (sender.c:639-642).
    dry_run: bool,
}

impl WireTally {
    /// Creates a tally for a run with `options`.
    pub(crate) fn new(totals: WireTotals, options: &LocalCopyOptions, dry_run: bool) -> Self {
        Self {
            totals,
            entries: Vec::new(),
            readdir: Vec::new(),
            by_path: HashMap::new(),
            exchanges: Vec::new(),
            pending: None,
            xfer_sum_len: options.transfer_checksum_algorithm().digest_len() as u64,
            flist_sum_len: options
                .checksum_enabled()
                .then(|| options.checksum_algorithm().digest_len()),
            preserve_uid: options.preserve_owner(),
            preserve_gid: options.preserve_group(),
            preserve_links: options.links_enabled(),
            preserve_devices: options.devices_enabled(),
            preserve_specials: options.specials_enabled(),
            preserve_atimes: options.preserve_atimes(),
            preserve_crtimes: options.preserve_crtimes(),
            numeric_ids: options.numeric_ids_enabled(),
            dry_run,
        }
    }

    /// Whether unchanged entries are itemized on the wire.
    pub(crate) const fn itemize_unchanged(&self) -> bool {
        self.totals.itemize_unchanged()
    }

    /// Adds a file-list entry the sender would send, with its position in its
    /// directory's `readdir()` order.
    pub(crate) fn add_entry(&mut self, entry: FileEntry, readdir_index: u32) {
        let index = self.entries.len() as u32;
        self.by_path.insert(entry.path().to_path_buf(), index);
        self.entries.push(entry);
        self.readdir.push(readdir_index);
    }

    /// Records an itemize-only exchange for the entry at `relative`.
    pub(crate) fn item(&mut self, relative: &Path) {
        if let Some(&index) = self.by_path.get(relative) {
            self.exchanges.push((index, Exchange::Item));
        }
    }

    /// Starts counting a transfer of the entry added last.
    ///
    /// A transfer abandoned before [`Self::end_transfer`] (for example a
    /// source that changed mid-copy and is redone) leaves no exchange behind.
    pub(crate) fn begin_transfer(&mut self) {
        let Some(entry) = self.entries.len().checked_sub(1) else {
            return;
        };
        self.pending = Some(PendingTransfer {
            entry: entry as u32,
            head: SumHead::WHOLE_FILE,
            tokens: ByteCounter::default(),
            literal_run: 0,
        });
    }

    /// Records the sum head the transfer's tokens are built against.
    pub(crate) fn sum_head(&mut self, head: SumHead) {
        if let Some(pending) = self.pending.as_mut() {
            pending.head = head;
        }
    }

    /// Adds `len` literal bytes to the transfer's current literal run.
    pub(crate) fn literal(&mut self, len: u64) {
        if let Some(pending) = self.pending.as_mut() {
            pending.literal_run += len;
        }
    }

    /// Adds a block-match token, closing the literal run before it.
    pub(crate) fn block_match(&mut self, block_index: u32) {
        if let Some(pending) = self.pending.as_mut() {
            pending.flush_literal_run();
            write_token_block_match(&mut pending.tokens, block_index)
                .expect("counting never fails");
        }
    }

    /// Ends the transfer with its end token.
    pub(crate) fn end_transfer(&mut self) {
        if let Some(mut pending) = self.pending.take() {
            pending.flush_literal_run();
            write_token_end(&mut pending.tokens).expect("counting never fails");
            self.exchanges.push((
                pending.entry,
                Exchange::Transfer {
                    head: pending.head,
                    tokens: pending.tokens.0,
                },
            ));
        }
    }

    /// Computes the client's `(sent, received)` totals.
    pub(crate) fn finish(mut self) -> (u64, u64) {
        let mut order: Vec<u32> = (0..self.entries.len() as u32).collect();
        order.sort_by(|&a, &b| {
            compare_file_entries(&self.entries[a as usize], &self.entries[b as usize])
        });
        let mut slots: Vec<Option<FileEntry>> =
            std::mem::take(&mut self.entries).into_iter().map(Some).collect();
        let sorted: Vec<FileEntry> = order
            .iter()
            .map(|&t| slots[t as usize].take().expect("each entry is sorted once"))
            .collect();

        let mut schedule = FrameSchedule::default();
        let mut sender = NdxCodecEnum::new(self.totals.protocol().as_u8());
        let mut generator = NdxCodecEnum::new(self.totals.protocol().as_u8());

        let layout = if self.totals.inc_recurse() {
            self.incremental_layout(&sorted, &order, &mut sender, &mut schedule)
        } else {
            self.flat_layout(&sorted, &order, &mut schedule)
        };

        let mut exchanges = std::mem::take(&mut self.exchanges);
        exchanges.sort_by_key(|&(entry, _)| layout.ndx[entry as usize]);
        let mut rounds = [(ByteCounter::default(), ByteCounter::default()); 2];
        for (entry, exchange) in exchanges {
            let ndx = layout.ndx[entry as usize];
            let (to_sender, to_generator) = &mut rounds[layout.round_of(ndx)];
            self.count_exchange(&mut generator, to_sender, ndx, exchange, true);
            self.count_exchange(&mut sender, to_generator, ndx, exchange, false);
        }
        let [(received_1, sent_1), (received_2, sent_2)] = rounds;
        let mut done = ByteCounter::default();
        sender.write_ndx_done(&mut done).expect("counting never fails");
        schedule.exchange_rounds(
            [sent_1.0, sent_2.0],
            [received_1.0, received_2.0],
            layout.list_count,
            done.0,
            self.totals.inc_recurse(),
        );
        schedule.totals()
    }

    /// Counts one exchange in one direction: the generator's request (to the
    /// sender) when `request`, otherwise the sender's reply.
    fn count_exchange(
        &self,
        codec: &mut NdxCodecEnum,
        out: &mut ByteCounter,
        ndx: i32,
        exchange: Exchange,
        request: bool,
    ) {
        codec.write_ndx(out, ndx).expect("counting never fails");
        // upstream: rsync.c:384 / sender.c:476 - write_shortint(iflags).
        if self.totals.protocol().as_u8() >= 29 {
            out.0 += 2;
        }
        let Exchange::Transfer { head, tokens } = exchange else {
            return;
        };
        if self.dry_run {
            return;
        }
        head.write(out).expect("counting never fails");
        if request {
            // upstream: generator.c:793-806 - each block's rolling sum (4)
            // and its `s2length`-byte strong sum.
            out.0 += u64::from(head.count()) * (4 + u64::from(head.s2length()));
        } else {
            out.0 += tokens + self.xfer_sum_len;
        }
    }

    fn flist_writer(&self) -> FileListWriter {
        let writer = FileListWriter::with_compat_flags(self.totals.protocol(), self.totals.compat_flags())
            .with_preserve_uid(self.preserve_uid)
            .with_preserve_gid(self.preserve_gid)
            .with_preserve_links(self.preserve_links)
            .with_preserve_devices(self.preserve_devices)
            .with_preserve_specials(self.preserve_specials)
            .with_preserve_atimes(self.preserve_atimes)
            .with_preserve_crtimes(self.preserve_crtimes)
            // upstream: flist.c:789-790,799-800 - inline owner names ride in
            // the entries only under incremental recursion.
            .with_name_follows(self.totals.inc_recurse() && !self.numeric_ids);
        match self.flist_sum_len {
            Some(len) => writer.with_always_checksum(len),
            None => writer,
        }
    }

    /// Orders a list's members as `send_directory()` sent them: `.` first,
    /// then `readdir()` order, then traversal order for operands.
    fn wire_order(&self, members: &[usize], sorted: &[FileEntry], order: &[u32]) -> Vec<usize> {
        let mut wire = members.to_vec();
        wire.sort_by_key(|&s| {
            let traversal = order[s] as usize;
            (
                sorted[s].path() != Path::new("."),
                self.readdir[traversal],
                traversal,
            )
        });
        wire
    }

    /// Encodes the file list as incremental lists and plans which lists
    /// travel in which protocol round.
    ///
    /// upstream: flist.c:3082-3101 - the initial list goes out with
    /// `NDX_FLIST_EOF` when it has no directories to expand; a one-entry
    /// initial list is followed by sub-lists until one adds an entry
    /// (`send_extra_file_list(f, 1)`); `main.c:1398` flushes; the rest go out
    /// in `send_files()`' first `send_extra_file_list()` (sender.c:516).
    fn incremental_layout(
        &self,
        sorted: &[FileEntry],
        order: &[u32],
        codec: &mut NdxCodecEnum,
        schedule: &mut FrameSchedule,
    ) -> Layout {
        let plan = SubListPlan::new(sorted);
        let starts = plan.ndx_starts();
        let mut writer = self.flist_writer();
        let mut rounds = [ByteCounter::default(), ByteCounter::default()];

        writer.set_first_ndx(starts[0]);
        for s in self.wire_order(&plan.initial, sorted, order) {
            writer
                .write_entry(&mut rounds[0], &sorted[s])
                .expect("counting never fails");
        }
        writer.write_end(&mut rounds[0], None).expect("counting never fails");

        let mut first_round_lists = 1;
        if plan.initial.len() == 1 {
            for sublist in &plan.sublists {
                first_round_lists += 1;
                if !sublist.members.is_empty() {
                    break;
                }
            }
        }
        for (k, (sublist, &start)) in plan.sublists.iter().zip(&starts[1..]).enumerate() {
            let round = &mut rounds[usize::from(k + 1 >= first_round_lists)];
            codec
                .write_ndx(round, NDX_FLIST_OFFSET - sublist.parent_dir_ndx)
                .expect("counting never fails");
            writer.set_first_ndx(start);
            for s in self.wire_order(&sublist.members, sorted, order) {
                writer
                    .write_entry(round, &sorted[s])
                    .expect("counting never fails");
            }
            writer.write_end(round, None).expect("counting never fails");
        }
        let last = &mut rounds[usize::from(plan.sublists.len() >= first_round_lists)];
        codec
            .write_ndx(last, NDX_FLIST_EOF)
            .expect("counting never fails");
        schedule.sender_flist([rounds[0].0, rounds[1].0]);

        let wire_ndx = plan.wire_ndx(sorted);
        let mut ndx = vec![0i32; sorted.len()];
        for (s, &n) in wire_ndx.iter().enumerate() {
            ndx[order[s] as usize] = n;
        }
        let second_round_ndx = starts
            .get(first_round_lists)
            .map_or(i32::MAX, |&start| start - 1);
        Layout {
            ndx,
            list_count: plan.list_count(),
            second_round_ndx,
        }
    }

    /// Encodes the file list as one flat list followed by the id lists.
    ///
    /// upstream: flist.c:send_file_list() then flist.c:3063 `send_id_lists()`
    /// when `numeric_ids <= 0 && !inc_recurse`.
    fn flat_layout(&self, sorted: &[FileEntry], order: &[u32], schedule: &mut FrameSchedule) -> Layout {
        let mut writer = self.flist_writer();
        let mut out = ByteCounter::default();
        for s in self.depth_first_order(sorted, order) {
            writer
                .write_entry(&mut out, &sorted[s])
                .expect("counting never fails");
        }
        writer.write_end(&mut out, None).expect("counting never fails");
        if !self.numeric_ids {
            self.write_id_lists(&mut out, sorted);
        }
        schedule.sender_flist([out.0, 0]);

        let mut ndx = vec![0i32; sorted.len()];
        for (s, &t) in order.iter().enumerate() {
            ndx[t as usize] = s as i32;
        }
        Layout {
            ndx,
            list_count: 1,
            second_round_ndx: i32::MAX,
        }
    }

    /// Orders a flat list as non-incremental `send_file_list()` sends it: a
    /// directory's entries in `readdir()` order, then each of those
    /// directories in turn, depth first.
    ///
    /// upstream: flist.c:send_directory() sends every name it reads, then
    /// calls `send_if_directory()` on each entry it just sent.
    fn depth_first_order(&self, sorted: &[FileEntry], order: &[u32]) -> Vec<usize> {
        let mut children: HashMap<&Path, Vec<usize>> = HashMap::new();
        let mut top = Vec::new();
        for (s, entry) in sorted.iter().enumerate() {
            let parent: &Path = entry.dirname();
            if parent.as_os_str().is_empty() || parent == Path::new(".") {
                top.push(s);
            } else {
                children.entry(parent).or_default().push(s);
            }
        }
        let mut out = Vec::with_capacity(sorted.len());
        let top = self.wire_order(&top, sorted, order);
        out.extend_from_slice(&top);
        let mut pending = vec![top.into_iter()];
        while let Some(list) = pending.last_mut() {
            let Some(s) = list.next() else {
                pending.pop();
                continue;
            };
            if !sorted[s].is_dir() || sorted[s].path() == Path::new(".") {
                continue;
            }
            if let Some(members) = children.remove(sorted[s].path().as_path()) {
                let members = self.wire_order(&members, sorted, order);
                out.extend_from_slice(&members);
                pending.push(members.into_iter());
            }
        }
        out
    }

    /// upstream: uidlist.c:410-424 `send_id_lists()` - one list per preserved
    /// owner kind, each `(id, name)` pair in first-sighting order.
    fn write_id_lists(&self, out: &mut ByteCounter, sorted: &[FileEntry]) {
        let id0_names = self
            .totals
            .compat_flags()
            .contains(CompatibilityFlags::ID0_NAMES);
        let protocol = self.totals.protocol().as_u8();
        let lists: [(bool, fn(&FileEntry) -> (Option<u32>, Option<&str>)); 2] = [
            (self.preserve_uid, |e| (e.uid(), e.user_name())),
            (self.preserve_gid, |e| (e.gid(), e.group_name())),
        ];
        for (preserve, id_of) in lists {
            if !preserve {
                continue;
            }
            let mut list = IdList::new();
            for entry in sorted {
                if let (Some(id), name) = id_of(entry) {
                    list.add_id(id, name.map(|n| n.as_bytes().to_vec()));
                }
            }
            list.write(out, id0_names, protocol)
                .expect("counting never fails");
        }
    }
}

/// Where each entry's exchanges travel.
#[derive(Debug)]
struct Layout {
    /// Wire NDX per entry, indexed in traversal order.
    ndx: Vec<i32>,
    list_count: usize,
    /// Lowest NDX belonging to a list sent in the second round.
    second_round_ndx: i32,
}

impl Layout {
    fn round_of(&self, ndx: i32) -> usize {
        usize::from(ndx >= self.second_round_ndx)
    }
}
