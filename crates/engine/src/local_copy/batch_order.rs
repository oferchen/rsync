//! Puts a streamed batch's records into NDX order as the walk produces them.
//!
//! A batch replays the stream a sender writes: each incremental file list,
//! then file-data records in ascending NDX, with sub-lists sent ahead of the
//! records as the sender's lookahead decides. A local walk produces records in
//! its own order and learns each list as it reads a directory, so a record can
//! arrive before a lower NDX is settled. [`RecordOrder`] holds a record only
//! until every entry below its NDX is finished, and hands everything else on
//! at once.
//!
//! # Upstream Reference
//!
//! - `sender.c:523-555` - sub-lists go out between file-data records
//! - `generator.c:2259-2283` - the generator walks each list in NDX order
//! - `generator.c:2780-2788` - a directory is itemized at the gap NDX below
//!   its own sub-list

use std::collections::{BTreeMap, HashMap};

use protocol::flist::SubListLookahead;

/// One piece of the batch stream, in the order it is written.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Emit {
    /// An encoded file list.
    List(Vec<u8>),
    /// A file-data or itemize record for `ndx`, without its NDX prefix.
    Record { ndx: i32, bytes: Vec<u8> },
}

/// Where an entry's record goes: its list in send order and its NDX.
#[derive(Clone, Copy, Debug)]
struct Place {
    list: usize,
    ndx: i32,
}

/// A released list's state while its records are emitted.
#[derive(Debug, Default)]
struct ListState {
    /// The encoded list, until the lookahead sends it.
    chunk: Option<Vec<u8>>,
    /// Unfinished entries whose records belong to this list, by NDX.
    open: BTreeMap<i32, usize>,
    /// Records waiting for a lower NDX to finish, by NDX.
    held: BTreeMap<i32, Vec<Vec<u8>>>,
}

/// One entry of a released list, as the caller places it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Slot {
    /// The caller's id for the entry.
    pub(crate) id: usize,
    /// The NDX the entry's records go out under.
    pub(crate) ndx: i32,
}

/// Orders streamed batch records by NDX.
#[derive(Debug)]
pub(crate) struct RecordOrder {
    lookahead: Option<SubListLookahead>,
    lists: Vec<ListState>,
    /// Sub-lists written so far (the initial list is written on release).
    sent: usize,
    /// The list whose records are being written.
    cursor: usize,
    place: HashMap<usize, Place>,
    /// Records of entries not placed yet.
    parked: HashMap<usize, Vec<Vec<u8>>>,
    /// Entries finished before they were placed.
    finished_early: Vec<usize>,
    closed: bool,
    /// Bytes of the records held or parked.
    held: usize,
}

impl RecordOrder {
    /// An empty order with no list released.
    pub(crate) fn new() -> Self {
        Self {
            lookahead: None,
            lists: Vec::new(),
            sent: 0,
            cursor: 0,
            place: HashMap::new(),
            parked: HashMap::new(),
            finished_early: Vec::new(),
            closed: false,
            held: 0,
        }
    }

    /// Releases the next list in send order: its encoded bytes, its entry
    /// count, and the entries whose records go out within it (every
    /// non-directory member, plus the owning directory, or `.` for the
    /// initial list, at the gap NDX below the list).
    pub(crate) fn release(
        &mut self,
        chunk: Vec<u8>,
        used: usize,
        slots: impl IntoIterator<Item = Slot>,
        out: &mut Vec<Emit>,
    ) {
        let list = self.lists.len();
        match self.lookahead.as_mut() {
            Some(lookahead) => lookahead.push_list(used),
            None => self.lookahead = Some(SubListLookahead::open(used)),
        }
        let mut state = ListState::default();
        if list == 0 {
            // upstream: flist.c:send_file_list() - the initial list goes out
            // before any record.
            out.push(Emit::List(chunk));
        } else {
            state.chunk = Some(chunk);
        }
        for slot in slots {
            self.place.insert(slot.id, Place { list, ndx: slot.ndx });
            state.open.insert(slot.ndx, slot.id);
            if let Some(records) = self.parked.remove(&slot.id) {
                state.held.entry(slot.ndx).or_default().extend(records);
            }
        }
        self.lists.push(state);
        let early = std::mem::take(&mut self.finished_early);
        for id in early {
            self.finish(id, out);
        }
        self.drain(out);
    }

    /// Accepts a record for entry `id`.
    pub(crate) fn record(&mut self, id: usize, bytes: Vec<u8>, out: &mut Vec<Emit>) {
        self.held += bytes.len();
        match self.place.get(&id) {
            Some(place) => {
                self.lists[place.list]
                    .held
                    .entry(place.ndx)
                    .or_default()
                    .push(bytes);
                self.drain(out);
            }
            None => self.parked.entry(id).or_default().push(bytes),
        }
    }

