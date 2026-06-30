//! Progress observer adaptation for SSH transfers.
//!
//! Bridges server-side per-file progress events to the client-side progress
//! observer used for live progress display during SSH and daemon transfers.

use std::time::Instant;

use super::super::super::progress::ClientProgressObserver;
use crate::server::{TransferProgressCallback, TransferProgressEvent};

/// Adapts a [`ClientProgressObserver`] to [`TransferProgressCallback`].
///
/// Converts server-side per-file progress events into client-side progress
/// updates, enabling live progress display during SSH and daemon transfers.
pub(super) struct ServerProgressAdapter<'a> {
    observer: &'a mut dyn ClientProgressObserver,
    start: Instant,
    overall_transferred: u64,
}

impl<'a> ServerProgressAdapter<'a> {
    pub(super) fn new(observer: &'a mut dyn ClientProgressObserver, start: Instant) -> Self {
        Self {
            observer,
            start,
            overall_transferred: 0,
        }
    }
}

impl TransferProgressCallback for ServerProgressAdapter<'_> {
    fn on_file_transferred(&mut self, event: &TransferProgressEvent<'_>) {
        use std::path::Path;
        use std::sync::Arc;

        self.overall_transferred += event.file_bytes;

        let client_event = super::super::super::summary::ClientEvent::from_progress(
            event.path,
            event.file_bytes,
            event.total_file_bytes,
            self.start.elapsed(),
            Arc::from(Path::new("")),
        );

        let update = super::super::super::progress::ClientProgressUpdate::from_transfer_event(
            client_event,
            event.files_done,
            event.total_files,
            event.total_file_bytes,
            self.overall_transferred,
            self.start.elapsed(),
            event.flist_eof,
        );

        self.observer.on_progress(&update);
    }
}
