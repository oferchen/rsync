use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::local_copy::{
    LocalCopyAction, LocalCopyExecution, LocalCopyOptions, LocalCopyPlan, LocalCopyProgress,
    LocalCopyRecord, LocalCopyRecordHandler,
};

use super::ClientError;
use super::error::map_local_copy_error;
use super::summary::ClientEvent;

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
}

impl ClientProgressUpdate {
    /// Returns the event associated with this progress update.
    #[must_use]
    pub fn event(&self) -> &ClientEvent {
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
    #[must_use]
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
    #[must_use]
    pub const fn overall_total_bytes(&self) -> Option<u64> {
        self.overall_total_bytes
    }

    /// Returns the elapsed time since the transfer began.
    #[must_use]
    pub const fn overall_elapsed(&self) -> Duration {
        self.overall_elapsed
    }
}

/// Observer invoked for each progress update generated during client execution.
pub trait ClientProgressObserver {
    /// Handles a new progress update.
    fn on_progress(&mut self, update: &ClientProgressUpdate);
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
    total: usize,
    emitted: usize,
    overall_total_bytes: Option<u64>,
    overall_transferred: u64,
    overall_start: Instant,
    in_flight: HashMap<PathBuf, u64>,
    destination_root: Arc<PathBuf>,
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
            .execute_with_report(LocalCopyExecution::DryRun, options.clone())
            .map_err(map_local_copy_error)?;

        let destination_root = Arc::new(preview_report.destination_root().to_path_buf());
        let total = preview_report
            .records()
            .iter()
            .map(|record| ClientEvent::from_record(record, Arc::clone(&destination_root)))
            .filter(|event| event.kind().is_progress())
            .count();

        let summary = preview_report.summary();
        let total_bytes = summary.total_source_bytes();

        Ok(Self {
            observer,
            total,
            emitted: 0,
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
        let event = ClientEvent::from_record(&record, Arc::clone(&self.destination_root));
        if !event.kind().is_progress() {
            return;
        }

        self.emitted = self.emitted.saturating_add(1);
        let index = self.emitted;
        let remaining = self.total.saturating_sub(index);

        let total_bytes = if matches!(record.action(), LocalCopyAction::DataCopied) {
            record.total_bytes()
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
        };

        self.observer.on_progress(&update);
    }

    fn handle_progress(&mut self, progress: LocalCopyProgress<'_>) {
        if self.total == 0 {
            return;
        }

        let index = (self.emitted + 1).min(self.total);
        let remaining = self.total.saturating_sub(index);
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
            Arc::new(PathBuf::from("/dest")),
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
        // Just verify we can access the event
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
}