    /// Marks entry `id` finished: it produces no further record.
    pub(crate) fn finish(&mut self, id: usize, out: &mut Vec<Emit>) {
        match self.place.get(&id) {
            Some(place) => {
                self.lists[place.list].open.remove(&place.ndx);
                self.drain(out);
            }
            None => self.finished_early.push(id),
        }
    }

    /// Whether `id`'s record would be written now: every lower NDX is
    /// finished and the lookahead has its list out. Such a record can be
    /// written straight to the batch instead of being held.
    pub(crate) fn at_head(&self, id: usize) -> bool {
        let Some(place) = self.place.get(&id) else {
            return false;
        };
        place.list == self.cursor
            && self.lists[place.list].open.keys().next() == Some(&place.ndx)
            && self.lists[place.list].held.range(..place.ndx).next().is_none()
    }

    /// Starts writing `id`'s record straight to the batch: sends what the
    /// lookahead sends before it and returns `true` when `id` is at the head
    /// of the stream. The caller then writes the record itself and must not
    /// pass it to [`Self::record`].
    pub(crate) fn begin_direct(&mut self, id: usize, out: &mut Vec<Emit>) -> bool {
        if !self.at_head(id) {
            return false;
        }
        let list = self.place[&id].list;
        let Some(sent) = self.lookahead.as_mut().and_then(|l| l.try_serve(list)) else {
            return false;
        };
        while self.sent < sent {
            self.send_next(out);
        }
        true
    }

    /// The NDX `id`'s records go out under, once its list is released.
    pub(crate) fn ndx(&self, id: usize) -> Option<i32> {
        self.place.get(&id).map(|place| place.ndx)
    }

    /// Number of lists released so far.
    pub(crate) fn list_count(&self) -> usize {
        self.lists.len()
    }

    /// The bytes held for records that cannot be written yet.
    pub(crate) const fn held_bytes(&self) -> usize {
        self.held
    }

    /// Whether `id`'s record would wait for a list that is not planned yet,
    /// as rsync's sender would list more before sending it.
    ///
    /// upstream: sender.c:523-555
    pub(crate) fn lists_wanted(&self, id: usize) -> bool {
        let Some(lookahead) = self.lookahead.as_ref() else {
            return false;
        };
        let list = self.place.get(&id).map_or(self.lists.len(), |place| place.list);
        lookahead.waits_for_lists(list)
    }

    /// Notes that every list has been released, so the lookahead decides
    /// without waiting for more.
    pub(crate) fn lists_complete(&mut self, out: &mut Vec<Emit>) {
        if let Some(lookahead) = self.lookahead.as_mut() {
            lookahead.close();
        }
        self.drain(out);
    }

    /// Ends the walk: every list is released, so entries still open produce
    /// nothing more, and everything held is written.
    pub(crate) fn close(&mut self, out: &mut Vec<Emit>) {
        self.closed = true;
        if let Some(lookahead) = self.lookahead.as_mut() {
            lookahead.close();
        }
        for list in &mut self.lists {
            list.open.clear();
        }
        self.drain(out);
        // upstream: sender.c:533-553 - sub-lists the requests never reached
        // still go out after the last record.
        while self.sent + 1 < self.lists.len() {
            self.send_next(out);
        }
    }

    /// Whether nothing is held or parked.
    pub(crate) fn is_empty(&self) -> bool {
        self.parked.is_empty() && self.lists.iter().all(|list| list.held.is_empty())
    }

    fn send_next(&mut self, out: &mut Vec<Emit>) {
        self.sent += 1;
        if let Some(chunk) = self.lists[self.sent].chunk.take() {
            out.push(Emit::List(chunk));
        }
    }

    fn drain(&mut self, out: &mut Vec<Emit>) {
        while self.cursor < self.lists.len() {
            let list = self.cursor;
            let open_below = self.lists[list].open.keys().next().copied();
            loop {
                let Some((&ndx, _)) = self.lists[list].held.first_key_value() else {
                    break;
                };
                if open_below.is_some_and(|open| open < ndx) {
                    break;
                }
                let Some(lookahead) = self.lookahead.as_mut() else {
                    return;
                };
                let Some(sent) = lookahead.try_serve(list) else {
                    return;
                };
                while self.sent < sent {
                    self.send_next(out);
                }
                let (ndx, records) = self.lists[list]
                    .held
                    .pop_first()
                    .expect("a held record was just seen");
                for bytes in records {
                    self.held -= bytes.len();
                    out.push(Emit::Record { ndx, bytes });
                }
            }
            if !self.lists[list].open.is_empty() || !self.lists[list].held.is_empty() {
                return;
            }
            self.cursor += 1;
        }
    }
}

