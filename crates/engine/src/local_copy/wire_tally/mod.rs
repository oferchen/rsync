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
use std::ffi::{OsStr, OsString};
use std::path::Path;

use protocol::codec::{NDX_DEL_STATS, NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, NdxCodecEnum};
use metadata::id_lookup::{lookup_group_name_cached, lookup_user_name_cached};
use protocol::flist::{FileEntry, FileListWriter, PathInterner, SubListPlan, compare_file_entries};
use protocol::idlist::IdList;
use protocol::wire::{
    CompressedTokenEncoder, SumHead, write_token_block_match, write_token_end, write_token_literal,
};
use protocol::{CompatibilityFlags, ProtocolVersion, write_varint};

use crate::local_copy::{LocalCopyOptions, wire_order_key};

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
    reports_created_directory: bool,
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
            reports_created_directory: false,
        }
    }

    /// Records whether the receiver announces the destination root it
    /// creates.
    ///
    /// upstream: main.c:828-829 `get_local_name()` prints `created directory
    /// %s` under `INFO_GTE(NAME, 1) || stdout_format_has_i`; the server half
    /// sends it to the client as one `MSG_INFO` frame.
    #[must_use]
    pub const fn with_created_directory_notice(mut self, reports: bool) -> Self {
        self.reports_created_directory = reports;
        self
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
#[derive(Clone, Copy, Debug, Default)]
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

/// The `-z` token encoder the sender would run, built from the session's
/// compression choice.
///
/// upstream: token.c:send_token() dispatches to `send_deflated_token()`,
/// `send_zstd_token()` or `send_compressed_token()` (lz4) by the negotiated
/// `do_compression`.
fn token_encoder(options: &LocalCopyOptions, protocol: ProtocolVersion) -> Option<CompressedTokenEncoder> {
    use compress::algorithm::CompressionAlgorithm;

    if !options.compress_enabled() {
        return None;
    }
    match options.compression_algorithm() {
        CompressionAlgorithm::Zlib => Some(CompressedTokenEncoder::new(
            options.compression_level(),
            u32::from(protocol.as_u8()),
        )),
        #[cfg(feature = "zstd")]
        CompressionAlgorithm::Zstd => {
            CompressedTokenEncoder::new_zstd(zstd_level(options.compression_level()), None).ok()
        }
        #[cfg(feature = "lz4")]
        CompressionAlgorithm::Lz4 => Some(CompressedTokenEncoder::new_lz4()),
    }
}

/// The zstd level a `CompressionLevel` stands for.
#[cfg(feature = "zstd")]
fn zstd_level(level: compress::zlib::CompressionLevel) -> i32 {
    use compress::zlib::CompressionLevel;

    match level {
        CompressionLevel::None | CompressionLevel::Default => 0,
        CompressionLevel::Fast => 1,
        CompressionLevel::Best => 19,
        CompressionLevel::Precise(v) => i32::from(v.get()),
        CompressionLevel::PreciseSigned(v) => v,
    }
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
pub(crate) struct WireTally {
    totals: WireTotals,
    /// File-list entries in traversal order.
    entries: Vec<FileEntry>,
    /// Each entry's position in its directory's `readdir()` order.
    readdir: Vec<u32>,
    /// Relative paths of the entries itemized without a transfer, resolved
    /// to entries once the walk is complete.
    items: Vec<OsString>,
    /// One shared parent path per directory, so entries do not each own a
    /// copy of it.
    dirnames: PathInterner,
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
    /// `MSG_DELETED` frames the generator sends the client.
    deleted: u64,
    /// Whether the generator sends delete statistics (`--delete*`).
    delete_mode: bool,
    /// Whether the statistics wait for a late delete pass.
    late_delete: bool,
    /// `-z`: the compressed token encoder literal data is fed through.
    encoder: Option<CompressedTokenEncoder>,
}

/// Per-type deletion counts, in `write_del_stats()` order: regular files,
/// directories, symlinks, devices, specials.
pub(crate) type DeleteCounts = [u64; 5];

impl WireTally {
    /// Creates a tally for a run with `options`.
    pub(crate) fn new(totals: WireTotals, options: &LocalCopyOptions, dry_run: bool) -> Self {
        Self {
            totals,
            entries: Vec::new(),
            readdir: Vec::new(),
            items: Vec::new(),
            dirnames: PathInterner::new(),
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
            deleted: 0,
            delete_mode: options.delete_extraneous(),
            // upstream: generator.c:127 EARLY_DELETE_DONE_MSG() is false for
            // `--delete-after` and `--delete-delay` (delete_during == 2).
            late_delete: options.delete_after_enabled() || options.delete_delay_enabled(),
            encoder: token_encoder(options, totals.protocol()),
        }
    }

    /// Whether unchanged entries are itemized on the wire.
    pub(crate) const fn itemize_unchanged(&self) -> bool {
        self.totals.itemize_unchanged()
    }

    /// Adds a file-list entry the sender would send, with its position in its
    /// directory's `readdir()` order.
    ///
    /// The entry needs no owner names: the tally attaches them while encoding,
    /// only where the writer can emit them.
    pub(crate) fn add_entry(&mut self, mut entry: FileEntry, readdir_index: u32) {
        let dirname = self.dirnames.intern(entry.dirname());
        entry.set_dirname(dirname);
        self.entries.push(entry);
        self.readdir.push(readdir_index);
    }

    /// Records an itemize-only exchange for the entry at `relative`.
    pub(crate) fn item(&mut self, relative: &Path) {
        self.items.push(relative.as_os_str().to_os_string());
    }

    /// Turns the itemized paths into exchanges on their entries, indexing
    /// the few itemized paths rather than every entry.
    fn resolve_items(&mut self) {
        if self.items.is_empty() {
            return;
        }
        let mut itemized: HashMap<&OsStr, usize> = HashMap::with_capacity(self.items.len());
        for path in &self.items {
            *itemized.entry(path.as_os_str()).or_default() += 1;
        }
        for (entry, index) in self.entries.iter().zip(0u32..) {
            if let Some(&times) = itemized.get(entry.path().as_os_str()) {
                self.exchanges
                    .extend(std::iter::repeat_n((index, Exchange::Item), times));
            }
        }
    }

    /// Records a deletion the generator reports to the client.
    ///
    /// upstream: log.c:913-916 `log_delete()` - a server half sends every
    /// deleted name as `MSG_DELETED`, a directory with its trailing NUL.
    pub(crate) fn deleted(&mut self, relative: &Path, is_dir: bool) {
        let name = relative.as_os_str().as_encoded_bytes().len() as u64;
        self.deleted += protocol::MESSAGE_HEADER_LEN as u64 + name + u64::from(is_dir);
    }

    /// Starts counting a transfer of the entry added last.
    ///
    /// A transfer abandoned before [`Self::end_transfer`] (for example a
    /// source that changed mid-copy and is redone) leaves no exchange behind.
    pub(crate) fn begin_transfer(&mut self) {
        let Some(entry) = self.entries.len().checked_sub(1) else {
            return;
        };
        if let Some(encoder) = self.encoder.as_mut() {
            encoder.reset();
        }
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

    /// Whether literal data must reach [`Self::literal_data`]: under `-z` the
    /// token sizes depend on the bytes, not just their count.
    pub(crate) const fn needs_literal_data(&self) -> bool {
        self.encoder.is_some()
    }

    /// Adds `len` literal bytes to the transfer's current literal run.
    ///
    /// Ignored under `-z`, where the bytes arrive through
    /// [`Self::literal_data`].
    pub(crate) fn literal(&mut self, len: u64) {
        if self.encoder.is_some() {
            return;
        }
        if let Some(pending) = self.pending.as_mut() {
            pending.literal_run += len;
        }
    }

    /// Adds literal bytes, compressing them under `-z`.
    pub(crate) fn literal_data(&mut self, data: &[u8]) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        match self.encoder.as_mut() {
            Some(encoder) => encoder
                .send_literal(&mut pending.tokens, data)
                .expect("counting never fails"),
            None => pending.literal_run += data.len() as u64,
        }
    }

    /// Adds a block-match token, closing the literal run before it.
    ///
    /// `matched` is the block's content when the caller has it; under zlib
    /// `-z` it primes the deflate history the way upstream's
    /// `see_deflate_token()` does (token.c:475-510).
    pub(crate) fn block_match(&mut self, block_index: u32, matched: Option<&[u8]>) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        match self.encoder.as_mut() {
            Some(encoder) => {
                encoder
                    .send_block_match(&mut pending.tokens, block_index)
                    .expect("counting never fails");
                if let Some(data) = matched {
                    encoder.see_token(data).expect("counting never fails");
                }
            }
            None => {
                pending.flush_literal_run();
                write_token_block_match(&mut pending.tokens, block_index)
                    .expect("counting never fails");
            }
        }
    }

    /// Ends the transfer with its end token.
    pub(crate) fn end_transfer(&mut self) {
        if let Some(mut pending) = self.pending.take() {
            match self.encoder.as_mut() {
                Some(encoder) => encoder
                    .finish(&mut pending.tokens)
                    .expect("counting never fails"),
                None => {
                    pending.flush_literal_run();
                    write_token_end(&mut pending.tokens).expect("counting never fails");
                }
            }
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
    ///
    /// `created_root` is the destination root when the run created it.
    pub(crate) fn finish(
        mut self,
        created_root: Option<&Path>,
        delete_counts: DeleteCounts,
    ) -> (u64, u64) {
        self.resolve_items();
        let mut order: Vec<u32> = (0..self.entries.len() as u32).collect();
        order.sort_by(|&a, &b| {
            compare_file_entries(&self.entries[a as usize], &self.entries[b as usize])
        });
        let mut sorted = std::mem::take(&mut self.entries);
        permute_in_place(&mut sorted, &order);
        let sorted = sorted.as_mut_slice();

        let mut schedule = FrameSchedule::default();
        let mut sender = NdxCodecEnum::new(self.totals.protocol().as_u8());
        let mut generator = NdxCodecEnum::new(self.totals.protocol().as_u8());

        let layout = if self.totals.inc_recurse() {
            self.incremental_layout(sorted, &order, &mut sender, &mut schedule)
        } else {
            self.flat_layout(sorted, &order, &mut schedule)
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
        let del_stats = if self.delete_mode {
            self.del_stats(&mut generator, delete_counts)
        } else {
            0
        };
        schedule.exchange_rounds(
            [sent_1.0, sent_2.0],
            [received_1.0, received_2.0],
            layout.list_count,
            done.0,
            self.totals.inc_recurse(),
        );
        schedule.generator_deletions(del_stats, self.deleted, self.late_delete, done.0);
        let (sent, mut received) = schedule.totals();
        if let Some(root) = created_root.filter(|_| self.totals.reports_created_directory) {
            received += created_directory_notice_len(root);
        }
        (sent, received)
    }

    /// upstream: main.c:229-242 `write_del_stats()` - `NDX_DEL_STATS` and one
    /// varint per deleted-entry type.
    fn del_stats(&self, codec: &mut NdxCodecEnum, counts: DeleteCounts) -> u64 {
        let mut out = ByteCounter::default();
        codec
            .write_ndx(&mut out, NDX_DEL_STATS)
            .expect("counting never fails");
        for count in counts {
            write_varint(&mut out, i32::try_from(count).unwrap_or(i32::MAX))
                .expect("counting never fails");
        }
        out.0
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
        let mut keyed: Vec<_> = members
            .iter()
            .map(|&s| {
                let traversal = order[s] as usize;
                (wire_order_key(is_dot(&sorted[s]), self.readdir[traversal], traversal), s)
            })
            .collect();
        keyed.sort_unstable();
        keyed.into_iter().map(|(_, s)| s).collect()
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
        sorted: &mut [FileEntry],
        order: &[u32],
        codec: &mut NdxCodecEnum,
        schedule: &mut FrameSchedule,
    ) -> Layout {
        let plan = SubListPlan::new(sorted);
        let starts = plan.ndx_starts();
        let mut writer = self.flist_writer();
        let mut rounds = [ByteCounter::default(), ByteCounter::default()];

        writer.set_first_ndx(starts[0]);
        let initial = self.wire_order(&plan.initial, sorted, order);
        self.write_list(&mut writer, &mut rounds[0], sorted, &initial);

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
            let members = self.wire_order(&sublist.members, sorted, order);
            self.write_list(&mut writer, round, sorted, &members);
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

    /// Writes one incremental list's members in wire order, then its end
    /// marker.
    ///
    /// upstream: flist.c:789-800 - an inline owner name follows only an entry
    /// whose uid or gid differs from the previous entry's (no `XMIT_SAME_UID`
    /// or `XMIT_SAME_GID`), so names are looked up for the list's first entry
    /// and wherever an id changes; the writer still decides each flag.
    fn write_list(
        &self,
        writer: &mut FileListWriter,
        out: &mut ByteCounter,
        sorted: &mut [FileEntry],
        members: &[usize],
    ) {
        let names = !self.numeric_ids;
        let mut previous = None;
        for &s in members {
            let entry = &mut sorted[s];
            let ids = (entry.uid(), entry.gid());
            if names && previous != Some(ids) {
                self.attach_owner_names(entry);
            }
            previous = Some(ids);
            writer.write_entry(out, entry).expect("counting never fails");
        }
        writer.write_end(out, None).expect("counting never fails");
    }

    /// Sets the user and group names the sender would look up for `entry`.
    ///
    /// upstream: uidlist.c:add_uid()/add_gid() - names come from the local
    /// account database; ids without a name send none.
    fn attach_owner_names(&self, entry: &mut FileEntry) {
        let name = |found: metadata::id_lookup::LookupResult<Vec<u8>>| {
            found.ok().flatten().and_then(|bytes| String::from_utf8(bytes).ok())
        };
        if self.preserve_uid
            && entry.user_name().is_none()
            && let Some(user) = entry.uid().and_then(|uid| name(lookup_user_name_cached(uid)))
        {
            entry.set_user_name(user);
        }
        if self.preserve_gid
            && entry.group_name().is_none()
            && let Some(group) = entry.gid().and_then(|gid| name(lookup_group_name_cached(gid)))
        {
            entry.set_group_name(group);
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
            // `.` is sorted first, at NDX 0.
            second_round_ndx: 1,
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
            if parent.as_os_str().is_empty() || parent.as_os_str() == "." {
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
            if !sorted[s].is_dir() || is_dot(&sorted[s]) {
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
        type IdOf = fn(&FileEntry) -> Option<u32>;
        type Lookup = fn(u32) -> metadata::id_lookup::LookupResult<Vec<u8>>;
        let lists: [(bool, IdOf, Lookup); 2] = [
            (self.preserve_uid, FileEntry::uid, lookup_user_name_cached),
            (self.preserve_gid, FileEntry::gid, lookup_group_name_cached),
        ];
        for (preserve, id_of, lookup) in lists {
            if !preserve {
                continue;
            }
            let mut list = IdList::new();
            if id0_names {
                // upstream: uidlist.c:398-405 - CF_ID0_NAMES sends id 0's name.
                list.add_id(0, lookup(0).ok().flatten());
            }
            for id in sorted.iter().filter_map(id_of) {
                if !list.contains(id) {
                    list.add_id(id, lookup(id).ok().flatten());
                }
            }
            list.write(out, id0_names, protocol)
                .expect("counting never fails");
        }
    }
}

/// Reorders `items` so that position `i` holds what was at `order[i]`,
/// following each permutation cycle with swaps instead of building a second
/// copy of the list.
fn permute_in_place<T>(items: &mut [T], order: &[u32]) {
    let mut pending = order.to_vec();
    for start in 0..items.len() {
        let mut slot = start;
        loop {
            let source = pending[slot] as usize;
            pending[slot] = slot as u32;
            if source == start {
                break;
            }
            items.swap(slot, source);
            slot = source;
        }
    }
}

/// Whether `entry` is the transfer root `.`; compares bytes, not path
/// components.
fn is_dot(entry: &FileEntry) -> bool {
    entry.path().as_os_str() == "."
}

/// Wire size of the receiver's `created directory` notice: one `MSG_INFO`
/// frame carrying the destination with its trailing slashes removed.
///
/// upstream: main.c:812-829 - `*cp = '\0'` lops the trailing slash before
/// `rprintf(FINFO, "created directory %s\n", dest_path)`.
fn created_directory_notice_len(root: &Path) -> u64 {
    let bytes = root.as_os_str().as_encoded_bytes();
    let trimmed = match bytes.iter().rposition(|&b| !std::path::is_separator(char::from(b))) {
        Some(last) => &bytes[..=last],
        None => &bytes[..bytes.len().min(1)],
    };
    (protocol::MESSAGE_HEADER_LEN + "created directory ".len() + trimmed.len() + 1) as u64
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
