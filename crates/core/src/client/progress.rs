//! Progress tracking for client transfers.
//!
//! This module provides the infrastructure for reporting incremental progress
//! updates during file transfers, mirroring the behavior of upstream rsync's
//! `--info=progress2` flag. The [`ClientProgressObserver`] trait allows custom
//! progress handlers to be notified as files are copied, enabling progress bars
//! and status displays in user interfaces.
//!
//! # Upstream Reference
//!
//! - `progress.c` - Progress display logic
//!
//! # Examples
//!
//! ```ignore
//! use core::client::{ClientConfig, ClientProgressUpdate, run_client_with_observer};
//!
//! let mut total_transferred = 0u64;
//! let mut observer = |update: &ClientProgressUpdate| {
//!     total_transferred = update.overall_transferred();
//!     println!("Progress: {}/{} files", update.index(), update.total());
//! };
//!
//! let config = ClientConfig::builder()
//!     .transfer_args(["source", "dest"])
//!     .build();
//!
//! run_client_with_observer(config, Some(&mut observer))?;
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::local_copy::{
    LocalCopyExecution, LocalCopyOptions, LocalCopyPlan, LocalCopyProgress, LocalCopyRecord,
    LocalCopyRecordHandler,
};

use super::ClientError;
use super::error::map_local_copy_error;
use super::summary::{ClientEvent, ClientEventKind};

/// Progress update emitted while executing a client transfer.
#[derive(Clone, Debug)]
pub struct ClientProgressUpdate {
    event: ClientEvent,
    total: usize,
    remaining: usize,
    index: usize,
    total_bytes: Option<u64>,
    final_update: bool,
    overall_transferred: u64,
    overall_total_bytes: Option<u64>,
    overall_elapsed: Duration,
    flist_eof: bool,
}

impl ClientProgressUpdate {
    /// Returns the event associated with this progress update.
    #[must_use]
    pub const fn event(&self) -> &ClientEvent {
        &self.event
    }

    /// Returns the number of remaining progress events after this update.
    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.remaining
    }

    /// Returns the total number of progress events in the transfer.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.total
    }

    /// Returns the 1-based index of the completed progress event.
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    /// Returns the total number of bytes expected for this transfer step, when known.
    pub const fn total_bytes(&self) -> Option<u64> {
        self.total_bytes
    }

    /// Reports whether this update corresponds to the completion of an action.
    #[must_use]
    pub const fn is_final(&self) -> bool {
        self.final_update
    }

    /// Returns the aggregate number of bytes transferred across the entire transfer.
    #[must_use]
    pub const fn overall_transferred(&self) -> u64 {
        self.overall_transferred
    }

    /// Returns the total number of bytes expected for the entire transfer, when known.
    pub const fn overall_total_bytes(&self) -> Option<u64> {
        self.overall_total_bytes
    }

    /// Returns the elapsed time since the transfer began.
    #[must_use]
    pub const fn overall_elapsed(&self) -> Duration {
        self.overall_elapsed
    }

    /// Reports whether the file list is complete (no more INC_RECURSE
    /// sub-lists pending).
    ///
    /// Mirrors upstream's `flist_eof` flag, which controls whether the
    /// per-file progress line ends with `to-chk=...` (complete) or
    /// `ir-chk=...` (incremental-recursive still in flight).
    ///
    /// upstream: progress.c:79-82 rprint_progress
    #[must_use]
    pub const fn flist_eof(&self) -> bool {
        self.flist_eof
    }
}

/// Observer invoked for each progress update generated during client execution.
pub trait ClientProgressObserver {
    /// Handles a new progress update.
    fn on_progress(&mut self, update: &ClientProgressUpdate);
}

impl ClientProgressUpdate {
    /// Creates a progress update from a server-side transfer event.
    ///
    /// Used by the SSH/daemon transfer adapter to convert per-file progress
    /// events from the transfer crate into client progress updates.
    pub fn from_transfer_event(
        event: ClientEvent,
        files_done: usize,
        total_files: usize,
        total_bytes: Option<u64>,
        overall_transferred: u64,
        overall_elapsed: Duration,
        flist_eof: bool,
    ) -> Self {
        Self {
            event,
            total: total_files,
            remaining: total_files.saturating_sub(files_done),
            index: files_done,
            total_bytes,
            final_update: true,
            overall_transferred,
            overall_total_bytes: None,
            overall_elapsed,
            flist_eof,
        }
    }
}