/// The writer-side state of a batch whose lists stream as the walk reads
/// each directory.
#[derive(Debug)]
pub(crate) struct BatchStream {
    /// Plans each list from the directory scans.
    pub(crate) planner: protocol::flist::SubListPlanner,
    /// Orders the records.
    pub(crate) order: RecordOrder,
    /// Top-level entries listed so far, until the initial list is complete.
    pub(crate) initial: Option<Vec<usize>>,
    /// Hardlink group (keyed by the traversal index of the member captured
    /// first) to the wire NDX of the member sent first.
    pub(crate) group_leader: HashMap<u32, i32>,
    /// The record of the file transferred last, until its iflags are final.
    pub(crate) pending: Option<(usize, Vec<u8>, Option<usize>)>,
    /// The record being written straight to the batch: its entry and the
    /// batch offset and value of its iflags word.
    pub(crate) direct: Option<(usize, Option<(u64, u16)>)>,
    /// Whether the last list, with `NDX_FLIST_EOF`, has been encoded.
    pub(crate) eof_written: bool,
    /// A write error raised where it could not be returned.
    pub(crate) error: Option<crate::local_copy::LocalCopyError>,
    /// The most record bytes held at once: ordered, pending and being built.
    pub(crate) peak_held: usize,
}

impl BatchStream {
    /// A stream with nothing listed.
    pub(crate) fn new() -> Self {
        Self {
            planner: protocol::flist::SubListPlanner::new(),
            order: RecordOrder::new(),
            initial: Some(Vec::new()),
            group_leader: HashMap::new(),
            pending: None,
            direct: None,
            eof_written: false,
            error: None,
            peak_held: 0,
        }
    }

    /// Notes what is held now, with `building` bytes of the record being
    /// built.
    pub(crate) fn note_held(&mut self, building: usize) {
        let pending = self.pending.as_ref().map_or(0, |(_, bytes, _)| bytes.len());
        self.peak_held = self
            .peak_held
            .max(self.order.held_bytes() + pending + building);
    }
}

