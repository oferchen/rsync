#![deny(unsafe_code)]
//! Deferred `--remove-source-files` bookkeeping for the sender.
//!
//! Upstream rsync never unlinks a source file inline right after sending it.
//! Instead the receiver/generator sends `MSG_SUCCESS(ndx)` back to the sender
//! only once the file has been fully received and committed to its final
//! destination, and the sender's `successful_send()` unlinks the source in
//! response (`io.c:1623-1637`, `sender.c:131-182`). This keeps
//! `--remove-source-files` crash-safe: an interrupted, failed, or redone
//! transfer never deletes a source that did not safely land at the destination.
//!
//! [`PendingSourceRemovals`] is the focused seam that records which file-list
//! entries the sender has transmitted but not yet had confirmed. Entries are
//! keyed by their flat file-list index, which is all that is needed to recover
//! the on-disk source path (`reconstruct_source_path`) and the recorded
//! identity guard (`FileEntry` size/mtime) at confirmation time - exactly what
//! upstream's `successful_send()` recomputes from `flist_for_ndx(ndx)`. Storing
//! only the index keeps the sender's resident set flat: the paths and identity
//! already live in the file list.
//!
//! # Upstream Reference
//!
//! - `sender.c:131-182` - `successful_send()` re-stats and unlinks on confirmation.
//! - `io.c:1096-1111` / `io.c:1623-1637` - `MSG_SUCCESS` wire round-trip.

use std::collections::HashSet;

/// Set of flat file-list indices whose `--remove-source-files` unlink has been
/// deferred until the peer confirms the commit via `MSG_SUCCESS`.
///
/// The sender inserts an index once it has finished transmitting that file
/// ([`mark_pending`](Self::mark_pending)) and removes it when the matching
/// `MSG_SUCCESS` arrives ([`confirm`](Self::confirm)). Any index still present
/// at the end of a transfer belongs to a file whose commit was never
/// confirmed, so its source is intentionally left in place.
#[derive(Debug, Default)]
pub(crate) struct PendingSourceRemovals {
    pending: HashSet<usize>,
}

impl PendingSourceRemovals {
    /// Records that entry `flat_ndx` has been transmitted and its source unlink
    /// is now pending the peer's `MSG_SUCCESS` confirmation.
    ///
    /// upstream: sender.c:480 marks `FLAG_FILE_SENT`; the unlink itself is
    /// deferred to `successful_send()` on `MSG_SUCCESS` receipt.
    pub(crate) fn mark_pending(&mut self, flat_ndx: usize) {
        self.pending.insert(flat_ndx);
    }

    /// Consumes a `MSG_SUCCESS` confirmation for `flat_ndx`.
    ///
    /// Returns `true` when the index was pending (so the caller must now unlink
    /// the source), and `false` when it was not - a confirmation the sender did
    /// not defer a removal for (e.g. an up-to-date file the peer's generator
    /// reported, or a duplicate), which is ignored just as upstream's
    /// `successful_send()` becomes a no-op guard when the file no longer
    /// matches.
    ///
    /// upstream: io.c:1623-1637 -> sender.c:131-182.
    pub(crate) fn confirm(&mut self, flat_ndx: usize) -> bool {
        self.pending.remove(&flat_ndx)
    }

    /// Returns `true` when no deferred removals remain outstanding.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Returns the number of deferred removals still awaiting confirmation.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::PendingSourceRemovals;

    /// A confirmation for a marked index reports the source must be unlinked,
    /// and clears the pending entry so a duplicate confirmation is a no-op.
    ///
    /// Encodes the upstream invariant that `successful_send()` runs exactly once
    /// per file, on `MSG_SUCCESS` receipt, not inline at send time.
    #[test]
    fn remove_source_confirm_marked_index_unlinks_once() {
        let mut pending = PendingSourceRemovals::default();
        pending.mark_pending(7);
        assert_eq!(pending.len(), 1);

        // First confirmation drives the unlink.
        assert!(pending.confirm(7));
        assert!(pending.is_empty());

        // A duplicate MSG_SUCCESS for the same ndx does nothing.
        assert!(!pending.confirm(7));
    }

    /// A confirmation for an index the sender never deferred is ignored - the
    /// sender must not unlink a file it did not transmit and mark pending.
    ///
    /// This is the crash-safety guard: an unexpected `MSG_SUCCESS` can never
    /// trigger a spurious source deletion.
    #[test]
    fn remove_source_confirm_unmarked_index_ignored() {
        let mut pending = PendingSourceRemovals::default();
        pending.mark_pending(1);

        assert!(!pending.confirm(2));
        // The genuinely pending entry is untouched.
        assert_eq!(pending.len(), 1);
        assert!(pending.confirm(1));
        assert!(pending.is_empty());
    }
}