impl ClientProgressUpdate {
    /// Creates a mid-transfer (non-final) progress update for testing.
    ///
    /// Identical to [`from_transfer_event`](Self::from_transfer_event) except
    /// `is_final()` returns `false`, simulating an in-flight progress tick
    /// before the file transfer completes.
    #[doc(hidden)]
    pub fn from_transfer_event_mid(
        event: ClientEvent,
        files_done: usize,
        total_files: usize,
        total_bytes: Option<u64>,
        overall_transferred: u64,
        overall_total_bytes: Option<u64>,
        overall_elapsed: Duration,
        flist_eof: bool,
    ) -> Self {
        Self {
            event,
            total: total_files,
            remaining: total_files.saturating_sub(files_done),
            index: files_done,
            total_bytes,
            final_update: false,
            overall_transferred,
            overall_total_bytes,
            overall_elapsed,
            flist_eof,
        }
    }
}

impl<F> ClientProgressObserver for F
where
    F: FnMut(&ClientProgressUpdate),
{
    fn on_progress(&mut self, update: &ClientProgressUpdate) {
        self(update);
    }
}

pub(crate) struct ClientProgressForwarder<'a> {
    observer: &'a mut dyn ClientProgressObserver,
    // Total file-list entries (the `to-chk=<remaining>/<total>` denominator),
    // including up-to-date ones the generator checks but never transfers.
    total: usize,
    // Number of entries that will actually be transferred. The `to-chk`
    // remaining counts down from this, so the last transferred entry reaches 0
    // even when up-to-date entries (e.g. an unchanged parent dir the local
    // executor visits last) trail it in processing order.
    transferred_total: usize,
    // Running count of entries transferred. Drives `xfr#` and the remaining
    // figure (transferred_total - transferred); only advances for real
    // transfers, never for up-to-date matches.
    transferred: usize,
    overall_total_bytes: Option<u64>,
    overall_transferred: u64,
    overall_start: Instant,
    in_flight: HashMap<PathBuf, u64>,
    destination_root: Arc<Path>,
}

impl<'a> ClientProgressForwarder<'a> {
    pub(crate) fn new(
        observer: &'a mut dyn ClientProgressObserver,
        plan: &LocalCopyPlan,
        mut options: LocalCopyOptions,
    ) -> Result<Self, ClientError> {
        if !options.events_enabled() {
            options = options.collect_events(true);
        }

        let preview_report = plan
            .execute_with_report(LocalCopyExecution::DryRun, options)
            .map_err(map_local_copy_error)?;

        let (summary, records, destination_root) = preview_report.into_parts();
        let destination_root: Arc<Path> = Arc::from(destination_root);
        let progress_events: Vec<_> = records
            .into_iter()
            .map(|record| ClientEvent::from_record_owned(record, Arc::clone(&destination_root)))
            .filter(|event| event.kind().is_progress())
            .collect();
        // Denominator counts every checked entry; the numerator base counts only
        // those that will transfer, so to-chk reaches 0 on the last transfer.
        let total = progress_events.len();
        let transferred_total = progress_events
            .iter()
            .filter(|event| !event.is_uptodate())
            .count();

        let total_bytes = summary.total_source_bytes();

        Ok(Self {
            observer,
            total,
            transferred_total,
            transferred: 0,
            overall_total_bytes: (total_bytes > 0).then_some(total_bytes),
            overall_transferred: 0,
            overall_start: Instant::now(),
            in_flight: HashMap::new(),
            destination_root,
        })
    }

    pub(crate) fn as_handler_mut(&mut self) -> &mut dyn LocalCopyRecordHandler {
        self
    }
}

impl<'a> LocalCopyRecordHandler for ClientProgressForwarder<'a> {
    fn handle(&mut self, record: LocalCopyRecord) {
        let event = ClientEvent::from_record_owned(record, Arc::clone(&self.destination_root));
        if !event.kind().is_progress() {
            return;
        }

        // upstream: an up-to-date entry prints no per-file progress block and
        // does not advance `xfr#` - it is silent under `--progress`/`-P` (it
        // surfaces only with `-vv`/`-i`), so a no-change run emits nothing.
        if event.is_uptodate() {
            return;
        }

        self.transferred = self.transferred.saturating_add(1);
        let index = self.transferred;
        let remaining = self.transferred_total.saturating_sub(self.transferred);

        let total_bytes = if matches!(event.kind(), ClientEventKind::DataCopied) {
            event.total_bytes()
        } else {
            None
        };

        let path = event.relative_path().to_path_buf();
        let previous = self.in_flight.remove(&path).unwrap_or_default();
        let additional = event.bytes_transferred().saturating_sub(previous);
        if additional > 0 {
            self.overall_transferred = self.overall_transferred.saturating_add(additional);
        }

        let update = ClientProgressUpdate {
            event,
            total: self.total,
            remaining,
            index,
            total_bytes,
            final_update: true,
            overall_transferred: self.overall_transferred,
            overall_total_bytes: self.overall_total_bytes,
            overall_elapsed: self.overall_start.elapsed(),
            // Local copies enumerate the file list eagerly before transferring,
            // so the list is always complete when progress is emitted.
            flist_eof: true,
        };

        self.observer.on_progress(&update);
    }