#[cfg(test)]
thread_local! {
    static LAST_PEAK_HELD: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Records the peak of a stream that just closed on this thread.
#[cfg(test)]
pub(crate) fn record_peak_held(peak: usize) {
    LAST_PEAK_HELD.with(|cell| cell.set(peak));
}

/// The most record bytes the last streamed batch on this thread held.
#[cfg(test)]
pub(crate) fn last_peak_held_bytes() -> usize {
    LAST_PEAK_HELD.with(std::cell::Cell::get)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// A stream of lists: list `k` owns NDX `starts[k]..starts[k] + used[k]`
    /// and the gap NDX `starts[k] - 1` for its owning directory. Every NDX
    /// carries one record, keyed by the NDX itself as the entry id.
    fn layout(used: &[usize]) -> Vec<(i32, Vec<i32>)> {
        let mut start = 1i32;
        let mut prev = 0usize;
        used.iter()
            .enumerate()
            .map(|(k, &n)| {
                if k > 0 {
                    start += prev as i32 + 1;
                }
                prev = n;
                let mut ndxs: Vec<i32> = (start..start + n as i32).collect();
                if k > 0 {
                    ndxs.insert(0, start - 1);
                }
                (start, ndxs)
            })
            .collect()
    }

    fn records(out: &[Emit]) -> Vec<i32> {
        out.iter()
            .filter_map(|e| match e {
                Emit::Record { ndx, .. } => Some(*ndx),
                Emit::List(_) => None,
            })
            .collect()
    }

    /// Releases every list, then produces and finishes every entry in
    /// `order`, and returns the stream and the most bytes ever held.
    fn run(used: &[usize], order: &[i32]) -> (Vec<Emit>, usize, bool) {
        let lists = layout(used);
        let mut ro = RecordOrder::new();
        let mut out = Vec::new();
        for (k, (_, ndxs)) in lists.iter().enumerate() {
            let slots = ndxs.iter().map(|&n| Slot { id: n as usize, ndx: n });
            ro.release(vec![k as u8], used[k], slots, &mut out);
        }
        ro.lists_complete(&mut out);
        let mut peak = 0;
        for &n in order {
            ro.record(n as usize, vec![0; 8], &mut out);
            peak = peak.max(ro.held_bytes());
            ro.finish(n as usize, &mut out);
        }
        ro.close(&mut out);
        (out, peak, ro.is_empty())
    }

    fn sizes() -> impl Strategy<Value = Vec<usize>> {
        prop::collection::vec(0usize..6, 1..8)
    }

    proptest! {
        /// Records produced in any order go out in ascending NDX, each once,
        /// and nothing stays held after the walk ends.
        #[test]
        fn any_arrival_order_emits_ndx_order(used in sizes(), seed in any::<u64>()) {
            let all: Vec<i32> = layout(&used).into_iter().flat_map(|(_, n)| n).collect();
            let mut order = all.clone();
            let mut s = seed;
            for i in (1..order.len()).rev() {
                s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                order.swap(i, (s >> 33) as usize % (i + 1));
            }
            let (out, _, empty) = run(&used, &order);
            let mut sorted = all;
            sorted.sort_unstable();
            prop_assert_eq!(records(&out), sorted);
            prop_assert!(empty, "a record was left held at the end of the run");
        }

        /// Records produced in NDX order are never held.
        #[test]
        fn ndx_order_holds_nothing(used in sizes()) {
            let all: Vec<i32> = layout(&used).into_iter().flat_map(|(_, n)| n).collect();
            let (_, peak, _) = run(&used, &all);
            prop_assert_eq!(peak, 0);
        }

        /// Every list goes out exactly once, in send order, and before any
        /// record of its own.
        #[test]
        fn lists_precede_their_records(used in sizes(), seed in any::<u64>()) {
            let lists = layout(&used);
            let all: Vec<i32> = lists.iter().flat_map(|(_, n)| n.clone()).collect();
            let mut order = all;
            let mut s = seed;
            for i in (1..order.len()).rev() {
                s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                order.swap(i, (s >> 33) as usize % (i + 1));
            }
            let (out, _, _) = run(&used, &order);
            let mut seen = Vec::new();
            for e in &out {
                match e {
                    Emit::List(chunk) => seen.push(chunk[0] as usize),
                    Emit::Record { ndx, .. } => {
                        let list = lists.iter().position(|(_, n)| n.contains(ndx)).unwrap();
                        prop_assert!(seen.contains(&list), "record {} before its list", ndx);
                    }
                }
            }
            prop_assert_eq!(seen, (0..used.len()).collect::<Vec<_>>());
        }
    }

    /// A record for an entry whose list is not released yet waits for it and
    /// then goes out in place.
    #[test]
    fn a_record_before_its_list_waits() {
        let mut ro = RecordOrder::new();
        let mut out = Vec::new();
        ro.release(vec![0], 1, [Slot { id: 1, ndx: 1 }], &mut out);
        ro.record(9, vec![9], &mut out);
        ro.finish(9, &mut out);
        ro.record(1, vec![1], &mut out);
        ro.finish(1, &mut out);
        ro.release(vec![1], 0, [Slot { id: 9, ndx: 2 }], &mut out);
        ro.close(&mut out);
        assert_eq!(records(&out), vec![1, 2]);
        assert!(ro.is_empty());
    }

    /// A record written straight through takes its place in the stream: the
    /// records behind it follow it, and nothing before it is skipped.
    #[test]
    fn a_direct_record_keeps_its_place() {
        let mut ro = RecordOrder::new();
        let mut out = Vec::new();
        ro.release(
            vec![0],
            3,
            [
                Slot { id: 1, ndx: 1 },
                Slot { id: 2, ndx: 2 },
                Slot { id: 3, ndx: 3 },
            ],
            &mut out,
        );
        ro.lists_complete(&mut out);
        ro.record(3, vec![3], &mut out);
        ro.record(1, vec![1], &mut out);
        assert!(!ro.begin_direct(2, &mut out), "1 is still open below 2");
        ro.finish(1, &mut out);
        assert!(ro.begin_direct(2, &mut out));
        out.push(Emit::Record { ndx: 2, bytes: vec![2] });
        ro.finish(2, &mut out);
        ro.finish(3, &mut out);
        ro.close(&mut out);
        assert_eq!(records(&out), vec![1, 2, 3]);
    }

    /// The head entry can be written straight through; one behind it cannot.
    #[test]
    fn only_the_lowest_open_entry_is_at_head() {
        let mut ro = RecordOrder::new();
        let mut out = Vec::new();
        ro.release(
            vec![0],
            2,
            [Slot { id: 1, ndx: 1 }, Slot { id: 2, ndx: 2 }],
            &mut out,
        );
        assert!(ro.at_head(1));
        assert!(!ro.at_head(2));
        ro.finish(1, &mut out);
        assert!(ro.at_head(2));
    }
}
