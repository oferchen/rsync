//! Transfer candidates for the pipelined receive loop.
//!
//! upstream: generator.c:2803-2818 - generate_files() decides each file in
//! flist order while the receiver works through earlier ones, so the
//! generator never holds per-file decisions for the whole list. The batch
//! driver gets the same bound by deciding the list a chunk at a time, only
//! when the request window is about to run out of candidates.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::path::PathBuf;

use crate::receiver::directory::FailedDirectories;
use crate::receiver::stats::TransferStats;
use crate::receiver::{PipelineSetup, ReceiverContext};

/// A transfer candidate: flat file-list index, destination path, base iflags.
pub(in crate::receiver) type Candidate = (usize, PathBuf, u32);

/// File-list entries decided per chunk by [`CandidateFeed::Chunked`].
///
/// Bounds the candidate state (entry refs, destination paths and the parallel
/// stat batch) to one chunk while keeping each stat batch far above the
/// parallel-stat threshold (`DEFAULT_STAT_THRESHOLD`, 64). Measured at about
/// 365 B of candidate state per entry, a chunk holds about 3 MB where the
/// whole-list pass held 365 MB for 1M entries. upstream has no equivalent:
/// recv_generator() stats one file at a time (generator.c:1608).
pub(in crate::receiver) const CANDIDATE_CHUNK: usize = 8192;

/// Where the pipelined receive loop takes its transfer candidates from.
pub(in crate::receiver) enum CandidateFeed<'a> {
    /// Candidates decided before the loop: an INC_RECURSE sub-list segment or
    /// the phase-2 redo. Taken whole on the first pull.
    Ready(Vec<Candidate>),
    /// The file list, decided [`CANDIDATE_CHUNK`] entries at a time.
    Chunked(ChunkedFeed<'a>),
}

/// Cursor over the file list for [`CandidateFeed::Chunked`].
pub(in crate::receiver) struct ChunkedFeed<'a> {
    /// Flat index of the first entry not yet decided.
    pub(in crate::receiver) next: usize,
    /// One past the last flat index to decide.
    pub(in crate::receiver) end: usize,
    /// Directories that could not be made; the files below them are skipped.
    pub(in crate::receiver) failed_dirs: &'a FailedDirectories,
    /// Run stats the candidate pass counts its skips into.
    pub(in crate::receiver) stats: &'a mut TransferStats,
}

/// Merges candidates with the recorded no-transfer itemize rows.
///
/// upstream: generator.c:582-593 - itemize() writes NDX + iflags for an entry
/// with significant attribute changes but no ITEM_TRANSFER, interleaved with
/// the transfer requests in flist order; the peer's sender prints each row and
/// echoes the attrs back (sender.c:295-297). A row travels as a candidate with
/// an empty path and its iflags.
pub(in crate::receiver) struct CandidateQueue {
    /// Candidates and rows in request order, not yet requested.
    pending: VecDeque<Candidate>,
    /// Recorded rows not yet merged, in generator walk order.
    rows: VecDeque<(usize, u16)>,
    /// The feed has run dry and every row has been merged.
    done: bool,
}

impl CandidateQueue {
    pub(in crate::receiver) fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            rows: VecDeque::new(),
            done: false,
        }
    }

    /// Candidates queued and not yet requested.
    pub(in crate::receiver) fn len(&self) -> usize {
        self.pending.len()
    }

    /// Whether every candidate and row has been taken and the feed is dry.
    pub(in crate::receiver) fn is_finished(&self) -> bool {
        self.done && self.pending.is_empty()
    }

    /// Takes up to `n` queued candidates in request order.
    pub(in crate::receiver) fn take(&mut self, n: usize) -> impl Iterator<Item = Candidate> + '_ {
        let n = n.min(self.pending.len());
        self.pending.drain(..n)
    }
}

impl ReceiverContext {
    /// Pulls from `feed` until `queue` holds `want` candidates or the feed runs
    /// dry, merging the no-transfer itemize rows recorded so far in walk order.
    ///
    /// Rows past the last candidate stay queued until a later candidate passes
    /// them or the feed runs dry, so a row recorded for the whole list before
    /// the first chunk still goes out at its own place in the list.
    pub(in crate::receiver) fn fill_candidates<W>(
        &self,
        feed: &mut CandidateFeed<'_>,
        queue: &mut CandidateQueue,
        want: usize,
        writer: &mut W,
        setup: &PipelineSetup,
        metadata_errors: &mut Vec<(PathBuf, String)>,
    ) -> io::Result<()>
    where
        W: Write + crate::writer::MsgInfoSender + ?Sized,
    {
        while !queue.done && queue.pending.len() < want {
            let batch = self.next_candidates(feed, writer, setup, metadata_errors)?;
            self.absorb_no_transfer_rows(&mut queue.rows);
            let Some(batch) = batch else {
                queue.pending.extend(
                    queue
                        .rows
                        .drain(..)
                        .map(|(idx, iflags)| (idx, PathBuf::new(), u32::from(iflags))),
                );
                queue.done = true;
                break;
            };
            for item in batch {
                while let Some(&(idx, iflags)) = queue
                    .rows
                    .front()
                    .filter(|&&(idx, _)| self.itemize_walk_key(idx) < (item.0, true))
                {
                    queue.rows.pop_front();
                    queue
                        .pending
                        .push_back((idx, PathBuf::new(), u32::from(iflags)));
                }
                queue.pending.push_back(item);
            }
        }
        Ok(())
    }

    /// The next batch of candidates, or `None` once the feed is exhausted.
    fn next_candidates<W>(
        &self,
        feed: &mut CandidateFeed<'_>,
        writer: &mut W,
        setup: &PipelineSetup,
        metadata_errors: &mut Vec<(PathBuf, String)>,
    ) -> io::Result<Option<Vec<Candidate>>>
    where
        W: Write + crate::writer::MsgInfoSender + ?Sized,
    {
        match feed {
            CandidateFeed::Ready(list) if list.is_empty() => Ok(None),
            CandidateFeed::Ready(list) => Ok(Some(std::mem::take(list))),
            CandidateFeed::Chunked(chunk) if chunk.next >= chunk.end => Ok(None),
            CandidateFeed::Chunked(chunk) => {
                let start = chunk.next;
                chunk.next = chunk.end.min(start + CANDIDATE_CHUNK);
                self.build_files_to_transfer_in_range(
                    start..chunk.next,
                    writer,
                    &setup.dest_dir,
                    #[cfg(unix)]
                    setup.sandbox.as_deref(),
                    &setup.metadata_opts,
                    Some(chunk.failed_dirs),
                    metadata_errors,
                    chunk.stats,
                    setup.acl_cache.as_ref(),
                    setup.acl_id_map.as_deref(),
                )
                .map(Some)
            }
        }
    }

    /// Moves newly recorded no-transfer itemize rows into `rows`, keeping it in
    /// generator walk order. A released INC_RECURSE sub-list head sorts ahead
    /// of its sub-list (generator.c:2780-2787), not at its own place in the
    /// parent's list.
    fn absorb_no_transfer_rows(&self, rows: &mut VecDeque<(usize, u16)>) {
        let recorded = std::mem::take(&mut *self.server_no_transfer_itemize.borrow_mut());
        if recorded.is_empty() {
            return;
        }
        rows.extend(recorded);
        rows.make_contiguous()
            .sort_by_key(|&(idx, _)| self.itemize_walk_key(idx));
    }
}