    fn handle_progress(&mut self, progress: LocalCopyProgress<'_>) {
        if self.total == 0 {
            return;
        }

        // The in-flight file is the next transfer (`xfr#`); to-chk counts the
        // transfers still pending after it.
        let index = (self.transferred + 1).min(self.transferred_total);
        let remaining = self.transferred_total.saturating_sub(self.transferred + 1);
        let event = ClientEvent::from_progress(
            progress.relative_path(),
            progress.bytes_transferred(),
            progress.total_bytes(),
            progress.elapsed(),
            Arc::clone(&self.destination_root),
        );

        let entry = self
            .in_flight
            .entry(progress.relative_path().to_path_buf())
            .or_insert(0);
        let additional = progress.bytes_transferred().saturating_sub(*entry);
        if additional > 0 {
            self.overall_transferred = self.overall_transferred.saturating_add(additional);
            *entry = (*entry).saturating_add(additional);
        }

        let update = ClientProgressUpdate {
            event,
            total: self.total,
            remaining,
            index,
            total_bytes: progress.total_bytes(),
            final_update: false,
            overall_transferred: self.overall_transferred,
            overall_total_bytes: self.overall_total_bytes,
            overall_elapsed: self.overall_start.elapsed(),
            // Local copies do not use INC_RECURSE: the file list is enumerated
            // eagerly before any progress event is emitted.
            flist_eof: true,
        };

        self.observer.on_progress(&update);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn create_test_update(
        total: usize,
        remaining: usize,
        index: usize,
        final_update: bool,
    ) -> ClientProgressUpdate {
        let event = ClientEvent::from_progress(
            Path::new("test.txt"),
            1024,
            Some(2048),
            Duration::from_secs(1),
            Arc::from(Path::new("/dest")),
        );
        ClientProgressUpdate {
            event,
            total,
            remaining,
            index,
            total_bytes: Some(2048),
            final_update,
            overall_transferred: 5000,
            overall_total_bytes: Some(10000),
            overall_elapsed: Duration::from_secs(5),
            flist_eof: true,
        }
    }

    #[test]
    fn remaining_returns_count() {
        let update = create_test_update(10, 5, 5, false);
        assert_eq!(update.remaining(), 5);
    }

    #[test]
    fn total_returns_count() {
        let update = create_test_update(10, 5, 5, false);
        assert_eq!(update.total(), 10);
    }

    #[test]
    fn index_returns_position() {
        let update = create_test_update(10, 5, 5, false);
        assert_eq!(update.index(), 5);
    }

    #[test]
    fn total_bytes_returns_value() {
        let update = create_test_update(10, 5, 5, false);
        assert_eq!(update.total_bytes(), Some(2048));
    }

    #[test]
    fn is_final_returns_true_when_final() {
        let update = create_test_update(10, 0, 10, true);
        assert!(update.is_final());
    }

    #[test]
    fn is_final_returns_false_when_not_final() {
        let update = create_test_update(10, 5, 5, false);
        assert!(!update.is_final());
    }

    #[test]
    fn overall_transferred_returns_value() {
        let update = create_test_update(10, 5, 5, false);
        assert_eq!(update.overall_transferred(), 5000);
    }

    #[test]
    fn overall_total_bytes_returns_value() {
        let update = create_test_update(10, 5, 5, false);
        assert_eq!(update.overall_total_bytes(), Some(10000));
    }

    #[test]
    fn overall_elapsed_returns_duration() {
        let update = create_test_update(10, 5, 5, false);
        assert_eq!(update.overall_elapsed(), Duration::from_secs(5));
    }

    #[test]
    fn event_returns_reference() {
        let update = create_test_update(10, 5, 5, false);
        let _event = update.event();
    }

    #[test]
    fn closure_implements_observer() {
        let mut updates = Vec::new();
        let mut observer = |update: &ClientProgressUpdate| {
            updates.push(update.index());
        };

        let update = create_test_update(10, 5, 5, false);
        observer.on_progress(&update);

        assert_eq!(updates, vec![5]);
    }

    /// upstream: progress.c:79-82 - `flist_eof ? "to" : "ir"` switches the
    /// chk-prefix on the trailing per-file summary. Verify the flag is
    /// surfaced through the public accessor.
    #[test]
    fn flist_eof_default_true_matches_completed_list() {
        let update = create_test_update(10, 5, 5, true);
        assert!(update.flist_eof());
    }

    #[test]
    fn flist_eof_can_be_false_for_inc_recurse() {
        let event = ClientEvent::from_progress(
            Path::new("a"),
            0,
            None,
            Duration::ZERO,
            Arc::from(Path::new("/d")),
        );
        let update =
            ClientProgressUpdate::from_transfer_event(event, 1, 2, None, 0, Duration::ZERO, false);
        assert!(!update.flist_eof());
    }
}
